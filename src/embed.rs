//! Semantic search based on local embeddings (pure-Rust via candle).
//!
//! This feature is **opt-in**: it is only fully compiled when the crate is built
//! with `--features semantic`. Without that feature, `semantic_search` returns an
//! error explaining how to enable it — so the tool stays registered on the server
//! regardless of the build.
//!
//! Design:
//! - Model: `sentence-transformers/all-MiniLM-L6-v2` (384 dim), downloaded once
//!   into the HuggingFace cache and then used offline.
//! - The embedding of each memory (the text `name + description + body`) is cached
//!   to the sidecar file `memory/<project>/.embeddings.json`. Only memories that
//!   changed (detected via a content hash) are re-embedded.
//! - Relevance score = cosine similarity. Since the vectors are already
//!   L2-normalized, cosine = dot product.

use crate::config::Config;
use crate::memory::Memory;
use serde::{Deserialize, Serialize};

/// Embedding model identity (used to invalidate the index when the model changes).
/// A **multilingual** model (50+ languages, including Indonesian) — important
/// because memories may be in non-English languages. BERT architecture, 384 dim,
/// no special prefix, so it is a drop-in for the pipeline below. Changing this
/// value automatically invalidates all sidecar indexes (see `EmbeddingIndex::load`).
pub const MODEL_ID: &str = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2";
/// Embedding vector dimension of the model above.
pub const EMBED_DIM: usize = 384;
/// Name of the per-project sidecar index file.
const INDEX_FILE: &str = ".embeddings.json";

/// A single semantic search result.
#[derive(Debug, Clone, Serialize)]
pub struct SemanticHit {
    pub name: String,
    pub description: String,
    /// Cosine similarity against the query (−1.0..1.0; higher is more relevant).
    pub score: f32,
}

// ===================== Sidecar index (always available) =====================

/// Index entry: content hash (for change detection) + embedding vector.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexEntry {
    hash: u64,
    vector: Vec<f32>,
}

/// The embedding index of a single project, stored as a JSON sidecar file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmbeddingIndex {
    /// The model used; if it differs from the current `MODEL_ID`, the index is discarded.
    model: String,
    /// Vector dimension; an extra safeguard.
    dim: usize,
    /// slug -> entry.
    entries: std::collections::BTreeMap<String, IndexEntry>,
}

impl EmbeddingIndex {
    fn empty() -> Self {
        Self {
            model: MODEL_ID.to_string(),
            dim: EMBED_DIM,
            entries: std::collections::BTreeMap::new(),
        }
    }

    /// Load the index from disk; return an empty index if it is missing /
    /// mismatched (model or dimension changed) / corrupt.
    fn load(config: &Config, project: &str) -> Self {
        let path = config.project_dir(project).join(INDEX_FILE);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::empty();
        };
        match serde_json::from_str::<EmbeddingIndex>(&raw) {
            Ok(idx) if idx.model == MODEL_ID && idx.dim == EMBED_DIM => idx,
            _ => Self::empty(), // model/dim changed or corrupt → discard
        }
    }

    fn save(&self, config: &Config, project: &str) -> anyhow::Result<()> {
        let path = config.project_dir(project).join(INDEX_FILE);
        std::fs::write(&path, serde_json::to_string(self)?)?;
        Ok(())
    }
}

/// The text that is embedded for a memory.
fn memory_text(m: &Memory) -> String {
    format!(
        "{}\n{}\n{}",
        m.front.name.replace('-', " "),
        m.front.description,
        m.body
    )
}

/// Content hash of a memory (FNV-1a 64-bit) for change detection — sufficient and dependency-free.
fn content_hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Cosine similarity of two normalized vectors (= dot product). Safe when the
/// lengths differ (takes the minimum), though ideally they are always equal.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

// ============================ Path with the feature enabled ============================

