//! Subsistem dokumen: tulis/baca dokumen panjang (spec, runbook, brainstorm,
//! worklog) sebagai file Markdown di Obsidian Vault.
//!
//! Berbeda dari memori, dokumen disimpan di root terpisah
//! (`<vault>/<docs_root>/<project>/<slug>.md`) dan SENGAJA tidak ikut diindeks
//! ke graf, semantic search, maupun `_MOC.md`. Konsekuensinya, satu-satunya cara
//! menemukan kembali dokumen adalah lewat [`list_docs`] & [`search_docs`] — itulah
//! mengapa keduanya didesain berbarengan dengan tulis.
//!
//! Frontmatter & parsing memakai ulang tipe `Memory`/`Frontmatter` dari modul
//! [`crate::memory`]; field `links` selalu kosong karena dokumen tidak bergraf.

use crate::config::{ensure_dir, Config};
use crate::memory::{now_rfc3339, search_in, Frontmatter, Memory, MemoryEntry, SearchHit};
use crate::project::slugify;
use std::path::{Path, PathBuf};

/// Kind default bila tidak ditentukan & bukan salah satu template dikenal.
const DEFAULT_KIND: &str = "note";

/// Mode penulisan dokumen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Tulis ulang isi dari awal (default untuk spec/runbook).
    Overwrite,
    /// Tambahkan entri ber-timestamp di bawah isi yang ada (default untuk
    /// brainstorm/worklog). Membuat file dari template bila belum ada.
    Append,
}

/// Definisi satu jenis dokumen: folder sama, beda template & mode default.
pub struct DocKind {
    pub name: &'static str,
    pub default_mode: WriteMode,
    /// Scaffold body saat dokumen pertama kali dibuat (boleh kosong).
    pub template: &'static str,
}

const SPEC_TEMPLATE: &str =
    "## Tujuan\n\n## Requirement\n\n## Non-goals\n\n## Desain\n\n## Risiko\n";
const RUNBOOK_TEMPLATE: &str = "## Prasyarat\n\n## Langkah\n\n## Verifikasi\n\n## Rollback\n";

/// Registry jenis dokumen bawaan. Menambah jenis baru = satu entri di sini.
pub const DOC_KINDS: &[DocKind] = &[
    DocKind {
        name: "brainstorm",
        default_mode: WriteMode::Append,
        template: "",
    },
    DocKind {
        name: "worklog",
        default_mode: WriteMode::Append,
        template: "",
    },
    DocKind {
        name: "spec",
        default_mode: WriteMode::Overwrite,
        template: SPEC_TEMPLATE,
    },
    DocKind {
        name: "runbook",
        default_mode: WriteMode::Overwrite,
        template: RUNBOOK_TEMPLATE,
    },
];

/// Cari definisi jenis dokumen berdasarkan nama (mis. `"spec"`).
pub fn doc_kind(name: &str) -> Option<&'static DocKind> {
    DOC_KINDS.iter().find(|k| k.name == name)
}

/// Argumen untuk menulis / menambah sebuah dokumen.
pub struct DocInput {
    pub name: String,
    /// Judul satu baris → disimpan di frontmatter `description`. Boleh kosong
    /// saat append (deskripsi yang ada dipertahankan).
    pub title: String,
    /// Jenis dokumen (mis. `spec`). Kosong → pertahankan yang ada / `note`.
    pub kind: String,
    pub body: String,
    pub tags: Vec<String>,
}

/// Hasil operasi tulis dokumen.
pub struct DocOutcome {
    pub slug: String,
    pub path: PathBuf,
    pub created: bool,
    pub mode: WriteMode,
}

