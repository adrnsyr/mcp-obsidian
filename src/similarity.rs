//! "Relasi pintar": menyarankan link antar-memori berdasarkan kemiripan.
//!
//! Skor kemiripan dua memori = gabungan dari:
//! - **kemiripan tag**  : Jaccard pada himpunan tag (`|A∩B| / |A∪B|`),
//! - **kemiripan isi**  : cosine similarity pada vektor TF-IDF dari teks
//!   (`name + description + body`).
//!
//! `skor = TAG_WEIGHT * tag + CONTENT_WEIGHT * isi`.
//!
//! Saran yang sudah tercatat di field `links` memori sumber tidak diusulkan lagi.

use crate::memory::Memory;
use crate::project::slugify;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Bobot komponen tag pada skor akhir.
const TAG_WEIGHT: f64 = 0.6;
/// Bobot komponen isi (TF-IDF cosine) pada skor akhir.
const CONTENT_WEIGHT: f64 = 0.4;

/// Berapa banyak saran yang dikembalikan per memori secara default.
pub const DEFAULT_TOP_N: usize = 5;
/// Skor minimum agar sebuah pasangan dianggap "mirip".
pub const DEFAULT_THRESHOLD: f64 = 0.05;

/// Satu saran relasi untuk sebuah memori sumber.
#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    /// Slug memori yang disarankan untuk ditautkan.
    pub name: String,
    /// Skor kemiripan gabungan (0.0–1.0), dibulatkan saat serialisasi.
    pub score: f64,
    /// Tag yang dimiliki bersama (alasan saran).
    pub shared_tags: Vec<String>,
    /// Term (kata kunci) paling kuat yang dimiliki bersama (alasan saran).
    pub shared_terms: Vec<String>,
}

/// Saran untuk seluruh memori dalam project: `(nama_sumber, daftar_saran)`.
pub type SuggestionMap = Vec<(String, Vec<Suggestion>)>;

/// Dokumen yang sudah diproses: vektor TF-IDF + himpunan tag + link eksplisit.
struct Doc {
    name: String,
    tags: HashSet<String>,
    /// term -> bobot TF-IDF
    vec: HashMap<String, f64>,
    /// norma (panjang) vektor, dipra-hitung untuk cosine.
    norm: f64,
    /// slug yang sudah ditautkan secara eksplisit (di-skip dari saran).
    explicit_links: HashSet<String>,
}

/// Stopword umum (Indonesia + Inggris) yang dibuang sebelum skoring isi.
const STOPWORDS: &[&str] = &[
    // Indonesia
    "yang", "dan", "di", "ke", "dari", "untuk", "pada", "dengan", "atau", "ini", "itu", "adalah",
    "akan", "tidak", "bisa", "ada", "juga", "agar", "saat", "oleh", "dalam", "karena", "sebagai",
    "lalu", "bila", "saja", "per", "tiap", "kita", "kami", "saya", "anda", "dia", "mereka", "nya",
    "satu", "dua", // Inggris
    "the", "and", "for", "with", "this", "that", "are", "was", "from", "into", "not", "but", "can",
    "has", "have", "will", "you", "use", "via", "per", "all", "any", "its", "out", "set", "see",
    "one", "two",
];

fn is_stopword(w: &str) -> bool {
    STOPWORDS.contains(&w)
}

/// Pecah teks menjadi token bermakna: lowercase, hanya alfanumerik, panjang ≥ 3,
/// bukan stopword.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.chars().count() >= 3 && !is_stopword(w))
        .collect()
}

