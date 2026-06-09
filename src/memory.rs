//! Representasi satu memori dan operasi baca/tulis ke file Markdown.
//!
//! Setiap memori = satu file `.md` dengan frontmatter YAML di atas dan body
//! Markdown di bawah. Body boleh berisi `[[wikilink]]` ke memori lain.

use crate::config::{ensure_dir, Config};
use crate::embed::SemanticHit;
use crate::project::slugify;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Frontmatter YAML sebuah memori.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frontmatter {
    /// Slug unik dalam satu project (sekaligus nama file).
    pub name: String,
    /// Ringkasan satu baris — dipakai saat search/list & di peta.
    pub description: String,
    /// Tag bebas untuk pengelompokan di peta & Obsidian.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Kategori memori, mis. `project`, `reference`, `decision`, `note`.
    #[serde(default = "default_type", rename = "type")]
    pub kind: String,
    /// Slug memori lain yang terkait (dirender sebagai wikilink di peta).
    #[serde(default)]
    pub links: Vec<String>,
    /// Timestamp RFC3339 saat dibuat.
    pub created: String,
    /// Timestamp RFC3339 saat terakhir diubah.
    pub updated: String,
}

fn default_type() -> String {
    "note".to_string()
}

/// Memori lengkap = frontmatter + body Markdown.
#[derive(Debug, Clone)]
pub struct Memory {
    pub front: Frontmatter,
    pub body: String,
}

impl Memory {
    /// Render ke teks file lengkap (frontmatter + body).
    pub fn to_file_string(&self) -> anyhow::Result<String> {
        let yaml = serde_yaml::to_string(&self.front)?;
        // `serde_yaml::to_string` tidak menambahkan delimiter `---`.
        Ok(format!("---\n{yaml}---\n\n{}\n", self.body.trim_end()))
    }

    /// Parse dari teks file lengkap.
    pub fn from_file_string(raw: &str) -> anyhow::Result<Self> {
        let (front_yaml, body) = split_frontmatter(raw)
            .ok_or_else(|| anyhow::anyhow!("file memori tidak punya frontmatter YAML"))?;
        let front: Frontmatter = serde_yaml::from_str(front_yaml)?;
        Ok(Self {
            front,
            body: body.trim().to_string(),
        })
    }
}

/// Pisahkan blok frontmatter (`---\n...\n---`) dari body.
/// Mengembalikan `(yaml_tanpa_delimiter, body)`.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw); // buang BOM bila ada
    let rest = raw.strip_prefix("---")?;
    // delimiter pembuka harus diikuti newline
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    // cari delimiter penutup `\n---`
    let end = rest.find("\n---")?;
    let yaml = &rest[..end];
    let after = &rest[end + 4..]; // lewati "\n---"
                                  // body dimulai setelah newline berikutnya
    let body = after
        .strip_prefix('\n')
        .or_else(|| after.strip_prefix("\r\n"))
        .unwrap_or(after);
    Some((yaml, body))
}

/// Hasil operasi tulis memori.
pub struct WriteOutcome {
    pub slug: String,
    pub path: std::path::PathBuf,
    pub created: bool,
}

/// Argumen untuk membuat / memperbarui memori.
pub struct WriteInput {
    pub name: String,
    pub description: String,
    pub body: String,
    pub tags: Vec<String>,
    pub kind: Option<String>,
    pub links: Vec<String>,
}

/// Tulis (buat atau update) sebuah memori. Saat update, field `created`
/// dipertahankan dari file lama; `updated` selalu di-refresh.
pub fn write_memory(
    config: &Config,
    project: &str,
    input: WriteInput,
) -> anyhow::Result<WriteOutcome> {
    let slug = slugify(&input.name);
    anyhow::ensure!(
        !slug.is_empty(),
        "nama memori tidak valid setelah disanitasi"
    );

    let dir = config.project_dir(project);
    ensure_dir(&dir)?;
    let path = config.memory_file(project, &slug);

    let now = now_rfc3339();
    let existing = read_memory(config, project, &slug).ok();
    let created = existing.is_none();
    let created_ts = existing
        .as_ref()
        .map(|m| m.front.created.clone())
        .unwrap_or_else(|| now.clone());

    let front = Frontmatter {
        name: slug.clone(),
        description: input.description,
        tags: normalize_list(input.tags),
        kind: input.kind.unwrap_or_else(default_type),
        links: input
            .links
            .into_iter()
            .map(|l| slugify(&l))
            .filter(|s| !s.is_empty())
            .collect(),
        created: created_ts,
        updated: now,
    };

    let mem = Memory {
        front,
        body: input.body,
    };
    std::fs::write(&path, mem.to_file_string()?)?;

    Ok(WriteOutcome {
        slug,
        path,
        created,
    })
}

