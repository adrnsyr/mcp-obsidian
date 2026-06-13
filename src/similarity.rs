//! "Smart relations": suggest links between memories based on similarity.
//!
//! The similarity score of two memories = a combination of:
//! - **tag similarity**     : Jaccard over the tag sets (`|A∩B| / |A∪B|`),
//! - **content similarity** : cosine similarity over the TF-IDF vectors of the
//!   text (`name + description + body`).
//!
//! `score = TAG_WEIGHT * tag + CONTENT_WEIGHT * content`.
//!
//! Suggestions already recorded in the source memory's `links` field are not
//! proposed again.

use crate::memory::Memory;
use crate::project::slugify;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Weight of the tag component in the final score.
const TAG_WEIGHT: f64 = 0.6;
/// Weight of the content component (TF-IDF cosine) in the final score.
const CONTENT_WEIGHT: f64 = 0.4;

/// How many suggestions are returned per memory by default.
pub const DEFAULT_TOP_N: usize = 5;
/// Minimum score for a pair to be considered "similar".
pub const DEFAULT_THRESHOLD: f64 = 0.05;

/// A single relation suggestion for a source memory.
#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    /// Slug of the memory suggested for linking.
    pub name: String,
    /// Combined similarity score (0.0–1.0), rounded on serialization.
    pub score: f64,
    /// Tags held in common (the reason for the suggestion).
    pub shared_tags: Vec<String>,
    /// The strongest terms (keywords) held in common (the reason for the suggestion).
    pub shared_terms: Vec<String>,
}

/// Suggestions for every memory in a project: `(source_name, suggestion_list)`.
pub type SuggestionMap = Vec<(String, Vec<Suggestion>)>;

/// A preprocessed document: TF-IDF vector + tag set + explicit links.
struct Doc {
    name: String,
    tags: HashSet<String>,
    /// term -> TF-IDF weight
    vec: HashMap<String, f64>,
    /// vector norm (length), precomputed for cosine.
    norm: f64,
    /// slugs already linked explicitly (skipped from suggestions).
    explicit_links: HashSet<String>,
}

/// Common stopwords (Indonesian + English) discarded before content scoring.
const STOPWORDS: &[&str] = &[
    // Indonesian
    "yang", "dan", "di", "ke", "dari", "untuk", "pada", "dengan", "atau", "ini", "itu", "adalah",
    "akan", "tidak", "bisa", "ada", "juga", "agar", "saat", "oleh", "dalam", "karena", "sebagai",
    "lalu", "bila", "saja", "per", "tiap", "kita", "kami", "saya", "anda", "dia", "mereka", "nya",
    "satu", "dua", // English
    "the", "and", "for", "with", "this", "that", "are", "was", "from", "into", "not", "but", "can",
    "has", "have", "will", "you", "use", "via", "per", "all", "any", "its", "out", "set", "see",
    "one", "two",
];

fn is_stopword(w: &str) -> bool {
    STOPWORDS.contains(&w)
}

/// Split text into meaningful tokens: lowercase, alphanumeric only, length ≥ 3,
/// not a stopword.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.chars().count() >= 3 && !is_stopword(w))
        .collect()
}

