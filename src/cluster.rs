//! Cluster memories into "themes" via **Louvain** community detection on the
//! (undirected) link graph.
//!
//! The graph is built from `LinkGraph`: every link A→B (the `links` field or a
//! `[[wikilink]]` in the body whose target exists) becomes an undirected edge
//! A↔B with weight 1 (weights accumulate when multiple links exist between the
//! same pair).
//!
//! Louvain maximizes **modularity** Q:
//! ```text
//! Q = (1 / 2m) * Σ_ij [ A_ij - (k_i * k_j) / 2m ] * δ(c_i, c_j)
//! ```
//! where `A_ij` is the edge weight, `k_i` the weighted degree of node i, `m` the
//! total edge weight, and `δ` = 1 when i & j share a community. This
//! implementation runs a single level (local-moving), which is sufficient for
//! small-to-medium memory graphs; nodes without edges become their own singleton
//! community.

use crate::links::LinkGraph;
use crate::memory::Memory;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// A single community/theme produced by clustering.
#[derive(Debug, Clone, Serialize)]
pub struct Cluster {
    /// Members (memory slugs), sorted.
    pub members: Vec<String>,
}

/// Clustering result for a project.
#[derive(Debug, Clone, Serialize)]
pub struct ClusterResult {
    /// Final modularity value (−0.5..1.0; higher means more sharply defined communities).
    pub modularity: f64,
    /// List of communities, sorted from most members to fewest.
    pub clusters: Vec<Cluster>,
}

/// Representation of a weighted undirected graph with nodes indexed `0..n`.
struct Graph {
    n: usize,
    /// adjacency: node -> [(neighbor, weight)]
    adj: Vec<Vec<(usize, f64)>>,
    /// weighted degree of each node (including 2× self-loop when present).
    degree: Vec<f64>,
    /// total edge weight `m` (Σ weights, self-loops counted once).
    total: f64,
}

impl Graph {
    /// Build an undirected graph from `LinkGraph`, optionally enriched with
    /// embedding similarity edges. Returns the graph + a sorted list of slugs
    /// (node index = position in this slice).
    ///
    /// `emb`: when `Some`, every pair of memories with cosine ≥ `sim_threshold`
    /// gets an additional edge weighted by its cosine. This makes memories that
    /// are **semantically similar** cluster together even when they do not link
    /// to each other.
    fn from_links(
        graph: &LinkGraph,
        emb: Option<&Embeddings>,
        sim_threshold: f64,
    ) -> (Self, Vec<String>) {
        // Nodes = all existing memories, sorted for determinism.
        let slugs: Vec<String> = graph.existing.iter().cloned().collect();
        let index: HashMap<&str, usize> = slugs
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        let n = slugs.len();

        // Accumulate undirected edge weights into a map keyed by (min,max).
        let mut weights: BTreeMap<(usize, usize), f64> = BTreeMap::new();
        for (from, outs) in &graph.forward {
            let Some(&u) = index.get(from.as_str()) else {
                continue;
            };
            for to in outs {
                let Some(&v) = index.get(to.as_str()) else {
                    continue; // link to a non-existent memory → ignored
                };
                if u == v {
                    continue; // no self-link
                }
                let key = (u.min(v), u.max(v));
                *weights.entry(key).or_insert(0.0) += 1.0;
            }
        }

        // Embedding similarity edges (when available): all pairs above the
        // threshold. O(n²) cosine — fine at per-project memory scale.
        if let Some(map) = emb {
            // a & b are used as tuple indices into `weights`; rewriting this as
            // an iterator would only obscure the intent.
            #[allow(clippy::needless_range_loop)]
            for a in 0..n {
                let Some(va) = map.get(&slugs[a]) else {
                    continue;
                };
                for b in (a + 1)..n {
                    let Some(vb) = map.get(&slugs[b]) else {
                        continue;
                    };
                    let sim = crate::embed::cosine_sim(va, vb) as f64;
                    if sim >= sim_threshold {
                        *weights.entry((a, b)).or_insert(0.0) += sim;
                    }
                }
            }
        }

        let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        let mut degree = vec![0.0; n];
        let mut total = 0.0;
        for (&(u, v), &w) in &weights {
            adj[u].push((v, w));
            adj[v].push((u, w));
            degree[u] += w;
            degree[v] += w;
            total += w;
        }

        (
            Self {
                n,
                adj,
                degree,
                total,
            },
            slugs,
        )
    }
}