/// Hasil operasi rename.
pub struct RenameOutcome {
    pub old_slug: String,
    pub new_slug: String,
    /// Slug memori lain yang tautannya ikut diperbarui.
    pub updated_referrers: Vec<String>,
}

/// Ganti nama (slug) sebuah memori, lalu perbarui SEMUA tautan masuk
/// (field `links` & `[[wikilink]]` di body) milik memori lain agar tetap
/// resolve. Timestamp `created` memori dipertahankan; `updated` di-refresh
/// untuk memori yang berubah. Tidak menyentuh `_MOC.md` (pemanggil regen).
pub fn rename_memory(
    config: &Config,
    project: &str,
    old_name: &str,
    new_name: &str,
) -> anyhow::Result<RenameOutcome> {
    let old_slug = slugify(old_name);
    let new_slug = slugify(new_name);
    anyhow::ensure!(
        !new_slug.is_empty(),
        "nama baru tidak valid setelah disanitasi"
    );
    anyhow::ensure!(
        old_slug != new_slug,
        "nama lama & baru menghasilkan slug yang sama ('{new_slug}')"
    );

    let old = read_memory(config, project, &old_slug).map_err(|_| {
        anyhow::anyhow!("memori '{old_slug}' tidak ditemukan di project '{project}'")
    })?;
    let new_path = config.memory_file(project, &new_slug);
    anyhow::ensure!(
        !new_path.exists(),
        "target '{new_slug}' sudah ada — pilih nama lain"
    );

    let now = now_rfc3339();

    // 1. Tulis file baru (pertahankan `created`), 2. hapus file lama.
    let mut front = old.front.clone();
    front.name = new_slug.clone();
    front.updated = now.clone();
    let new_mem = Memory {
        front,
        body: old.body.clone(),
    };
    std::fs::write(&new_path, new_mem.to_file_string()?)?;
    std::fs::remove_file(config.memory_file(project, &old_slug))?;

    // 3. Perbarui semua perujuk (field links + wikilink body).
    let mut updated_referrers = Vec::new();
    for m in load_all(config, project) {
        let slug = slugify(&m.front.name);
        if slug == new_slug {
            continue; // memori hasil rename sendiri
        }
        let mut changed = false;

        let mut links = m.front.links.clone();
        for l in links.iter_mut() {
            if slugify(l) == old_slug {
                *l = new_slug.clone();
                changed = true;
            }
        }
        // dedup pasca-penggantian (jaga urutan kemunculan pertama).
        let mut seen = std::collections::BTreeSet::new();
        links.retain(|l| seen.insert(slugify(l)));

        let new_body = crate::links::rewrite_wikilink_target(&m.body, &old_slug, &new_slug);
        if new_body != m.body {
            changed = true;
        }

        if changed {
            let mut f = m.front.clone();
            f.links = links;
            f.updated = now.clone();
            let updated = Memory {
                front: f,
                body: new_body,
            };
            std::fs::write(
                config.memory_file(project, &slug),
                updated.to_file_string()?,
            )?;
            updated_referrers.push(slug);
        }
    }
    updated_referrers.sort();

    Ok(RenameOutcome {
        old_slug,
        new_slug,
        updated_referrers,
    })
}

/// Baca satu memori berdasarkan slug.
pub fn read_memory(config: &Config, project: &str, name: &str) -> anyhow::Result<Memory> {
    let slug = slugify(name);
    let path = config.memory_file(project, &slug);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("gagal membaca '{}': {e}", path.display()))?;
    Memory::from_file_string(&raw)
}

/// Hapus satu memori. Mengembalikan `true` bila file memang ada & terhapus.
pub fn delete_memory(config: &Config, project: &str, name: &str) -> anyhow::Result<bool> {
    let slug = slugify(name);
    let path = config.memory_file(project, &slug);
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)?;
    Ok(true)
}

/// Item ringkas untuk list/search.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryEntry {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub updated: String,
}

impl From<&Frontmatter> for MemoryEntry {
    fn from(f: &Frontmatter) -> Self {
        Self {
            name: f.name.clone(),
            description: f.description.clone(),
            tags: f.tags.clone(),
            kind: f.kind.clone(),
            updated: f.updated.clone(),
        }
    }
}