#[cfg(feature = "semantic")]
mod backend {
    use super::*;
    use anyhow::{Context, Error as E};
    use candle_core::{Device, Tensor};
    use candle_nn::VarBuilder;
    use candle_transformers::models::bert::{BertModel, Config as BertConfig, DTYPE};
    use hf_hub::{api::sync::Api, Repo, RepoType};
    use std::sync::Mutex;
    use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer};

    /// Wrapper around the BERT model + tokenizer. Built once, used repeatedly.
    pub struct Embedder {
        model: BertModel,
        tokenizer: Tokenizer,
        device: Device,
    }

    impl Embedder {
        /// Load the model (blocking; downloads on the first run, then uses the HF cache).
        pub fn load() -> anyhow::Result<Self> {
            let device = Device::Cpu;
            let repo = Repo::new(MODEL_ID.to_string(), RepoType::Model);
            let api = Api::new()
                .context("failed to initialize HuggingFace API")?
                .repo(repo);

            let config_path = api.get("config.json").context("download config.json")?;
            let tokenizer_path = api
                .get("tokenizer.json")
                .context("download tokenizer.json")?;
            let weights_path = api
                .get("model.safetensors")
                .context("download model.safetensors")?;

            let bert_config: BertConfig =
                serde_json::from_str(&std::fs::read_to_string(config_path)?)?;

            let mut tokenizer = Tokenizer::from_file(tokenizer_path).map_err(E::msg)?;
            tokenizer.with_padding(Some(PaddingParams {
                strategy: PaddingStrategy::BatchLongest,
                ..Default::default()
            }));

            let vb =
                unsafe { VarBuilder::from_mmaped_safetensors(&[weights_path], DTYPE, &device)? };
            let model = BertModel::load(vb, &bert_config)?;

            Ok(Self {
                model,
                tokenizer,
                device,
            })
        }

        /// Embed a batch of texts → one normalized vector (384 dim) per text.
        pub fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }
            let encodings = self
                .tokenizer
                .encode_batch(texts.to_vec(), true)
                .map_err(E::msg)?;

            let mut ids = Vec::with_capacity(encodings.len());
            let mut masks = Vec::with_capacity(encodings.len());
            for enc in &encodings {
                ids.push(Tensor::new(enc.get_ids(), &self.device)?);
                masks.push(Tensor::new(enc.get_attention_mask(), &self.device)?);
            }

            let input_ids = Tensor::stack(&ids, 0)?; // (batch, seq)
            let attention_mask = Tensor::stack(&masks, 0)?; // (batch, seq)
            let token_type_ids = input_ids.zeros_like()?;

            let embeddings =
                self.model
                    .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;

            // Masked mean pooling: ignore padding tokens so the embedding is correct.
            let mask = attention_mask.to_dtype(DTYPE)?.unsqueeze(2)?; // (b, seq, 1)
            let sum_mask = mask.sum(1)?; // (b, 1)
            let summed = embeddings.broadcast_mul(&mask)?.sum(1)?; // (b, 384)
            let mean = summed.broadcast_div(&sum_mask)?;

            // L2 normalize → cosine = dot product.
            let normalized = mean.broadcast_div(&mean.sqr()?.sum_keepdim(1)?.sqrt()?)?;
            Ok(normalized.to_vec2::<f32>()?)
        }
    }

    /// Global embedder (lazy): built on first use, then cached.
    /// `Mutex<Option<...>>` because `BertModel::forward` needs exclusive context
    /// and loading the model is expensive (done only once).
    static EMBEDDER: Mutex<Option<std::sync::Arc<Embedder>>> = Mutex::new(None);

    /// Get the global embedder, loading it if not already present.
    fn embedder() -> anyhow::Result<std::sync::Arc<Embedder>> {
        let mut guard = EMBEDDER.lock().unwrap();
        if let Some(e) = guard.as_ref() {
            return Ok(e.clone());
        }
        let e = std::sync::Arc::new(Embedder::load()?);
        *guard = Some(e.clone());
        Ok(e)
    }

    /// Ensure the project index is up to date with the list of memories (incremental):
    /// embed only new/changed memories, drop deleted ones, and save to disk.
    /// Returns a ready-to-use index.
    pub(super) fn ensure_index(
        config: &Config,
        project: &str,
        memories: &[Memory],
    ) -> anyhow::Result<EmbeddingIndex> {
        let mut index = EmbeddingIndex::load(config, project);

        // Determine which memories need embedding (new or with a changed hash).
        let mut to_embed: Vec<(String, String)> = Vec::new(); // (slug, text)
        let mut wanted: std::collections::BTreeSet<String> = Default::default();
        for m in memories {
            let slug = crate::project::slugify(&m.front.name);
            wanted.insert(slug.clone());
            let text = memory_text(m);
            let h = content_hash(&text);
            match index.entries.get(&slug) {
                Some(e) if e.hash == h => {} // still fresh
                _ => to_embed.push((slug, text)),
            }
        }

        // Drop entries for memories that no longer exist.
        index.entries.retain(|slug, _| wanted.contains(slug));

        if !to_embed.is_empty() {
            let emb = embedder()?;
            let texts: Vec<String> = to_embed.iter().map(|(_, t)| t.clone()).collect();
            let vectors = emb.embed(&texts)?;
            for ((slug, text), vector) in to_embed.into_iter().zip(vectors) {
                index.entries.insert(
                    slug,
                    IndexEntry {
                        hash: content_hash(&text),
                        vector,
                    },
                );
            }
            index.save(config, project)?;
        }

        Ok(index)
    }

    /// Embed a single query into a normalized vector.
    pub(super) fn embed_query(query: &str) -> anyhow::Result<Vec<f32>> {
        let emb = embedder()?;
        let mut v = emb.embed(&[query.to_string()])?;
        v.pop()
            .ok_or_else(|| anyhow::anyhow!("empty query embedding"))
    }
}