/// Tulis (buat / overwrite / append) sebuah dokumen.
///
/// - `Overwrite` pada dokumen baru dengan body kosong → diisi template `kind`.
/// - `Append` → entri baru `## <timestamp>` ditambahkan; bila file belum ada,
///   dibuat dari template `kind` lalu di-append.
///
/// `created` dipertahankan saat update; `updated` selalu di-refresh.
pub fn write_doc(
    config: &Config,
    project: &str,
    input: DocInput,
    mode: WriteMode,
) -> anyhow::Result<DocOutcome> {
    let slug = slugify(&input.name);
    anyhow::ensure!(
        !slug.is_empty(),
        "nama dokumen tidak valid setelah disanitasi"
    );

    let dir = config.docs_project_dir(project);
    ensure_dir(&dir)?;
    let path = config.docs_file(project, &slug);

    let now = now_rfc3339();
    let existing = read_doc(config, project, &slug).ok();
    let created = existing.is_none();
    let created_ts = existing
        .as_ref()
        .map(|m| m.front.created.clone())
        .unwrap_or_else(|| now.clone());

    // Kind: argumen eksplisit menang; jika kosong, pertahankan yang ada.
    let kind = if !input.kind.trim().is_empty() {
        input.kind.trim().to_string()
    } else {
        existing
            .as_ref()
            .map(|m| m.front.kind.clone())
            .unwrap_or_else(|| DEFAULT_KIND.to_string())
    };

    // Description: judul baru menang; jika kosong, pertahankan yang ada.
    let description = if !input.title.trim().is_empty() {
        input.title.trim().to_string()
    } else {
        existing
            .as_ref()
            .map(|m| m.front.description.clone())
            .unwrap_or_default()
    };

    // Tags: argumen non-kosong menang; jika kosong, pertahankan yang ada.
    let tags = if input.tags.iter().any(|t| !t.trim().is_empty()) {
        normalize(input.tags)
    } else {
        existing
            .as_ref()
            .map(|m| m.front.tags.clone())
            .unwrap_or_default()
    };

    let template = || {
        doc_kind(&kind)
            .map(|k| k.template)
            .unwrap_or("")
            .to_string()
    };

    let body = match mode {
        WriteMode::Overwrite => {
            if created && input.body.trim().is_empty() {
                template()
            } else {
                input.body.clone()
            }
        }
        WriteMode::Append => {
            let mut base = match &existing {
                Some(m) => m.body.clone(),
                None => template(),
            };
            let entry = format!("## {now}\n\n{}", input.body.trim());
            if !base.trim().is_empty() {
                base.push_str("\n\n");
            }
            base.push_str(&entry);
            base
        }
    };

    let front = Frontmatter {
        name: slug.clone(),
        description,
        tags,
        kind,
        links: Vec::new(), // dokumen tidak bergraf
        created: created_ts,
        updated: now,
    };

    let mem = Memory { front, body };
    std::fs::write(&path, mem.to_file_string()?)?;

    Ok(DocOutcome {
        slug,
        path,
        created,
        mode,
    })
}

/// Baca satu dokumen berdasarkan slug.
pub fn read_doc(config: &Config, project: &str, name: &str) -> anyhow::Result<Memory> {
    let slug = slugify(name);
    let path = config.docs_file(project, &slug);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("gagal membaca '{}': {e}", path.display()))?;
    Memory::from_file_string(&raw)
}

/// Muat semua dokumen dalam satu project. Khusus folder docs — TIDAK dipakai
/// oleh indexer memori, jadi dokumen tak pernah mencemari graf/semantic/MOC.
pub fn load_all_docs(config: &Config, project: &str) -> Vec<Memory> {
    let dir = config.docs_project_dir(project);
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_doc_file(&path) {
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

/// Daftar ringkas dokumen dalam satu project, opsional difilter `kind`.
pub fn list_docs(config: &Config, project: &str, kind: Option<&str>) -> Vec<MemoryEntry> {
    load_all_docs(config, project)
        .iter()
        .filter(|m| kind.is_none_or(|k| m.front.kind == k))
        .map(|m| MemoryEntry::from(&m.front))
        .collect()
}

/// Pencarian keyword di folder docs, opsional difilter `kind`.
pub fn search_docs(
    config: &Config,
    project: &str,
    query: Option<&str>,
    kind: Option<&str>,
) -> Vec<SearchHit> {
    let docs: Vec<Memory> = load_all_docs(config, project)
        .into_iter()
        .filter(|m| kind.is_none_or(|k| m.front.kind == k))
        .collect();
    search_in(&docs, query, None)
}

fn is_doc_file(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return false;
    }
    match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => !name.starts_with('_'),
        None => false,
    }
}

