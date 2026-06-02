//! Representasi satu memori dan operasi baca/tulis ke file Markdown.
//!
//! Setiap memori = satu file `.md` dengan frontmatter YAML di atas dan body
//! Markdown di bawah. Body boleh berisi `[[wikilink]]` ke memori lain.

use crate::config::{ensure_dir, Config};
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
    let q = query.map(|s| s.to_lowercase());
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

        if let Some(q) = &q {
            if f.name.to_lowercase().contains(q) {
                score += 5;
            }
            if f.description.to_lowercase().contains(q) {
                score += 3;
            }
            if f.tags.iter().any(|x| x.to_lowercase().contains(q)) {
                score += 2;
            }
            let body_lower = mem.body.to_lowercase();
            if let Some(pos) = body_lower.find(q) {
                score += 1;
                snippet = make_snippet(&mem.body, pos, q.len());
            }
            if score == 0 {
                continue; // ada query tapi tidak cocok sama sekali
            }
        } else {
            // tanpa query (mungkin hanya filter tag): semua lolos
            score = 1;
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