/// Preprocess all memories into `Doc`s (compute TF-IDF once for the entire
/// project corpus).
fn build_docs(memories: &[Memory]) -> Vec<Doc> {
    let n = memories.len() as f64;

    // 1) term frequency per document.
    let tfs: Vec<HashMap<String, f64>> = memories
        .iter()
        .map(|m| {
            let text = format!(
                "{} {} {}",
                m.front.name.replace('-', " "),
                m.front.description,
                m.body
            );
            let mut tf: HashMap<String, f64> = HashMap::new();
            for tok in tokenize(&text) {
                *tf.entry(tok).or_insert(0.0) += 1.0;
            }
            tf
        })
        .collect();

    // 2) document frequency of each term (how many documents contain it).
    let mut df: HashMap<String, f64> = HashMap::new();
    for tf in &tfs {
        for term in tf.keys() {
            *df.entry(term.clone()).or_insert(0.0) += 1.0;
        }
    }

    // 3) TF-IDF vector + norm (smoothed IDF: ln((N+1)/(df+1)) + 1).
    memories
        .iter()
        .zip(tfs)
        .map(|(m, tf)| {
            let mut vec = HashMap::with_capacity(tf.len());
            for (term, freq) in tf {
                let idf = ((n + 1.0) / (df[&term] + 1.0)).ln() + 1.0;
                vec.insert(term, freq * idf);
            }
            let norm = vec.values().map(|w| w * w).sum::<f64>().sqrt();
            Doc {
                name: m.front.name.clone(),
                tags: m.front.tags.iter().map(|t| slugify(t)).collect(),
                vec,
                norm,
                explicit_links: m.front.links.iter().map(|l| slugify(l)).collect(),
            }
        })
        .collect()
}

/// Cosine similarity between two TF-IDF vectors.
fn cosine(a: &Doc, b: &Doc) -> f64 {
    if a.norm == 0.0 || b.norm == 0.0 {
        return 0.0;
    }
    // Iterate the smaller map for efficiency.
    let (small, large) = if a.vec.len() <= b.vec.len() {
        (a, b)
    } else {
        (b, a)
    };
    let dot: f64 = small
        .vec
        .iter()
        .filter_map(|(t, w)| large.vec.get(t).map(|w2| w * w2))
        .sum();
    dot / (a.norm * b.norm)
}

/// Jaccard similarity between two tag sets.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Shared terms with the highest combined weight (to explain a suggestion).
fn top_shared_terms(a: &Doc, b: &Doc, limit: usize) -> Vec<String> {
    let mut shared: Vec<(String, f64)> = a
        .vec
        .iter()
        .filter_map(|(t, wa)| b.vec.get(t).map(|wb| (t.clone(), wa + wb)))
        .collect();
    shared.sort_by(|x, y| {
        y.1.partial_cmp(&x.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(x.0.cmp(&y.0))
    });
    shared.into_iter().take(limit).map(|(t, _)| t).collect()
}

/// Optional embedding map: slug → normalized vector. When present, the "content"
/// component of the suggestion score uses embedding cosine (by meaning) instead
/// of TF-IDF.
pub type Embeddings = std::collections::HashMap<String, Vec<f32>>;

/// Content similarity of two documents: use embeddings when both have a vector,
/// otherwise fall back to TF-IDF cosine.
fn content_similarity(t: &Doc, d: &Doc, emb: Option<&Embeddings>) -> f64 {
    if let Some(map) = emb {
        if let (Some(vt), Some(vd)) = (map.get(&t.name), map.get(&d.name)) {
            // Vectors are already L2-normalized → cosine = dot product.
            return crate::embed::cosine_sim(vt, vd) as f64;
        }
    }
    cosine(t, d)
}

/// Compute suggestions for the document at index `ti` against the other documents.
fn suggest_from_docs(
    docs: &[Doc],
    ti: usize,
    top_n: usize,
    threshold: f64,
    emb: Option<&Embeddings>,
) -> Vec<Suggestion> {
    let t = &docs[ti];
    let mut out: Vec<Suggestion> = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        if i == ti || t.explicit_links.contains(&d.name) {
            continue;
        }
        let tag_sim = jaccard(&t.tags, &d.tags);
        let content_sim = content_similarity(t, d, emb);
        let score = TAG_WEIGHT * tag_sim + CONTENT_WEIGHT * content_sim;
        if score < threshold {
            continue;
        }
        out.push(Suggestion {
            name: d.name.clone(),
            score,
            shared_tags: {
                let mut v: Vec<String> = t.tags.intersection(&d.tags).cloned().collect();
                v.sort();
                v
            },
            shared_terms: top_shared_terms(t, d, 5),
        });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.name.cmp(&b.name))
    });
    out.truncate(top_n);
    out
}

