//! Document subsystem: write/read long documents (spec, runbook, brainstorm,
//! worklog) as Markdown files in the Obsidian Vault.
//!
//! Unlike memories, documents are stored in a separate root
//! (`<vault>/<docs_root>/<project>/<slug>.md`) and are DELIBERATELY excluded
//! from the graph, semantic search, and `_MOC.md`. As a result, the only way to
//! find a document again is through [`list_docs`] & [`search_docs`] — which is
//! why both were designed alongside the write path.
//!
//! Frontmatter & parsing reuse the `Memory`/`Frontmatter` types from the
//! [`crate::memory`] module; the `links` field is always empty because documents
//! are not part of the graph.

use crate::config::{ensure_dir, Config};
use crate::memory::{now_rfc3339, search_in, Frontmatter, Memory, MemoryEntry, SearchHit};
use crate::project::slugify;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Default kind when none is specified & it is not one of the known templates.
const DEFAULT_KIND: &str = "note";

/// Document write mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Rewrite the content from scratch (default for spec/runbook).
    Overwrite,
    /// Append a timestamped entry below the existing content (default for
    /// brainstorm/worklog). Creates the file from the template if it does not
    /// exist yet.
    Append,
}

/// Definition of a single document kind: same folder, different template &
/// default mode.
pub struct DocKind {
    pub name: &'static str,
    pub default_mode: WriteMode,
    /// Scaffold body when the document is first created (may be empty).
    pub template: &'static str,
}

const SPEC_TEMPLATE: &str = "## Goal\n\n## Requirements\n\n## Non-goals\n\n## Design\n\n## Risks\n";
const RUNBOOK_TEMPLATE: &str = "## Prerequisites\n\n## Steps\n\n## Verification\n\n## Rollback\n";

/// Registry of built-in document kinds. Adding a new kind = one entry here.
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

/// Look up a document kind definition by name (e.g. `"spec"`).
pub fn doc_kind(name: &str) -> Option<&'static DocKind> {
    DOC_KINDS.iter().find(|k| k.name == name)
}

/// Arguments for writing / appending to a document.
pub struct DocInput {
    pub name: String,
    /// One-line title → stored in the `description` frontmatter. May be empty
    /// on append (the existing description is preserved).
    pub title: String,
    /// Document kind (e.g. `spec`). Empty → keep the existing one / `note`.
    pub kind: String,
    pub body: String,
    pub tags: Vec<String>,
}

/// Result of a document write operation.
pub struct DocOutcome {
    pub slug: String,
    pub path: PathBuf,
    pub created: bool,
    pub mode: WriteMode,
}