/// Muat semua memori dalam satu project (mengabaikan file `_MOC.md`).
pub fn load_all(config: &Config, project: &str) -> Vec<Memory> {
    let dir = config.project_dir(project);
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_memory_file(&path) {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(mem) = Memory::from_file_string(&raw) {
                out.push(mem);
            }
        }
    }
    out.sort_by(|a, b| a.front.name.cmp(&b.front.name));
    out
}

/// Daftar ringkas semua memori dalam satu project.
pub fn list_entries(config: &Config, project: &str) -> Vec<MemoryEntry> {
    load_all(config, project)
        .iter()
        .map(|m| MemoryEntry::from(&m.front))
        .collect()
}

/// Hasil satu kecocokan pencarian.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub score: u32,
    pub snippet: String,
}

/// Cari memori berdasarkan query teks (opsional) dan/atau tag (opsional).
/// Skor sederhana: cocok di nama/description bernilai lebih tinggi dari body.
pub fn search(
    config: &Config,
    project: &str,
    query: Option<&str>,
    tag: Option<&str>,
) -> Vec<SearchHit> {
    // Pecah query jadi term per-whitespace. Mencocokkan SETIAP term secara
    // terpisah (bukan frasa utuh) agar query multi-kata seperti
    // "io_lock konkurensi" tetap cocok walau kata-katanya tak berurutan.
    let q_terms: Option<Vec<String>> = query.map(|s| {
        s.to_lowercase()
            .split_whitespace()
            .map(|t| t.to_string())
            .collect()
    });
    let tag = tag.map(slugify);
    let mut hits = Vec::new();

    for mem in load_all(config, project) {
        let f = &mem.front;

        if let Some(t) = &tag {
            let has = f.tags.iter().any(|x| &slugify(x) == t);
            if !has {
                continue;
            }
        }

        let mut score = 0u32;
        let mut snippet = f.description.clone();

        match &q_terms {
            // Query kosong/whitespace dianggap "tanpa query" → semua lolos.
            Some(terms) if !terms.is_empty() => {
                let name_lower = f.name.to_lowercase();
                let desc_lower = f.description.to_lowercase();
                let body_lower = mem.body.to_lowercase();
                let mut snippet_set = false;
                // Skor dijumlahkan lintas-term: dok yang cocok lebih banyak
                // term naik peringkatnya. Cukup satu term cocok untuk lolos.
                for term in terms {
                    if name_lower.contains(term) {
                        score += 5;
                    }
                    if desc_lower.contains(term) {
                        score += 3;
                    }
                    if f.tags.iter().any(|x| x.to_lowercase().contains(term)) {
                        score += 2;
                    }
                    if let Some(pos) = body_lower.find(term) {
                        score += 1;
                        if !snippet_set {
                            snippet = make_snippet(&mem.body, pos, term.len());
                            snippet_set = true;
                        }
                    }
                }
                if score == 0 {
                    continue; // ada query tapi tidak ada term yang cocok
                }
            }
            _ => {
                // tanpa query (mungkin hanya filter tag): semua lolos
                score = 1;
            }
        }

        hits.push(SearchHit {
            name: f.name.clone(),
            description: f.description.clone(),
            tags: f.tags.clone(),
            score,
            snippet,
        });
    }

    hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    hits
}

/// Satu hasil pencarian hybrid (gabungan keyword + semantik).
#[derive(Debug, Clone, Serialize)]
pub struct HybridHit {
    pub name: String,
    pub description: String,
    /// Skor gabungan 0.0–1.0 (rata-rata komponen keyword & semantik).
    pub score: f32,
    /// Komponen keyword ternormalisasi (0.0–1.0).
    pub keyword: f32,
    /// Komponen semantik (cosine, 0.0–1.0; 0 bila fitur semantic mati).
    pub semantic: f32,
}

/// Gabungkan hasil keyword (`SearchHit`) & semantik (`SemanticHit`) menjadi satu
/// ranking. Keyword dinormalisasi ke skor tertinggi pada batch ini; skor akhir =
/// rata-rata sederhana kedua komponen. Fungsi murni (tanpa I/O) agar mudah diuji.
pub fn merge_hybrid(kw: &[SearchHit], sem: &[SemanticHit], top: usize) -> Vec<HybridHit> {
    use std::collections::BTreeMap;
    let max_kw = kw.iter().map(|h| h.score).max().unwrap_or(0) as f32;
    // slug -> (kw_norm, semantic, description)
    let mut acc: BTreeMap<String, (f32, f32, String)> = BTreeMap::new();
    for h in kw {
        let norm = if max_kw > 0.0 {
            h.score as f32 / max_kw
        } else {
            0.0
        };
        let e = acc
            .entry(h.name.clone())
            .or_insert((0.0, 0.0, h.description.clone()));
        e.0 = norm;
        if e.2.is_empty() {
            e.2 = h.description.clone();
        }
    }
    for s in sem {
        let e = acc
            .entry(s.name.clone())
            .or_insert((0.0, 0.0, s.description.clone()));
        e.1 = s.score.max(0.0);
        if e.2.is_empty() {
            e.2 = s.description.clone();
        }
    }
    let mut hits: Vec<HybridHit> = acc
        .into_iter()
        .map(|(name, (kwn, sem, desc))| HybridHit {
            name,
            description: desc,
            score: 0.5 * kwn + 0.5 * sem,
            keyword: kwn,
            semantic: sem,
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.name.cmp(&b.name))
    });
    hits.truncate(top);
    hits
}

