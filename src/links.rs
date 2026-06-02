//! Analisis graf tautan antar-memori: ekstraksi wikilink, backlink (derived),
//! deteksi broken link & orphan.
//!
//! Tautan keluar (outgoing) satu memori berasal dari DUA sumber:
//! 1. field `links` di frontmatter (relasi eksplisit/terstruktur), dan
//! 2. `[[wikilink]]` yang ditulis di dalam body.
//!
//! Backlink TIDAK pernah disimpan ke file — selalu dihitung dari graf agar
//! konsisten (ini cara native Obsidian). Saat A menaut B, B otomatis "ditaut
//! oleh A" tanpa menyentuh file B.

use crate::memory::Memory;
use crate::project::slugify;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Ekstrak target wikilink `[[...]]` dari sebuah teks body.
///
/// Mendukung bentuk Obsidian: `[[nama]]`, `[[nama|alias]]`, `[[nama#heading]]`.
/// Hanya bagian `nama` yang diambil (sebelum `|` atau `#`), lalu di-slugify.
/// Embed `![[...]]` juga ikut tertangkap (kurung `[[` tetap cocok).
pub fn extract_wikilinks(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            // cari penutup "]]"
            if let Some(rel_end) = body[i + 2..].find("]]") {
                let inner = &body[i + 2..i + 2 + rel_end];
                // ambil sebelum '|' (alias) dan '#' (heading)
                let target = inner.split(['|', '#']).next().unwrap_or("").trim();
                let slug = slugify(target);
                if !slug.is_empty() {
                    out.push(slug);
                }
                i = i + 2 + rel_end + 2; // lompat ke setelah "]]"
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Gabungan tautan keluar sebuah memori (field `links` ∪ wikilink di body),
/// sudah di-slugify, unik, dan tidak menunjuk diri sendiri. Terurut.
pub fn outgoing_links(mem: &Memory) -> Vec<String> {
    let self_slug = slugify(&mem.front.name);
    let mut set: BTreeSet<String> = BTreeSet::new();
    for l in &mem.front.links {
        let s = slugify(l);
        if !s.is_empty() && s != self_slug {
            set.insert(s);
        }
    }
    for l in extract_wikilinks(&mem.body) {
        if l != self_slug {
            set.insert(l);
        }
    }
    set.into_iter().collect()
}

/// Graf tautan satu project.
pub struct LinkGraph {
    /// Semua slug memori yang benar-benar ada.
    pub existing: BTreeSet<String>,
    /// slug -> tautan keluar (sudah difilter; bisa memuat target tak-ada).
    pub forward: BTreeMap<String, Vec<String>>,
    /// slug -> daftar memori yang menautnya (backlink, derived).
    pub backward: BTreeMap<String, Vec<String>>,
}

impl LinkGraph {
    /// Bangun graf dari seluruh memori sebuah project.
    pub fn build(memories: &[Memory]) -> Self {
        let existing: BTreeSet<String> = memories.iter().map(|m| slugify(&m.front.name)).collect();

        let mut forward: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut backward: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for m in memories {
            let from = slugify(&m.front.name);
            let outs = outgoing_links(m);
            for to in &outs {
                // backlink hanya bermakna bila target ada.
                if existing.contains(to) {
                    backward.entry(to.clone()).or_default().push(from.clone());
                }
            }
            forward.insert(from, outs);
        }

        // jaga determinisme.
        for v in backward.values_mut() {
            v.sort();
            v.dedup();
        }

        Self {
            existing,
            forward,
            backward,
        }
    }

    /// Backlink untuk satu slug (kosong bila tidak ada).
    pub fn backlinks_of(&self, slug: &str) -> Vec<String> {
        self.backward.get(slug).cloned().unwrap_or_default()
    }
}

/// Satu broken link: memori `from` menunjuk `to` yang tidak ada.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BrokenLink {
    pub from: String,
    pub to: String,
}

/// Laporan kesehatan graf memori sebuah project.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub total: usize,
    /// Tautan ke memori yang tidak ada (di body atau field links).
    pub broken_links: Vec<BrokenLink>,
    /// Memori tanpa tautan keluar maupun masuk (terisolasi dari graf).
    pub orphans: Vec<String>,
}

/// Periksa kesehatan graf: broken link + orphan.
pub fn doctor(memories: &[Memory]) -> DoctorReport {
    let graph = LinkGraph::build(memories);

    let mut broken_links = Vec::new();
    for (from, outs) in &graph.forward {
        for to in outs {
            if !graph.existing.contains(to) {
                broken_links.push(BrokenLink {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
    }
    broken_links.sort_by(|a, b| a.from.cmp(&b.from).then(a.to.cmp(&b.to)));

    let mut orphans: Vec<String> = graph
        .existing
        .iter()
        .filter(|slug| {
            let has_out = graph
                .forward
                .get(*slug)
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let has_in = graph
                .backward
                .get(*slug)
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            !has_out && !has_in
        })
        .cloned()
        .collect();
    orphans.sort();

    DoctorReport {
        total: memories.len(),
        broken_links,
        orphans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Frontmatter, Memory};

    fn mem(name: &str, body: &str, links: &[&str]) -> Memory {
        Memory {
            front: Frontmatter {
                name: name.into(),
                description: "d".into(),
                tags: vec![],
                kind: "note".into(),
                links: links.iter().map(|s| s.to_string()).collect(),
                created: "2026".into(),
                updated: "2026".into(),
            },
            body: body.into(),
        }
    }

    #[test]
    fn extracts_wikilink_variants() {
        let got = extract_wikilinks(
            "Lihat [[Auth Flow]], [[deploy|si deploy]], [[notes#bab-1]] dan ![[gambar]].",
        );
        assert_eq!(got, vec!["auth-flow", "deploy", "notes", "gambar"]);
    }

    #[test]
    fn outgoing_merges_links_and_body_without_self() {
        let m = mem("a", "taut ke [[b]] dan [[a]] sendiri", &["c", "b"]);
        // gabungan {b, c} dari field + {b} dari body, tanpa 'a'
        assert_eq!(outgoing_links(&m), vec!["b", "c"]);
    }

    #[test]
    fn backlinks_are_derived_both_directions() {
        let mems = vec![
            mem("a", "ke [[b]]", &[]),
            mem("b", "tak menaut siapa pun", &["c"]),
            mem("c", "", &[]),
        ];
        let g = LinkGraph::build(&mems);
        assert_eq!(g.backlinks_of("b"), vec!["a"]); // a -> b
        assert_eq!(g.backlinks_of("c"), vec!["b"]); // b -> c (via field links)
        assert!(g.backlinks_of("a").is_empty());
    }

    #[test]
    fn doctor_finds_broken_and_orphans() {
        let mems = vec![
            mem("a", "ke [[b]] dan [[hantu]]", &[]), // hantu tak ada
            mem("b", "", &[]),
            mem("sendirian", "tak ada relasi", &[]),
        ];
        let rep = doctor(&mems);
        assert_eq!(rep.total, 3);
        assert_eq!(
            rep.broken_links,
            vec![BrokenLink {
                from: "a".into(),
                to: "hantu".into()
            }]
        );
        assert_eq!(rep.orphans, vec!["sendirian"]);
        // b tidak orphan (ditaut oleh a), a tidak orphan (punya outgoing)
    }
}
