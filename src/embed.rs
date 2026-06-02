//! Pencarian semantik berbasis embedding lokal (pure-Rust via candle).
//!
//! Fitur ini **opt-in**: hanya terkompilasi penuh bila crate dibangun dengan
//! `--features semantic`. Tanpa feature itu, `semantic_search` mengembalikan
//! error yang menjelaskan cara mengaktifkannya — sehingga tool tetap terdaftar
//! di server apa pun build-nya.
//!
//! Desain:
//! - Model: `sentence-transformers/all-MiniLM-L6-v2` (384 dim), diunduh sekali
//!   ke cache HuggingFace lalu dipakai offline.
//! - Embedding tiap memori (teks `name + description + body`) di-cache ke file
//!   sidecar `memory/<project>/.embeddings.json`. Hanya memori yang berubah
//!   (deteksi via hash isi) yang di-embed ulang.
//! - Skor relevansi = cosine similarity. Karena vektor sudah L2-normalized,
//!   cosine = dot product.

use crate::config::Config;
use crate::memory::Memory;
use serde::{Deserialize, Serialize};

/// Identitas model embedding (untuk invalidasi index bila model berganti).
/// Model **multilingual** (50+ bahasa, termasuk Indonesia) — penting karena
/// memori bisa berbahasa non-Inggris. Arsitektur BERT, 384 dim, tanpa prefix
/// khusus, jadi drop-in untuk pipeline di bawah. Mengganti nilai ini otomatis
/// meng-invalidasi semua index sidecar (lihat `EmbeddingIndex::load`).
pub const MODEL_ID: &str = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2";
/// Dimensi vektor embedding model di atas.
pub const EMBED_DIM: usize = 384;
/// Nama file sidecar index per project.
const INDEX_FILE: &str = ".embeddings.json";

/// Satu hasil pencarian semantik.
#[derive(Debug, Clone, Serialize)]
pub struct SemanticHit {
    pub name: String,
    pub description: String,
    /// Cosine similarity terhadap query (−1.0..1.0; makin tinggi makin relevan).
    pub score: f32,
}

// ===================== Index sidecar (selalu tersedia) =====================

/// Entri index: hash isi (untuk deteksi perubahan) + vektor embedding.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexEntry {
    hash: u64,
    vector: Vec<f32>,
}

/// Index embedding satu project, disimpan sebagai file sidecar JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmbeddingIndex {
    /// Model yang dipakai; bila beda dengan `MODEL_ID` sekarang, index dibuang.
    model: String,
    /// Dimensi vektor; pengaman tambahan.
    dim: usize,
    /// slug -> entri.
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

    /// Muat index dari disk; kembalikan index kosong bila tidak ada / tak cocok
    /// (model atau dimensi berubah) / korup.
    fn load(config: &Config, project: &str) -> Self {
        let path = config.project_dir(project).join(INDEX_FILE);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::empty();
        };
        match serde_json::from_str::<EmbeddingIndex>(&raw) {
            Ok(idx) if idx.model == MODEL_ID && idx.dim == EMBED_DIM => idx,
            _ => Self::empty(), // model/dim berubah atau korup → buang
        }
    }

    fn save(&self, config: &Config, project: &str) -> anyhow::Result<()> {
        let path = config.project_dir(project).join(INDEX_FILE);
        std::fs::write(&path, serde_json::to_string(self)?)?;
        Ok(())
    }
}

/// Teks yang di-embed untuk sebuah memori.
fn memory_text(m: &Memory) -> String {
    format!(
        "{}\n{}\n{}",
        m.front.name.replace('-', " "),
        m.front.description,
        m.body
    )
}

/// Hash isi memori (FNV-1a 64-bit) untuk deteksi perubahan — cukup & tanpa dep.
fn content_hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Cosine similarity dua vektor ternormalisasi (= dot product). Aman bila
/// panjang beda (ambil minimum) walau idealnya selalu sama.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