fn make_snippet(body: &str, pos: usize, qlen: usize) -> String {
    let start = pos.saturating_sub(40);
    let end = (pos + qlen + 40).min(body.len());
    // geser ke batas char yang valid
    let start = floor_char_boundary(body, start);
    let end = ceil_char_boundary(body, end);
    let mut s = body[start..end].replace('\n', " ");
    if start > 0 {
        s = format!("…{s}");
    }
    if end < body.len() {
        s = format!("{s}…");
    }
    s
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn is_memory_file(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return false;
    }
    match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => !name.starts_with('_'), // _MOC.md dan file meta lain diabaikan
        None => false,
    }
}

fn normalize_list(items: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    out.dedup();
    out
}

/// Timestamp lokal RFC3339, mis. `2026-05-30T22:40:00+07:00`.
pub fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CNT: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn tmp_config() -> Config {
        let n = CNT.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mcpobs-mem-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        Config {
            vault_path: dir,
            memory_root: "memory".into(),
            default_project: Some("test".into()),
        }
    }

    fn count_created(raw: &str) -> usize {
        raw.lines().filter(|l| l.starts_with("created:")).count()
    }

    #[test]
    fn write_read_roundtrip() {
        let cfg = tmp_config();
        let out = write_memory(
            &cfg,
            "demo",
            WriteInput {
                name: "Auth Flow".into(),
                description: "desc".into(),
                body: "Pakai JWT. [[deploy-pipeline]]".into(),
                tags: vec!["auth".into(), "security".into()],
                kind: Some("project".into()),
                links: vec!["deploy-pipeline".into()],
            },
        )
        .unwrap();

        assert_eq!(out.slug, "auth-flow");
        assert!(out.created);

        let raw = std::fs::read_to_string(&out.path).unwrap();
        assert_eq!(count_created(&raw), 1, "frontmatter rusak:\n{raw}");

        let mem = read_memory(&cfg, "demo", "auth-flow").unwrap();
        assert_eq!(mem.front.name, "auth-flow");
        assert_eq!(mem.front.tags, vec!["auth", "security"]);
        assert_eq!(mem.front.kind, "project");
        assert_eq!(mem.front.links, vec!["deploy-pipeline"]);
        assert!(mem.body.contains("JWT"));
    }

    #[test]
    fn update_preserves_created_timestamp() {
        let cfg = tmp_config();
        write_memory(&cfg, "demo", simple("X", "d1", "b1")).unwrap();
        let created1 = read_memory(&cfg, "demo", "x").unwrap().front.created;

        let out = write_memory(&cfg, "demo", simple("X", "d2", "b2")).unwrap();
        assert!(!out.created, "write kedua harusnya update, bukan create");

        let m = read_memory(&cfg, "demo", "x").unwrap();
        assert_eq!(m.front.created, created1, "created harus dipertahankan");
        assert_eq!(m.front.description, "d2");
        assert_eq!(m.body, "b2");
    }

    #[test]
    fn search_and_list() {
        let cfg = tmp_config();
        write_memory(&cfg, "demo", simple("Alpha", "soal jwt", "body alpha jwt")).unwrap();
        write_memory(&cfg, "demo", simple("Beta", "soal lain", "body beta")).unwrap();

        let entries = list_entries(&cfg, "demo");
        assert_eq!(entries.len(), 2);

        let hits = search(&cfg, "demo", Some("jwt"), None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "alpha");
    }

    /// Regresi: query multi-kata harus cocok per-token, bukan sebagai frasa utuh.
    /// Dulu query di-`.contains()` sebagai satu string, sehingga "rust konkurensi"
    /// (yang tak pernah muncul berurutan) mengembalikan 0 hasil walau kedua kata ada.
    #[test]
    fn search_multiword_matches_scattered_terms() {
        let cfg = tmp_config();
        write_memory(
            &cfg,
            "demo",
            simple(
                "Alpha",
                "arsitektur",
                "ditulis dalam rust dan menjaga konkurensi via mutex",
            ),
        )
        .unwrap();
        write_memory(&cfg, "demo", simple("Beta", "lain", "tak ada kata kunci")).unwrap();

        let hits = search(&cfg, "demo", Some("rust konkurensi"), None);
        assert_eq!(hits.len(), 1, "query multi-kata harus cocok per-token");
        assert_eq!(hits[0].name, "alpha");
    }

    /// Dok yang cocok lebih banyak term harus berperingkat lebih tinggi (skor
    /// dijumlahkan lintas-term).
    #[test]
    fn search_ranks_more_term_matches_higher() {
        let cfg = tmp_config();
        write_memory(
            &cfg,
            "demo",
            simple("Both", "d", "rust dan konkurensi keduanya"),
        )
        .unwrap();
        write_memory(&cfg, "demo", simple("One", "d", "hanya rust di sini")).unwrap();

        let hits = search(&cfg, "demo", Some("rust konkurensi"), None);
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].name, "both",
            "dok yang cocok lebih banyak token harus di atas"
        );
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn rename_updates_referrers_and_preserves_created() {
        let cfg = tmp_config();
        // target yang akan di-rename
        write_memory(&cfg, "demo", simple("Old Name", "d", "isi")).unwrap();
        let created = read_memory(&cfg, "demo", "old-name").unwrap().front.created;
        // perujuk: via field links + via wikilink body (pakai display berbeda)
        write_memory(
            &cfg,
            "demo",
            WriteInput {
                name: "Referrer".into(),
                description: "d".into(),
                body: "lihat [[Old Name]] juga".into(),
                tags: vec![],
                kind: None,
                links: vec!["old-name".into()],
            },
        )
        .unwrap();

        let out = rename_memory(&cfg, "demo", "old-name", "New Name").unwrap();
        assert_eq!(out.new_slug, "new-name");
        assert_eq!(out.updated_referrers, vec!["referrer"]);

        // file lama hilang, baru ada, created dipertahankan.
        assert!(read_memory(&cfg, "demo", "old-name").is_err());
        let renamed = read_memory(&cfg, "demo", "new-name").unwrap();
        assert_eq!(renamed.front.created, created);

        // perujuk: field links & body wikilink sudah menunjuk slug baru.
        let r = read_memory(&cfg, "demo", "referrer").unwrap();
        assert_eq!(r.front.links, vec!["new-name"]);
        assert!(r.body.contains("[[new-name]]"), "body: {}", r.body);
    }

    #[test]
    fn rename_rejects_existing_target() {
        let cfg = tmp_config();
        write_memory(&cfg, "demo", simple("A", "d", "b")).unwrap();
        write_memory(&cfg, "demo", simple("B", "d", "b")).unwrap();
        let res = rename_memory(&cfg, "demo", "a", "B");
        assert!(res.is_err(), "rename ke slug yang sudah ada harus gagal");
    }

    #[test]
    fn merge_hybrid_combines_and_ranks() {
        let kw = vec![
            SearchHit {
                name: "a".into(),
                description: "da".into(),
                tags: vec![],
                score: 10,
                snippet: String::new(),
            },
            SearchHit {
                name: "b".into(),
                description: "db".into(),
                tags: vec![],
                score: 5,
                snippet: String::new(),
            },
        ];
        let sem = vec![
            SemanticHit {
                name: "b".into(),
                description: "db".into(),
                score: 0.9,
            },
            SemanticHit {
                name: "c".into(),
                description: "dc".into(),
                score: 0.8,
            },
        ];
        let hits = merge_hybrid(&kw, &sem, 5);
        // 3 slug unik (a, b, c). kw max=10 → a:kw=1.0, b:kw=0.5.
        // b = 0.5*0.5 + 0.5*0.9 = 0.70; a = 0.5*1.0 = 0.50; c = 0.5*0.8 = 0.40.
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].name, "b");
        assert!((hits[0].score - 0.70).abs() < 1e-6);
        // c hanya muncul di semantic → keyword=0.
        let c = hits.iter().find(|h| h.name == "c").unwrap();
        assert_eq!(c.keyword, 0.0);
    }

    fn simple(name: &str, desc: &str, body: &str) -> WriteInput {
        WriteInput {
            name: name.into(),
            description: desc.into(),
            body: body.into(),
            tags: vec![],
            kind: None,
            links: vec![],
        }
    }
}