/// Write (create / overwrite / append) a document.
///
/// - `Overwrite` on a new document with an empty body → filled with the `kind`
///   template.
/// - `Append` → a new `## <timestamp>` entry is added; if the file does not
///   exist yet, it is created from the `kind` template and then appended to.
///
/// `created` is preserved on update; `updated` is always refreshed.
pub fn write_doc(
    config: &Config,
    project: &str,
    input: DocInput,
    mode: WriteMode,
) -> anyhow::Result<DocOutcome> {
    let slug = slugify(&input.name);
    anyhow::ensure!(
        !slug.is_empty(),
        "document name is invalid after sanitization"
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

    // Kind: an explicit argument wins; if empty, keep the existing one.
    let kind = if !input.kind.trim().is_empty() {
        input.kind.trim().to_string()
    } else {
        existing
            .as_ref()
            .map(|m| m.front.kind.clone())
            .unwrap_or_else(|| DEFAULT_KIND.to_string())
    };

    // Description: a new title wins; if empty, keep the existing one.
    let description = if !input.title.trim().is_empty() {
        input.title.trim().to_string()
    } else {
        existing
            .as_ref()
            .map(|m| m.front.description.clone())
            .unwrap_or_default()
    };

    // Tags: a non-empty argument wins; if empty, keep the existing one.
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
        links: Vec::new(), // documents are not part of the graph
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

/// Read a single document by slug.
pub fn read_doc(config: &Config, project: &str, name: &str) -> anyhow::Result<Memory> {
    let slug = slugify(name);
    let path = config.docs_file(project, &slug);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", path.display()))?;
    Memory::from_file_string(&raw)
}

/// Delete a single document. Returns `true` if the file actually existed & was removed.
pub fn delete_doc(config: &Config, project: &str, name: &str) -> anyhow::Result<bool> {
    let slug = slugify(name);
    let path = config.docs_file(project, &slug);
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)?;
    Ok(true)
}

/// Result of a document rename operation.
pub struct DocRenameOutcome {
    pub old_slug: String,
    pub new_slug: String,
}

/// Rename (slug) a document. Simpler than for memories because documents are
/// not part of the graph — there are no incoming links to update. The `created`
/// timestamp is preserved; `updated` is refreshed.
pub fn rename_doc(
    config: &Config,
    project: &str,
    old_name: &str,
    new_name: &str,
) -> anyhow::Result<DocRenameOutcome> {
    let old_slug = slugify(old_name);
    let new_slug = slugify(new_name);
    anyhow::ensure!(
        !new_slug.is_empty(),
        "new name is invalid after sanitization"
    );
    anyhow::ensure!(
        old_slug != new_slug,
        "old & new names produce the same slug ('{new_slug}')"
    );

    let old = read_doc(config, project, &old_slug)
        .map_err(|_| anyhow::anyhow!("document '{old_slug}' not found in project '{project}'"))?;
    let new_path = config.docs_file(project, &new_slug);
    anyhow::ensure!(
        !new_path.exists(),
        "target '{new_slug}' already exists — choose another name"
    );

    let mut front = old.front.clone();
    front.name = new_slug.clone();
    front.updated = now_rfc3339();
    let new_mem = Memory {
        front,
        body: old.body.clone(),
    };
    std::fs::write(&new_path, new_mem.to_file_string()?)?;
    std::fs::remove_file(config.docs_file(project, &old_slug))?;

    Ok(DocRenameOutcome { old_slug, new_slug })
}

/// Load all documents in a single project. Docs folder only — NOT used by the
/// memory indexer, so documents never pollute the graph/semantic/MOC.
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

/// Brief list of documents in a single project, optionally filtered by `kind`.
pub fn list_docs(config: &Config, project: &str, kind: Option<&str>) -> Vec<MemoryEntry> {
    load_all_docs(config, project)
        .iter()
        .filter(|m| kind.is_none_or(|k| m.front.kind == k))
        .map(|m| MemoryEntry::from(&m.front))
        .collect()
}

/// Keyword search in the docs folder, optionally filtered by `kind`.
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

/// Build the text content of `_DOCS.md` (the document index) for a project.
/// Grouped by `type`, each entry = wikilink + description + update time.
pub fn build_docs_index_string(project: &str, docs: &[Memory]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# 📄 Document Index — `{project}`\n\n"));
    out.push_str(&format!(
        "> Auto-generated by mcp-obsidian on {}. \
         Do not edit manually — changes will be overwritten.\n\n",
        now_rfc3339()
    ));

    if docs.is_empty() {
        out.push_str("_No documents yet for this project._\n");
        return out;
    }

    out.push_str(&format!("Total documents: **{}**\n\n", docs.len()));

    let mut by_kind: BTreeMap<String, Vec<&Memory>> = BTreeMap::new();
    for d in docs {
        by_kind.entry(d.front.kind.clone()).or_default().push(d);
    }
    for (kind, items) in &by_kind {
        out.push_str(&format!("## {}\n\n", title_case(kind)));
        for d in items {
            let f = &d.front;
            let desc = if f.description.is_empty() {
                String::new()
            } else {
                format!(" — {}", f.description)
            };
            out.push_str(&format!(
                "- [[{}]]{}  _(updated {})_\n",
                f.name, desc, f.updated
            ));
        }
        out.push('\n');
    }

    out
}

/// Regenerate `_DOCS.md` on disk for a project. Returns the text content.
pub fn regenerate_docs_index(config: &Config, project: &str) -> anyhow::Result<String> {
    let docs = load_all_docs(config, project);
    let content = build_docs_index_string(project, &docs);
    ensure_dir(&config.docs_project_dir(project))?;
    std::fs::write(config.docs_index_file(project), &content)?;
    Ok(content)
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
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
        assert!(
            doc.front.links.is_empty(),
            "documents are not part of the graph"
        );
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
        assert!(doc.body.contains("## Prerequisites"));
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
        // two entries => at least two "## " headings
        assert!(doc.body.matches("## ").count() >= 2);
        assert_eq!(doc.front.created, created_ts, "created is preserved");
        assert_eq!(doc.front.kind, "worklog", "old kind is preserved");
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
        // a non-matching kind filter => empty
        assert!(search_docs(&cfg, "p", Some("vektor"), Some("runbook")).is_empty());
    }

    #[test]
    fn delete_removes_file() {
        let cfg = tmp_config();
        write_doc(
            &cfg,
            "p",
            input("Buang", "spec", "", "x"),
            WriteMode::Overwrite,
        )
        .unwrap();
        assert!(delete_doc(&cfg, "p", "buang").unwrap());
        assert!(read_doc(&cfg, "p", "buang").is_err());
        // delete again → false (does not exist)
        assert!(!delete_doc(&cfg, "p", "buang").unwrap());
    }

    #[test]
    fn rename_moves_and_preserves_created() {
        let cfg = tmp_config();
        write_doc(
            &cfg,
            "p",
            input("Nama Lama", "runbook", "Judul", "isi runbook"),
            WriteMode::Overwrite,
        )
        .unwrap();
        let created = read_doc(&cfg, "p", "nama-lama").unwrap().front.created;

        let out = rename_doc(&cfg, "p", "Nama Lama", "Nama Baru").unwrap();
        assert_eq!(out.old_slug, "nama-lama");
        assert_eq!(out.new_slug, "nama-baru");

        assert!(
            read_doc(&cfg, "p", "nama-lama").is_err(),
            "old file is gone"
        );
        let renamed = read_doc(&cfg, "p", "nama-baru").unwrap();
        assert_eq!(renamed.front.name, "nama-baru");
        assert_eq!(renamed.front.kind, "runbook");
        assert_eq!(renamed.body, "isi runbook");
        assert_eq!(renamed.front.created, created, "created is preserved");
    }

    #[test]
    fn rename_to_existing_fails() {
        let cfg = tmp_config();
        write_doc(&cfg, "p", input("A", "spec", "", "a"), WriteMode::Overwrite).unwrap();
        write_doc(&cfg, "p", input("B", "spec", "", "b"), WriteMode::Overwrite).unwrap();
        assert!(rename_doc(&cfg, "p", "A", "B").is_err());
        // A stays intact after the failed rename
        assert_eq!(read_doc(&cfg, "p", "a").unwrap().body, "a");
    }

    #[test]
    fn index_groups_by_kind_and_excludes_itself() {
        let cfg = tmp_config();
        write_doc(
            &cfg,
            "p",
            input("A", "spec", "Spec A", "x"),
            WriteMode::Overwrite,
        )
        .unwrap();
        write_doc(
            &cfg,
            "p",
            input("B", "runbook", "RB B", "y"),
            WriteMode::Overwrite,
        )
        .unwrap();

        let content = regenerate_docs_index(&cfg, "p").unwrap();
        assert!(content.contains("Total documents: **2**"));
        assert!(content.contains("## Spec"));
        assert!(content.contains("## Runbook"));
        assert!(content.contains("[[a]]"));
        assert!(content.contains("[[b]]"));

        // The _DOCS.md file exists on disk but is NOT read back as a document.
        assert!(cfg.docs_index_file("p").exists());
        let entries = list_docs(&cfg, "p", None);
        assert_eq!(
            entries.len(),
            2,
            "_DOCS.md must not be counted as a document"
        );
    }

    #[test]
    fn index_empty_project_is_graceful() {
        let cfg = tmp_config();
        let content = regenerate_docs_index(&cfg, "kosong").unwrap();
        assert!(content.contains("No documents yet"));
    }

    #[test]
    fn docs_isolated_from_memory_graph() {
        let cfg = tmp_config();
        // Write one document & one memory in the same project.
        write_doc(
            &cfg,
            "proj",
            input("Spec X", "spec", "", "doc content"),
            WriteMode::Overwrite,
        )
        .unwrap();
        memory::write_memory(
            &cfg,
            "proj",
            memory::WriteInput {
                name: "Fact Y".into(),
                description: "a fact".into(),
                body: "memory content".into(),
                tags: vec![],
                kind: None,
                links: vec![],
            },
        )
        .unwrap();

        // The memory side does not see the document.
        let mem_entries = memory::list_entries(&cfg, "proj");
        assert_eq!(mem_entries.len(), 1);
        assert_eq!(mem_entries[0].name, "fact-y");

        // The document side does not see the memory.
        let doc_entries = list_docs(&cfg, "proj", None);
        assert_eq!(doc_entries.len(), 1);
        assert_eq!(doc_entries[0].name, "spec-x");
    }
}