/// Like [`suggest_all_ext`] but for a single memory (by slug); when `emb` is
/// given, the content component uses embedding similarity (by meaning) instead
/// of TF-IDF.
pub fn suggest_for_ext(
    target: &str,
    memories: &[Memory],
    top_n: usize,
    threshold: f64,
    emb: Option<&Embeddings>,
) -> Vec<Suggestion> {
    let docs = build_docs(memories);
    match docs.iter().position(|d| d.name == target) {
        Some(ti) => suggest_from_docs(&docs, ti, top_n, threshold, emb),
        None => Vec::new(),
    }
}

/// Relation suggestions for every memory in a project (skipping those with no
/// suggestions). Uses TF-IDF for the content component.
pub fn suggest_all(memories: &[Memory], top_n: usize, threshold: f64) -> SuggestionMap {
    suggest_all_ext(memories, top_n, threshold, None)
}

/// Like [`suggest_all`], but when `emb` is given, the content component uses
/// embedding similarity (by meaning) instead of TF-IDF.
pub fn suggest_all_ext(
    memories: &[Memory],
    top_n: usize,
    threshold: f64,
    emb: Option<&Embeddings>,
) -> SuggestionMap {
    let docs = build_docs(memories);
    let mut result: SuggestionMap = Vec::new();
    for ti in 0..docs.len() {
        let s = suggest_from_docs(&docs, ti, top_n, threshold, emb);
        if !s.is_empty() {
            result.push((docs[ti].name.clone(), s));
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Frontmatter, Memory};

    fn mem(name: &str, desc: &str, body: &str, tags: &[&str], links: &[&str]) -> Memory {
        Memory {
            front: Frontmatter {
                name: name.into(),
                description: desc.into(),
                tags: tags.iter().map(|s| s.to_string()).collect(),
                kind: "note".into(),
                links: links.iter().map(|s| s.to_string()).collect(),
                created: "2026".into(),
                updated: "2026".into(),
            },
            body: body.into(),
        }
    }

    #[test]
    fn related_memories_score_higher_than_unrelated() {
        let mems = vec![
            mem(
                "auth-flow",
                "jwt authentication",
                "jwt token for user login",
                &["auth"],
                &[],
            ),
            mem(
                "login-page",
                "login page",
                "login form sends jwt token",
                &["auth"],
                &[],
            ),
            mem(
                "ui-theme",
                "theme colors",
                "dark and light color palette",
                &["design"],
                &[],
            ),
        ];
        let s = suggest_for_ext("auth-flow", &mems, 5, 0.0, None);
        assert_eq!(s.first().map(|x| x.name.as_str()), Some("login-page"));
        // login-page (same tag+content) should outrank ui-theme.
        let score_login = s.iter().find(|x| x.name == "login-page").unwrap().score;
        let score_theme = s
            .iter()
            .find(|x| x.name == "ui-theme")
            .map(|x| x.score)
            .unwrap_or(0.0);
        assert!(score_login > score_theme);
        assert!(s
            .iter()
            .find(|x| x.name == "login-page")
            .unwrap()
            .shared_tags
            .contains(&"auth".to_string()));
    }

    #[test]
    fn already_linked_is_not_suggested() {
        let mems = vec![
            mem("a", "about jwt", "jwt token", &["auth"], &["b"]),
            mem("b", "also about jwt", "jwt token", &["auth"], &[]),
        ];
        let s = suggest_for_ext("a", &mems, 5, 0.0, None);
        assert!(
            s.iter().all(|x| x.name != "b"),
            "b is already linked, must not be suggested"
        );
    }

    #[test]
    fn threshold_filters_weak_matches() {
        let mems = vec![
            mem("a", "cat", "cute orange cat", &["hewan"], &[]),
            mem("b", "compiler", "rust code optimization", &["dev"], &[]),
        ];
        let s = suggest_for_ext("a", &mems, 5, 0.5, None);
        assert!(
            s.is_empty(),
            "two unrelated memories must not pass a high threshold"
        );
    }
}