// ============================ Public module API ============================

/// The embedding vector of each memory (slug → normalized vector), when available.
///
/// Returns `Some(map)` only when the build uses the `semantic` feature AND the
/// index is successfully built/read. Returns `None` when the feature is disabled —
/// the caller (suggest/cluster) then falls back to the non-embedding method.
/// Never errors: any failure is treated as `None` so existing features keep working.
#[cfg(feature = "semantic")]
pub fn vectors_for(
    config: &Config,
    project: &str,
    memories: &[Memory],
) -> Option<std::collections::HashMap<String, Vec<f32>>> {
    let index = backend::ensure_index(config, project, memories).ok()?;
    let map = index
        .entries
        .iter()
        .map(|(slug, e)| (slug.clone(), e.vector.clone()))
        .collect();
    Some(map)
}

/// Variant without the feature: always `None` → the caller uses the old method.
#[cfg(not(feature = "semantic"))]
pub fn vectors_for(
    _config: &Config,
    _project: &str,
    _memories: &[Memory],
) -> Option<std::collections::HashMap<String, Vec<f32>>> {
    None
}

/// Cosine similarity of two normalized vectors (= dot product), public so it can
/// be used by the suggest/cluster modules. Safe when lengths differ (takes the shortest).
pub fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Semantic search: return the memories most relevant to `query`.
///
/// If the `semantic` feature is disabled, returns an error explaining how to
/// enable it.
#[cfg(feature = "semantic")]
pub fn semantic_search(
    config: &Config,
    project: &str,
    query: &str,
    top: usize,
    memories: &[Memory],
) -> anyhow::Result<Vec<SemanticHit>> {
    use std::collections::HashMap;

    let index = backend::ensure_index(config, project, memories)?;
    let qvec = backend::embed_query(query)?;

    // Map slug → description to enrich the results.
    let desc: HashMap<String, String> = memories
        .iter()
        .map(|m| {
            (
                crate::project::slugify(&m.front.name),
                m.front.description.clone(),
            )
        })
        .collect();

    let mut hits: Vec<SemanticHit> = index
        .entries
        .iter()
        .map(|(slug, e)| SemanticHit {
            name: slug.clone(),
            description: desc.get(slug).cloned().unwrap_or_default(),
            score: cosine(&qvec, &e.vector),
        })
        .collect();

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(top);
    Ok(hits)
}

/// Variant without the feature: explain how to enable it.
#[cfg(not(feature = "semantic"))]
pub fn semantic_search(
    _config: &Config,
    _project: &str,
    _query: &str,
    _top: usize,
    _memories: &[Memory],
) -> anyhow::Result<Vec<SemanticHit>> {
    anyhow::bail!(
        "Semantic search is not available in this build. Rebuild with \
         `cargo build --release --features semantic` (this downloads a ~90MB model \
         on the first run, then works offline)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_content() {
        assert_ne!(content_hash("a"), content_hash("b"));
        assert_eq!(content_hash("sama"), content_hash("sama"));
    }

    #[test]
    fn cosine_of_identical_normalized_is_one() {
        // a simple normalized vector.
        let v = vec![0.6, 0.8];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        assert!((cosine(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
    }

    #[cfg(not(feature = "semantic"))]
    #[test]
    fn search_without_feature_errors() {
        let cfg = Config {
            vault_path: std::env::temp_dir(),
            memory_root: "memory".into(),
            docs_root: "docs".into(),
            default_project: None,
        };
        let res = semantic_search(&cfg, "p", "q", 5, &[]);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("--features semantic"));
    }
}
