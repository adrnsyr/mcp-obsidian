//! `recall`: retrieval konteks terpadu untuk AI agent.
//!
//! Menggabungkan tiga kemampuan yang sudah ada menjadi SATU panggilan, agar
//! agent tidak perlu merangkai sendiri (search → read → backlinks → cluster):
//!
//! 1. **Pencarian semantik** (`embed::semantic_search`) → top-K memori paling
//!    relevan dengan query (berdasarkan makna).
//! 2. Untuk tiap hit, lampirkan **isi penuh** (body) + **tetangga graf**
//!    (tautan keluar & backlink, via `links::LinkGraph`).
//! 3. **Tema** (`cluster`) tiap hit + daftar memori setema lainnya.
//!
//! Hasilnya satu payload terstruktur siap dijadikan konteks LLM.
//!
//! Catatan: bergantung pada `embed::semantic_search`, jadi memerlukan build
//! dengan fitur `semantic`. Tanpa fitur itu, error diteruskan apa adanya.

use crate::cluster;
use crate::config::Config;
use crate::embed;
use crate::links::LinkGraph;
use crate::memory::{self, Memory};
use crate::project::slugify;
use serde::Serialize;
use std::collections::HashMap;

/// Satu memori dalam hasil recall, lengkap dengan konteks grafnya.
#[derive(Debug, Clone, Serialize)]
pub struct RecallItem {
    pub name: String,
    pub description: String,
    /// Relevansi semantik terhadap query (cosine similarity).
    pub score: f32,
    #[serde(rename = "type")]
    pub kind: String,
    pub tags: Vec<String>,
    /// Isi penuh memori (body Markdown).
    pub body: String,
    /// Memori yang ditaut oleh item ini (tautan keluar yang valid).
    pub links: Vec<String>,
    /// Memori yang menaut item ini (backlink).
    pub backlinks: Vec<String>,
    /// Memori lain yang setema (satu klaster) dengan item ini.
    pub theme: Vec<String>,
}

/// Hasil recall lengkap.
#[derive(Debug, Clone, Serialize)]
pub struct RecallResult {
    pub query: String,
    pub project: String,
    pub items: Vec<RecallItem>,
}

/// Lakukan recall: cari memori relevan dengan `query`, lalu perkaya tiap hasil
/// dengan isi penuh + tetangga graf + tema.
pub fn recall(
    config: &Config,
    project: &str,
    query: &str,
    top: usize,
) -> anyhow::Result<RecallResult> {
    let memories = memory::load_all(config, project);

    // 1. Retrieval semantik (meneruskan error bila fitur `semantic` mati).
    let hits = embed::semantic_search(config, project, query, top, &memories)?;

    // 2. Struktur pendukung dihitung sekali untuk semua hit.
    let graph = LinkGraph::build(&memories);
    let clustering = cluster::cluster(&memories);

    // slug → indeks klaster, agar pencarian tema O(1).
    let mut theme_of: HashMap<String, usize> = HashMap::new();
    for (i, c) in clustering.clusters.iter().enumerate() {
        for m in &c.members {
            theme_of.insert(m.clone(), i);
        }
    }

    // slug → memori, untuk akses isi penuh.
    let by_slug: HashMap<String, &Memory> = memories
        .iter()
        .map(|m| (slugify(&m.front.name), m))
        .collect();

    // 3. Rakit tiap hit menjadi RecallItem.
    let mut items = Vec::with_capacity(hits.len());
    for hit in hits {
        let slug = &hit.name;
        let mem = by_slug.get(slug);

        let (kind, tags, body) = match mem {
            Some(m) => (m.front.kind.clone(), m.front.tags.clone(), m.body.clone()),
            None => (String::new(), Vec::new(), String::new()),
        };

        // Tautan keluar yang valid (menunjuk memori yang ada).
        let links: Vec<String> = graph
            .forward
            .get(slug)
            .map(|outs| {
                outs.iter()
                    .filter(|t| graph.existing.contains(*t))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let backlinks = graph.backlinks_of(slug);

        // Teman setema (anggota klaster yang sama, tanpa dirinya sendiri).
        let theme = theme_of
            .get(slug)
            .map(|&i| {
                clustering.clusters[i]
                    .members
                    .iter()
                    .filter(|m| *m != slug)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        items.push(RecallItem {
            name: hit.name,
            description: hit.description,
            score: hit.score,
            kind,
            tags,
            body,
            links,
            backlinks,
            theme,
        });
    }

    Ok(RecallResult {
        query: query.to_string(),
        project: project.to_string(),
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::WriteInput;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CNT: AtomicU64 = AtomicU64::new(0);

    fn tmp_config() -> Config {
        let n = CNT.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mcpobs-recall-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        Config {
            vault_path: dir,
            memory_root: "memory".into(),
            docs_root: "docs".into(),
            default_project: Some("test".into()),
        }
    }

    // Tanpa fitur `semantic`, recall harus meneruskan error semantic_search
    // (bukan panik), karena retrieval inti tak tersedia.
    #[cfg(not(feature = "semantic"))]
    #[test]
    fn recall_without_feature_errors() {
        let cfg = tmp_config();
        memory::write_memory(
            &cfg,
            "demo",
            WriteInput {
                name: "A".into(),
                description: "d".into(),
                body: "b".into(),
                tags: vec![],
                kind: None,
                links: vec![],
            },
        )
        .unwrap();

        let res = recall(&cfg, "demo", "apa pun", 3);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("--features semantic"));
    }
}