/// Run Louvain (a single local-moving level) and return the community label per
/// node (`comm[i]` = community id of node i, not necessarily compact).
fn louvain_labels(g: &Graph) -> Vec<usize> {
    // Initially each node is its own community.
    let mut comm: Vec<usize> = (0..g.n).collect();
    // Σ_tot[c] = total weighted degree of all nodes in community c.
    let mut sigma_tot: Vec<f64> = g.degree.clone();

    if g.total == 0.0 {
        return comm; // no edges: everything is a singleton
    }
    let m2 = 2.0 * g.total; // 2m

    let mut improved = true;
    // bound the iterations so it always terminates despite numeric oscillation.
    let mut rounds = 0;
    while improved && rounds < 100 {
        improved = false;
        rounds += 1;

        for i in 0..g.n {
            let ci = comm[i];
            let ki = g.degree[i];

            // Weight from i to each neighboring community (including its current one).
            let mut w_to: HashMap<usize, f64> = HashMap::new();
            for &(j, w) in &g.adj[i] {
                *w_to.entry(comm[j]).or_insert(0.0) += w;
            }

            // Detach i from its community: subtract its degree contribution.
            sigma_tot[ci] -= ki;

            // Find the best community (largest modularity gain). Baseline = staying in ci.
            // ΔQ for moving to c ∝ w_to[c] - Σ_tot[c] * k_i / 2m.
            let mut best_comm = ci;
            let w_to_ci = w_to.get(&ci).copied().unwrap_or(0.0);
            let mut best_gain = w_to_ci - sigma_tot[ci] * ki / m2;

            for (&c, &w_ic) in &w_to {
                let gain = w_ic - sigma_tot[c] * ki / m2;
                // strict `>` + tie-break to the smallest community id for determinism.
                if gain > best_gain || (gain == best_gain && c < best_comm) {
                    best_gain = gain;
                    best_comm = c;
                }
            }

            // Place i in the chosen community.
            sigma_tot[best_comm] += ki;
            if best_comm != ci {
                comm[i] = best_comm;
                improved = true;
            }
        }
    }

    comm
}

/// Compute modularity Q for a given community labeling.
fn modularity(g: &Graph, comm: &[usize]) -> f64 {
    if g.total == 0.0 {
        return 0.0;
    }
    let m2 = 2.0 * g.total;

    // Σ_in (internal edge weight, counted 2× since undirected) & Σ_tot per community.
    let mut sigma_in: HashMap<usize, f64> = HashMap::new();
    let mut sigma_tot: HashMap<usize, f64> = HashMap::new();
    for i in 0..g.n {
        *sigma_tot.entry(comm[i]).or_insert(0.0) += g.degree[i];
        for &(j, w) in &g.adj[i] {
            if comm[j] == comm[i] {
                *sigma_in.entry(comm[i]).or_insert(0.0) += w; // each internal edge counted 2×
            }
        }
    }

    let mut q = 0.0;
    for (c, &tot) in &sigma_tot {
        let inside = sigma_in.get(c).copied().unwrap_or(0.0); // = 2 * internal weight
        q += inside / m2 - (tot / m2).powi(2);
    }
    q
}

/// Default cosine threshold for two memories to be considered "similar" enough
/// for a theme edge. Deliberately fairly high: multilingual models tend to give
/// cosine ~0.4–0.5 even for pairs that are only loosely related, so a low
/// threshold makes the graph fully connected & merges all memories into a single
/// theme. 0.6 keeps only pairs that are genuinely close in meaning linked.
pub const DEFAULT_SIM_THRESHOLD: f64 = 0.6;