/// Pra-proses semua memori menjadi `Doc` (hitung TF-IDF sekali untuk seluruh
/// korpus project).
fn build_docs(memories: &[Memory]) -> Vec<Doc> {
    let n = memories.len() as f64;

    // 1) term frequency per dokumen.
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

    // 2) document frequency tiap term (berapa dokumen memuatnya).
    let mut df: HashMap<String, f64> = HashMap::new();
    for tf in &tfs {
        for term in tf.keys() {
            *df.entry(term.clone()).or_insert(0.0) += 1.0;
        }
    }

    // 3) vektor TF-IDF + norma (IDF smoothed: ln((N+1)/(df+1)) + 1).
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

/// Cosine similarity antar dua vektor TF-IDF.
fn cosine(a: &Doc, b: &Doc) -> f64 {
    if a.norm == 0.0 || b.norm == 0.0 {
        return 0.0;
    }
    // Iterasi map yang lebih kecil demi efisiensi.
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

/// Jaccard similarity antar dua himpunan tag.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Term bersama dengan bobot gabungan tertinggi (untuk menjelaskan saran).
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

/// Peta embedding opsional: slug → vektor ternormalisasi. Bila ada, komponen
/// "isi" pada skor saran memakai cosine embedding (by makna) alih-alih TF-IDF.
pub type Embeddings = std::collections::HashMap<String, Vec<f32>>;

/// Kemiripan isi dua dokumen: pakai embedding bila keduanya punya vektor,
/// selain itu fallback ke cosine TF-IDF.
fn content_similarity(t: &Doc, d: &Doc, emb: Option<&Embeddings>) -> f64 {
    if let Some(map) = emb {
        if let (Some(vt), Some(vd)) = (map.get(&t.name), map.get(&d.name)) {
            // Vektor sudah L2-normalized → cosine = dot product.
            return crate::embed::cosine_sim(vt, vd) as f64;
        }
    }
    cosine(t, d)
}

/// Hitung saran untuk dokumen index `ti` terhadap dokumen lain.
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

/// Seperti [`suggest_all_ext`] tapi untuk satu memori (berdasarkan slug);
/// bila `emb` diberikan, komponen isi memakai
/// kemiripan embedding (by makna) alih-alih TF-IDF.
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

/// Saran relasi untuk semua memori dalam project (mengabaikan yang tanpa saran).
/// Memakai TF-IDF untuk komponen isi.
pub fn suggest_all(memories: &[Memory], top_n: usize, threshold: f64) -> SuggestionMap {
    suggest_all_ext(memories, top_n, threshold, None)
}

/// Seperti [`suggest_all`], tapi bila `emb` diberikan, komponen isi memakai
/// kemiripan embedding (by makna) alih-alih TF-IDF.
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
                "autentikasi jwt",
                "token jwt untuk login pengguna",
                &["auth"],
                &[],
            ),
            mem(
                "login-page",
                "halaman login",
                "form login mengirim token jwt",
                &["auth"],
                &[],
            ),
            mem(
                "ui-theme",
                "warna tema",
                "palet warna gelap dan terang",
                &["design"],
                &[],
            ),
        ];
        let s = suggest_for_ext("auth-flow", &mems, 5, 0.0, None);
        assert_eq!(s.first().map(|x| x.name.as_str()), Some("login-page"));
        // login-page (tag+isi sama) harus mengungguli ui-theme.
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
            mem("a", "soal jwt", "token jwt", &["auth"], &["b"]),
            mem("b", "soal jwt juga", "token jwt", &["auth"], &[]),
        ];
        let s = suggest_for_ext("a", &mems, 5, 0.0, None);
        assert!(
            s.iter().all(|x| x.name != "b"),
            "b sudah ditautkan, tak boleh disarankan"
        );
    }

    #[test]
    fn threshold_filters_weak_matches() {
        let mems = vec![
            mem("a", "kucing", "kucing oranye lucu", &["hewan"], &[]),
            mem("b", "kompiler", "optimisasi kode rust", &["dev"], &[]),
        ];
        let s = suggest_for_ext("a", &mems, 5, 0.5, None);
        assert!(
            s.is_empty(),
            "dua memori tak terkait tak boleh lolos threshold tinggi"
        );
    }
}