fn normalize(items: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CNT: AtomicU64 = AtomicU64::new(0);

    fn tmp_config() -> Config {
        let n = CNT.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mcpobs-docs-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        Config {
            vault_path: dir,
            memory_root: "memory".into(),
            docs_root: "docs".into(),
            default_project: Some("test".into()),
        }
    }

    fn input(name: &str, kind: &str, title: &str, body: &str) -> DocInput {
        DocInput {
            name: name.into(),
            title: title.into(),
            kind: kind.into(),
            body: body.into(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn write_read_roundtrip() {
        let cfg = tmp_config();
        let out = write_doc(
            &cfg,
            "demo",
            input("Login Spec", "spec", "Spec login", "isi body"),
            WriteMode::Overwrite,
        )
        .unwrap();
        assert!(out.created);
        assert_eq!(out.slug, "login-spec");

        let doc = read_doc(&cfg, "demo", "login-spec").unwrap();
        assert_eq!(doc.front.kind, "spec");
        assert_eq!(doc.front.description, "Spec login");
        assert_eq!(doc.body, "isi body");
        assert!(doc.front.links.is_empty(), "dokumen tidak bergraf");
    }

    #[test]
    fn new_doc_with_empty_body_uses_template() {
        let cfg = tmp_config();
        write_doc(
            &cfg,
            "demo",
            input("Deploy", "runbook", "Runbook deploy", ""),
            WriteMode::Overwrite,
        )
        .unwrap();
        let doc = read_doc(&cfg, "demo", "deploy").unwrap();
        assert!(doc.body.contains("## Prasyarat"));
        assert!(doc.body.contains("## Rollback"));
    }

    #[test]
    fn append_adds_sections_and_preserves_created() {
        let cfg = tmp_config();
        let first = write_doc(
            &cfg,
            "demo",
            input("Sprint Log", "worklog", "Log sprint", "entri pertama"),
            WriteMode::Append,
        )
        .unwrap();
        assert!(first.created);
        let created_ts = read_doc(&cfg, "demo", "sprint-log").unwrap().front.created;

        let second = write_doc(
            &cfg,
            "demo",
            input("Sprint Log", "", "", "entri kedua"),
            WriteMode::Append,
        )
        .unwrap();
        assert!(!second.created);

        let doc = read_doc(&cfg, "demo", "sprint-log").unwrap();
        assert!(doc.body.contains("entri pertama"));
        assert!(doc.body.contains("entri kedua"));
        // dua entri => minimal dua heading "## "
        assert!(doc.body.matches("## ").count() >= 2);
        assert_eq!(doc.front.created, created_ts, "created dipertahankan");
        assert_eq!(doc.front.kind, "worklog", "kind lama dipertahankan");
    }

    #[test]
    fn append_autocreates_from_template() {
        let cfg = tmp_config();
        let out = write_doc(
            &cfg,
            "demo",
            input("Ide Baru", "brainstorm", "", "ide pertama"),
            WriteMode::Append,
        )
        .unwrap();
        assert!(out.created);
        let doc = read_doc(&cfg, "demo", "ide-baru").unwrap();
        assert_eq!(doc.front.kind, "brainstorm");
        assert!(doc.body.contains("ide pertama"));
    }

    #[test]
    fn list_filters_by_kind() {
        let cfg = tmp_config();
        write_doc(&cfg, "p", input("A", "spec", "", "x"), WriteMode::Overwrite).unwrap();
        write_doc(
            &cfg,
            "p",
            input("B", "runbook", "", "y"),
            WriteMode::Overwrite,
        )
        .unwrap();

        assert_eq!(list_docs(&cfg, "p", None).len(), 2);
        let specs = list_docs(&cfg, "p", Some("spec"));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "a");
    }

    #[test]
    fn search_finds_by_body() {
        let cfg = tmp_config();
        write_doc(
            &cfg,
            "p",
            input("Catatan", "spec", "", "membahas indeks vektor"),
            WriteMode::Overwrite,
        )
        .unwrap();
        let hits = search_docs(&cfg, "p", Some("vektor"), None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "catatan");
        // filter kind yang tak cocok => kosong
        assert!(search_docs(&cfg, "p", Some("vektor"), Some("runbook")).is_empty());
    }

    #[test]
    fn docs_isolated_from_memory_graph() {
        let cfg = tmp_config();
        // Tulis satu dokumen & satu memori di project yang sama.
        write_doc(
            &cfg,
            "proj",
            input("Spec X", "spec", "", "isi doc"),
            WriteMode::Overwrite,
        )
        .unwrap();
        memory::write_memory(
            &cfg,
            "proj",
            memory::WriteInput {
                name: "Fakta Y".into(),
                description: "sebuah fakta".into(),
                body: "isi memori".into(),
                tags: vec![],
                kind: None,
                links: vec![],
            },
        )
        .unwrap();

        // Sisi memori tidak melihat dokumen.
        let mem_entries = memory::list_entries(&cfg, "proj");
        assert_eq!(mem_entries.len(), 1);
        assert_eq!(mem_entries[0].name, "fakta-y");

        // Sisi dokumen tidak melihat memori.
        let doc_entries = list_docs(&cfg, "proj", None);
        assert_eq!(doc_entries.len(), 1);
        assert_eq!(doc_entries[0].name, "spec-x");
    }
}