/// Optional embedding map: slug → normalized vector.
pub type Embeddings = std::collections::HashMap<String, Vec<f32>>;

/// Cluster a project's memories into themes via Louvain (link graph only).
pub fn cluster(memories: &[Memory]) -> ClusterResult {
    cluster_ext(memories, None, DEFAULT_SIM_THRESHOLD)
}

/// Like [`cluster`], but when `emb` is given the graph is enriched with
/// embedding similarity edges (cosine ≥ `sim_threshold`) before Louvain — so
/// themes are formed from links AND semantic proximity.
pub fn cluster_ext(
    memories: &[Memory],
    emb: Option<&Embeddings>,
    sim_threshold: f64,
) -> ClusterResult {
    let link_graph = LinkGraph::build(memories);
    let (g, slugs) = Graph::from_links(&link_graph, emb, sim_threshold);

    let labels = louvain_labels(&g);
    let q = modularity(&g, &labels);

    // Group slugs by community label.
    let mut groups: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (i, &c) in labels.iter().enumerate() {
        groups.entry(c).or_default().push(slugs[i].clone());
    }

    let mut clusters: Vec<Cluster> = groups
        .into_values()
        .map(|mut members| {
            members.sort();
            Cluster { members }
        })
        .collect();

    // Sort: largest community first, then alphabetically by first member.
    clusters.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then_with(|| a.members.first().cmp(&b.members.first()))
    });

    ClusterResult {
        modularity: q,
        clusters,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Frontmatter, Memory};

    fn mem(name: &str, links: &[&str]) -> Memory {
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
            body: String::new(),
        }
    }

    #[test]
    fn two_cliques_become_two_clusters() {
        // Two triangles {a,b,c} and {x,y,z} with a single bridge c-x.
        let mems = vec![
            mem("a", &["b", "c"]),
            mem("b", &["c"]),
            mem("c", &["x"]), // bridge
            mem("x", &["y", "z"]),
            mem("y", &["z"]),
            mem("z", &[]),
        ];
        let res = cluster(&mems);
        assert_eq!(
            res.clusters.len(),
            2,
            "expected 2 communities, got {:?}",
            res.clusters
        );
        // modularity for two nearly separate cliques should be positive & fairly high.
        assert!(
            res.modularity > 0.3,
            "modularity too low: {}",
            res.modularity
        );

        // a,b,c share a community; x,y,z share a community.
        let comm_of = |slug: &str| {
            res.clusters
                .iter()
                .position(|cl| cl.members.iter().any(|m| m == slug))
                .unwrap()
        };
        assert_eq!(comm_of("a"), comm_of("b"));
        assert_eq!(comm_of("a"), comm_of("c"));
        assert_eq!(comm_of("x"), comm_of("y"));
        assert_eq!(comm_of("x"), comm_of("z"));
        assert_ne!(comm_of("a"), comm_of("x"));
    }

    #[test]
    fn isolated_memories_are_singletons() {
        let mems = vec![mem("a", &[]), mem("b", &[]), mem("c", &[])];
        let res = cluster(&mems);
        assert_eq!(res.clusters.len(), 3);
        assert_eq!(res.modularity, 0.0);
    }

    #[test]
    fn connected_pair_clusters_together() {
        let mems = vec![mem("a", &["b"]), mem("b", &[]), mem("lone", &[])];
        let res = cluster(&mems);
        // {a,b} one community, {lone} on its own → 2 communities.
        assert_eq!(res.clusters.len(), 2);
        assert_eq!(res.clusters[0].members, vec!["a", "b"]); // largest first
        assert_eq!(res.clusters[1].members, vec!["lone"]);
    }

    #[test]
    fn empty_project_no_clusters() {
        let res = cluster(&[]);
        assert_eq!(res.clusters.len(), 0);
        assert_eq!(res.modularity, 0.0);
    }
}
