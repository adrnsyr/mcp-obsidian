//! Analysis of the inter-memory link graph: wikilink extraction, backlinks
//! (derived), broken link & orphan detection.
//!
//! A memory's outgoing links come from TWO sources:
//! 1. the `links` field in the frontmatter (explicit/structured relations), and
//! 2. `[[wikilink]]`s written in the body.
//!
//! Backlinks are NEVER stored to a file — they are always computed from the
//! graph for consistency (this is Obsidian's native approach). When A links to
//! B, B is automatically "linked by A" without touching B's file.

use crate::memory::Memory;
use crate::project::slugify;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Extract `[[...]]` wikilink targets from a body text.
///
/// Supports the Obsidian forms: `[[name]]`, `[[name|alias]]`, `[[name#heading]]`.
/// Only the `name` part is taken (before `|` or `#`), then slugified.
/// Embeds `![[...]]` are also captured (the `[[` bracket still matches).
pub fn extract_wikilinks(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            // find the closing "]]"
            if let Some(rel_end) = body[i + 2..].find("]]") {
                let inner = &body[i + 2..i + 2 + rel_end];
                // take the part before '|' (alias) and '#' (heading)
                let target = inner.split(['|', '#']).next().unwrap_or("").trim();
                let slug = slugify(target);
                if !slug.is_empty() {
                    out.push(slug);
                }
                i = i + 2 + rel_end + 2; // jump to just after "]]"
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The combined outgoing links of a memory (`links` field ∪ wikilinks in the
/// body), slugified, unique, and not pointing to itself. Sorted.
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

/// The link graph of a single project.
pub struct LinkGraph {
    /// All memory slugs that actually exist.
    pub existing: BTreeSet<String>,
    /// slug -> outgoing links (already filtered; may contain non-existent targets).
    pub forward: BTreeMap<String, Vec<String>>,
    /// slug -> list of memories linking to it (backlinks, derived).
    pub backward: BTreeMap<String, Vec<String>>,
}

impl LinkGraph {
    /// Build the graph from all of a project's memories.
    pub fn build(memories: &[Memory]) -> Self {
        let existing: BTreeSet<String> = memories.iter().map(|m| slugify(&m.front.name)).collect();

        let mut forward: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut backward: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for m in memories {
            let from = slugify(&m.front.name);
            let outs = outgoing_links(m);
            for to in &outs {
                // a backlink is only meaningful when the target exists.
                if existing.contains(to) {
                    backward.entry(to.clone()).or_default().push(from.clone());
                }
            }
            forward.insert(from, outs);
        }

        // keep it deterministic.
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

    /// Backlinks for a single slug (empty when there are none).
    pub fn backlinks_of(&self, slug: &str) -> Vec<String> {
        self.backward.get(slug).cloned().unwrap_or_default()
    }
}

/// A single broken link: memory `from` points to a `to` that does not exist.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BrokenLink {
    pub from: String,
    pub to: String,
    /// Filled in by the caller (server) when the target actually DOES exist in
    /// another project — distinguishing "wrong scope / needs rename" from
    /// "genuinely missing".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub also_in_project: Option<String>,
}

/// Health report for a project's memory graph.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub total: usize,
    /// Links to non-existent memories (in the body or the links field).
    pub broken_links: Vec<BrokenLink>,
    /// Memories with neither outgoing nor incoming links (isolated from the graph).
    pub orphans: Vec<String>,
    /// Memories that appear to be stubs/placeholders (need filling in).
    pub stubs: Vec<String>,
    /// Memories without a description.
    pub no_description: Vec<String>,
    /// Memories without tags.
    pub no_tags: Vec<String>,
}

/// Whether a memory appears to be an empty stub/placeholder.
fn is_stub(m: &Memory) -> bool {
    if m.front.kind.eq_ignore_ascii_case("stub") {
        return true;
    }
    if m.front.tags.iter().any(|t| slugify(t) == "stub") {
        return true;
    }
    let upper = m.body.to_uppercase();
    upper.contains("TO BE FILLED IN") || upper.contains("⚠️ STUB")
}

/// Outgoing link slugs of `mem` that point to memories NOT present in `existing`.
/// Used to warn about dangling links when writing a memory.
pub fn missing_targets(mem: &Memory, existing: &BTreeSet<String>) -> Vec<String> {
    outgoing_links(mem)
        .into_iter()
        .filter(|s| !existing.contains(s))
        .collect()
}

/// Rewrite a body: every `[[target...]]` whose slug == `old_slug` has its target
/// replaced with `new_slug` (alias `|...` & heading `#...` are preserved).
/// Used by `memory_rename` to update wikilinks in referring memories.
pub fn rewrite_wikilink_target(body: &str, old_slug: &str, new_slug: &str) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    let mut last = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(rel_end) = body[i + 2..].find("]]") {
                let inner_start = i + 2;
                let inner_end = i + 2 + rel_end;
                let inner = &body[inner_start..inner_end];
                let suffix_pos = inner.find(['|', '#']);
                let target = match suffix_pos {
                    Some(p) => &inner[..p],
                    None => inner,
                };
                if slugify(target) == old_slug {
                    out.push_str(&body[last..inner_start]);
                    out.push_str(new_slug);
                    if let Some(p) = suffix_pos {
                        out.push_str(&inner[p..]);
                    }
                    last = inner_end;
                }
                i = inner_end + 2;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&body[last..]);
    out
}

/// Check the health of the graph: broken links + orphans.
pub fn doctor(memories: &[Memory]) -> DoctorReport {
    let graph = LinkGraph::build(memories);

    let mut broken_links = Vec::new();
    for (from, outs) in &graph.forward {
        for to in outs {
            if !graph.existing.contains(to) {
                broken_links.push(BrokenLink {
                    from: from.clone(),
                    to: to.clone(),
                    also_in_project: None,
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

    // Metadata hygiene + stub detection (only needs the list of memories).
    let mut stubs = Vec::new();
    let mut no_description = Vec::new();
    let mut no_tags = Vec::new();
    for m in memories {
        let slug = slugify(&m.front.name);
        if is_stub(m) {
            stubs.push(slug.clone());
        }
        if m.front.description.trim().is_empty() {
            no_description.push(slug.clone());
        }
        if m.front.tags.is_empty() {
            no_tags.push(slug.clone());
        }
    }
    stubs.sort();
    no_description.sort();
    no_tags.sort();

    DoctorReport {
        total: memories.len(),
        broken_links,
        orphans,
        stubs,
        no_description,
        no_tags,
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
            "See [[Auth Flow]], [[deploy|the deploy]], [[notes#bab-1]] and ![[gambar]].",
        );
        assert_eq!(got, vec!["auth-flow", "deploy", "notes", "gambar"]);
    }

    #[test]
    fn outgoing_merges_links_and_body_without_self() {
        let m = mem("a", "link to [[b]] and [[a]] itself", &["c", "b"]);
        // union of {b, c} from the field + {b} from the body, without 'a'
        assert_eq!(outgoing_links(&m), vec!["b", "c"]);
    }

    #[test]
    fn backlinks_are_derived_both_directions() {
        let mems = vec![
            mem("a", "to [[b]]", &[]),
            mem("b", "links to no one", &["c"]),
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
            mem("a", "to [[b]] and [[hantu]]", &[]), // hantu does not exist
            mem("b", "", &[]),
            mem("sendirian", "no relations", &[]),
        ];
        let rep = doctor(&mems);
        assert_eq!(rep.total, 3);
        assert_eq!(
            rep.broken_links,
            vec![BrokenLink {
                from: "a".into(),
                to: "hantu".into(),
                also_in_project: None,
            }]
        );
        assert_eq!(rep.orphans, vec!["sendirian"]);
        // b is not an orphan (linked by a), a is not an orphan (has outgoing)
    }

    #[test]
    fn missing_targets_only_dangling() {
        let mems = [
            mem("a", "to [[b]] and [[hantu]]", &["c"]),
            mem("b", "", &[]),
        ];
        let existing: BTreeSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        // 'b' exists, 'c' & 'hantu' do not.
        assert_eq!(missing_targets(&mems[0], &existing), vec!["c", "hantu"]);
    }

    #[test]
    fn rewrite_wikilink_keeps_alias_and_heading() {
        let body = "See [[Old Name]], [[old-name|alias]], [[old-name#bab]] and [[lain]].";
        let got = rewrite_wikilink_target(body, "old-name", "baru");
        assert_eq!(
            got,
            "See [[baru]], [[baru|alias]], [[baru#bab]] and [[lain]]."
        );
    }

    #[test]
    fn doctor_flags_stub_and_missing_metadata() {
        let mut s = mem("stub-note", "## ⚠️ STUB — TO BE FILLED IN\nlater", &["x"]);
        s.front.description = "  ".into();
        s.front.tags = vec![];
        let mut ok = mem("ok-note", "full content", &["stub-note"]);
        ok.front.description = "present".into();
        ok.front.tags = vec!["t".into()];
        let rep = doctor(&[s, ok]);
        assert_eq!(rep.stubs, vec!["stub-note"]);
        assert_eq!(rep.no_description, vec!["stub-note"]);
        assert_eq!(rep.no_tags, vec!["stub-note"]);
    }
}