// ============================ Jalur dengan feature ============================

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

    /// Pembungkus model BERT + tokenizer. Dibangun sekali, dipakai berulang.
    pub struct Embedder {
        model: BertModel,
        tokenizer: Tokenizer,
        device: Device,
    }

    impl Embedder {
        /// Muat model (blocking; unduh saat run pertama lalu pakai cache HF).
        pub fn load() -> anyhow::Result<Self> {
            let device = Device::Cpu;
            let repo = Repo::new(MODEL_ID.to_string(), RepoType::Model);
            let api = Api::new()
                .context("gagal inisialisasi HuggingFace API")?
                .repo(repo);

            let config_path = api.get("config.json").context("unduh config.json")?;
            let tokenizer_path = api.get("tokenizer.json").context("unduh tokenizer.json")?;
            let weights_path = api
                .get("model.safetensors")
                .context("unduh model.safetensors")?;

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

        /// Embed sekumpulan teks → satu vektor ternormalisasi (384 dim) per teks.
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

            // Masked mean pooling: abaikan token padding agar embedding benar.
            let mask = attention_mask.to_dtype(DTYPE)?.unsqueeze(2)?; // (b, seq, 1)
            let sum_mask = mask.sum(1)?; // (b, 1)
            let summed = embeddings.broadcast_mul(&mask)?.sum(1)?; // (b, 384)
            let mean = summed.broadcast_div(&sum_mask)?;

            // L2 normalize → cosine = dot product.
            let normalized = mean.broadcast_div(&mean.sqr()?.sum_keepdim(1)?.sqrt()?)?;
            Ok(normalized.to_vec2::<f32>()?)
        }
    }

    /// Embedder global (lazy): dibangun saat pertama dipakai, lalu di-cache.
    /// `Mutex<Option<...>>` karena `BertModel::forward` butuh konteks eksklusif
    /// & pemuatan model mahal (sekali saja).
    static EMBEDDER: Mutex<Option<std::sync::Arc<Embedder>>> = Mutex::new(None);

    /// Ambil embedder global, memuatnya bila belum ada.
    fn embedder() -> anyhow::Result<std::sync::Arc<Embedder>> {
        let mut guard = EMBEDDER.lock().unwrap();
        if let Some(e) = guard.as_ref() {
            return Ok(e.clone());
        }
        let e = std::sync::Arc::new(Embedder::load()?);
        *guard = Some(e.clone());
        Ok(e)
    }

    /// Pastikan index project mutakhir terhadap daftar memori (incremental):
    /// embed hanya memori baru/berubah, buang yang sudah dihapus, simpan ke disk.
    /// Mengembalikan index siap-pakai.
    pub(super) fn ensure_index(
        config: &Config,
        project: &str,
        memories: &[Memory],
    ) -> anyhow::Result<EmbeddingIndex> {
        let mut index = EmbeddingIndex::load(config, project);

        // Tentukan memori yang perlu di-embed (baru atau hash berubah).
        let mut to_embed: Vec<(String, String)> = Vec::new(); // (slug, text)
        let mut wanted: std::collections::BTreeSet<String> = Default::default();
        for m in memories {
            let slug = crate::project::slugify(&m.front.name);
            wanted.insert(slug.clone());
            let text = memory_text(m);
            let h = content_hash(&text);
            match index.entries.get(&slug) {
                Some(e) if e.hash == h => {} // masih segar
                _ => to_embed.push((slug, text)),
            }
        }

        // Buang entri memori yang sudah tidak ada.
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

    /// Embed satu query menjadi vektor ternormalisasi.
    pub(super) fn embed_query(query: &str) -> anyhow::Result<Vec<f32>> {
        let emb = embedder()?;
        let mut v = emb.embed(&[query.to_string()])?;
        v.pop()
            .ok_or_else(|| anyhow::anyhow!("embedding query kosong"))
    }
}

// ============================ API publik modul ============================

/// Vektor embedding tiap memori (slug → vektor ternormalisasi), bila tersedia.
///
/// Mengembalikan `Some(map)` hanya bila build memakai fitur `semantic` DAN
/// index berhasil dibangun/dibaca. Mengembalikan `None` bila fitur mati —
/// pemanggil (suggest/cluster) lalu fallback ke metode non-embedding.
/// Tidak pernah error: kegagalan apa pun diperlakukan sebagai `None` agar
/// fitur lama tetap berjalan.
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

/// Varian tanpa feature: selalu `None` → pemanggil pakai metode lama.
#[cfg(not(feature = "semantic"))]
pub fn vectors_for(
    _config: &Config,
    _project: &str,
    _memories: &[Memory],
) -> Option<std::collections::HashMap<String, Vec<f32>>> {
    None
}

/// Cosine similarity dua vektor ternormalisasi (= dot product), publik agar
/// dipakai modul suggest/cluster. Aman bila panjang beda (ambil yang terpendek).
pub fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Pencarian semantik: kembalikan memori paling relevan dengan `query`.
///
/// Bila feature `semantic` tidak aktif, mengembalikan error yang menjelaskan
/// cara mengaktifkannya.
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

    // Peta slug → deskripsi untuk melengkapi hasil.
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

/// Varian tanpa feature: jelaskan cara mengaktifkan.
#[cfg(not(feature = "semantic"))]
pub fn semantic_search(
    _config: &Config,
    _project: &str,
    _query: &str,
    _top: usize,
    _memories: &[Memory],
) -> anyhow::Result<Vec<SemanticHit>> {
    anyhow::bail!(
        "pencarian semantik tidak tersedia di build ini. Rebuild dengan \
         `cargo build --release --features semantic` (mengunduh model ~90MB \
         saat run pertama, lalu offline)."
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
        // vektor ternormalisasi sederhana.
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
            default_project: None,
        };
        let res = semantic_search(&cfg, "p", "q", 5, &[]);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("--features semantic"));
    }
}
