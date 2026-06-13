//! `recall`: unified context retrieval for AI agents.
//!
//! Combines three existing capabilities into ONE call, so the agent does not have
//! to chain them together itself (search → read → backlinks → cluster):
//!
//! 1. **Semantic search** (`embed::semantic_search`) → the top-K memories most
//!    relevant to the query (based on meaning).
//! 2. For each hit, attach its **full content** (body) + **graph neighbors**
//!    (outgoing links & backlinks, via `links::LinkGraph`).
//! 3. The **theme** (`cluster`) of each hit + a list of other memories in the same theme.
//!
//! The result is a single structured payload ready to be used as LLM context.
//!
//! Note: depends on `embed::semantic_search`, so it requires a build with the
//! `semantic` feature. Without that feature, the error is propagated as-is.

use crate::cluster;
use crate::config::Config;
use crate::embed;
use crate::links::LinkGraph;
use crate::memory::{self, Memory};
use crate::project::slugify;
use serde::Serialize;
use std::collections::HashMap;

/// A single memory in the recall result, complete with its graph context.
#[derive(Debug, Clone, Serialize)]
pub struct RecallItem {
    pub name: String,
    pub description: String,
    /// Semantic relevance to the query (cosine similarity).
    pub score: f32,
    #[serde(rename = "type")]
    pub kind: String,
    pub tags: Vec<String>,
    /// Full content of the memory (Markdown body).
    pub body: String,
    /// Memories linked to by this item (valid outgoing links).
    pub links: Vec<String>,
    /// Memories that link to this item (backlinks).
    pub backlinks: Vec<String>,
    /// Other memories in the same theme (same cluster) as this item.
    pub theme: Vec<String>,
}

/// The complete recall result.
#[derive(Debug, Clone, Serialize)]
pub struct RecallResult {
    pub query: String,
    pub project: String,
    pub items: Vec<RecallItem>,
}

/// Perform recall: find memories relevant to `query`, then enrich each result
/// with its full content + graph neighbors + theme.
pub fn recall(
    config: &Config,
    project: &str,
    query: &str,
    top: usize,
) -> anyhow::Result<RecallResult> {
    let memories = memory::load_all(config, project);

    // 1. Semantic retrieval (propagates the error if the `semantic` feature is disabled).
    let hits = embed::semantic_search(config, project, query, top, &memories)?;

    // 2. Supporting structures computed once for all hits.
    let graph = LinkGraph::build(&memories);
    let clustering = cluster::cluster(&memories);

    // slug → cluster index, so theme lookup is O(1).
    let mut theme_of: HashMap<String, usize> = HashMap::new();
    for (i, c) in clustering.clusters.iter().enumerate() {
        for m in &c.members {
            theme_of.insert(m.clone(), i);
        }
    }

    // slug → memory, for accessing the full content.
    let by_slug: HashMap<String, &Memory> = memories
        .iter()
        .map(|m| (slugify(&m.front.name), m))
        .collect();

    // 3. Assemble each hit into a RecallItem.
    let mut items = Vec::with_capacity(hits.len());
    for hit in hits {
        let slug = &hit.name;
        let mem = by_slug.get(slug);

        let (kind, tags, body) = match mem {
            Some(m) => (m.front.kind.clone(), m.front.tags.clone(), m.body.clone()),
            None => (String::new(), Vec::new(), String::new()),
        };

        // Valid outgoing links (pointing to memories that exist).
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

        // Same-theme peers (members of the same cluster, excluding itself).
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

    // Without the `semantic` feature, recall must propagate the semantic_search
    // error (not panic), because the core retrieval is unavailable.
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

        let res = recall(&cfg, "demo", "anything", 3);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("--features semantic"));
    }
}
