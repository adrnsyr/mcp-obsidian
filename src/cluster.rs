//! Klaster memori menjadi "tema" via deteksi komunitas **Louvain** pada graf
//! tautan (tak-berarah).
//!
//! Graf dibangun dari `LinkGraph`: setiap tautan A→B (field `links` atau
//! `[[wikilink]]` di body, yang targetnya ada) menjadi edge tak-berarah A↔B
//! berbobot 1 (bobot diakumulasi bila ada beberapa tautan antar pasangan sama).
//!
//! Louvain memaksimalkan **modularity** Q:
//! ```text
//! Q = (1 / 2m) * Σ_ij [ A_ij - (k_i * k_j) / 2m ] * δ(c_i, c_j)
//! ```
//! dengan `A_ij` bobot edge, `k_i` derajat berbobot node i, `m` total bobot edge,
//! dan `δ` = 1 bila i & j sekomunitas. Implementasi ini menjalankan satu level
//! (local-moving) yang sudah cukup untuk graf memori berukuran kecil–menengah;
//! node tanpa edge menjadi komunitas singleton-nya sendiri.

use crate::links::LinkGraph;
use crate::memory::Memory;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// Satu komunitas/tema hasil klaster.
#[derive(Debug, Clone, Serialize)]
pub struct Cluster {
    /// Anggota (slug memori), terurut.
    pub members: Vec<String>,
}

/// Hasil klaster sebuah project.
#[derive(Debug, Clone, Serialize)]
pub struct ClusterResult {
    /// Nilai modularity akhir (−0.5..1.0; makin tinggi makin tegas komunitasnya).
    pub modularity: f64,
    /// Daftar komunitas, terurut dari anggota terbanyak.
    pub clusters: Vec<Cluster>,
}

/// Representasi graf tak-berarah berbobot dengan node berindeks `0..n`.
struct Graph {
    n: usize,
    /// adjacency: node -> [(tetangga, bobot)]
    adj: Vec<Vec<(usize, f64)>>,
    /// derajat berbobot tiap node (termasuk 2× self-loop bila ada).
    degree: Vec<f64>,
    /// total bobot edge `m` (Σ bobot, self-loop dihitung sekali).
    total: f64,
}

impl Graph {
    /// Bangun graf tak-berarah dari `LinkGraph`, opsional diperkaya dengan edge
    /// kemiripan embedding. Mengembalikan graf + daftar slug terurut (indeks
    /// node = posisi di slice ini).
    ///
    /// `emb`: bila `Some`, tiap pasang memori dengan cosine ≥ `sim_threshold`
    /// mendapat edge tambahan berbobot = cosine-nya. Ini membuat memori yang
    /// **mirip secara makna** ikut mengelompok walau belum saling menaut.
    fn from_links(
        graph: &LinkGraph,
        emb: Option<&Embeddings>,
        sim_threshold: f64,
    ) -> (Self, Vec<String>) {
        // Node = semua memori yang ada, terurut agar deterministik.
        let slugs: Vec<String> = graph.existing.iter().cloned().collect();
        let index: HashMap<&str, usize> = slugs
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        let n = slugs.len();

        // Akumulasi bobot edge tak-berarah ke map berkunci (min,max).
        let mut weights: BTreeMap<(usize, usize), f64> = BTreeMap::new();
        for (from, outs) in &graph.forward {
            let Some(&u) = index.get(from.as_str()) else {
                continue;
            };
            for to in outs {
                let Some(&v) = index.get(to.as_str()) else {
                    continue; // tautan ke memori tak-ada → diabaikan
                };
                if u == v {
                    continue; // tak ada self-link
                }
                let key = (u.min(v), u.max(v));
                *weights.entry(key).or_insert(0.0) += 1.0;
            }
        }

        // Edge kemiripan embedding (bila tersedia): semua pasangan di atas
        // ambang. O(n²) cosine — cukup untuk skala memori per-project.
        if let Some(map) = emb {
            // a & b dipakai sebagai indeks tuple di `weights`; rewrite iterator
            // justru mengaburkan maksudnya.
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

/// Jalankan Louvain (satu level local-moving) dan kembalikan label komunitas
/// per node (`comm[i]` = id komunitas node i, belum tentu rapat).
fn louvain_labels(g: &Graph) -> Vec<usize> {
    // Mula-mula tiap node komunitasnya sendiri.
    let mut comm: Vec<usize> = (0..g.n).collect();
    // Σ_tot[c] = total derajat berbobot semua node di komunitas c.
    let mut sigma_tot: Vec<f64> = g.degree.clone();

    if g.total == 0.0 {
        return comm; // tak ada edge: semua singleton
    }
    let m2 = 2.0 * g.total; // 2m

    let mut improved = true;
    // batasi iterasi agar pasti berhenti walau ada osilasi numerik.
    let mut rounds = 0;
    while improved && rounds < 100 {
        improved = false;
        rounds += 1;

        for i in 0..g.n {
            let ci = comm[i];
            let ki = g.degree[i];

            // Bobot dari i ke tiap komunitas tetangga (termasuk komunitasnya kini).
            let mut w_to: HashMap<usize, f64> = HashMap::new();
            for &(j, w) in &g.adj[i] {
                *w_to.entry(comm[j]).or_insert(0.0) += w;
            }

            // Lepaskan i dari komunitasnya: kurangi kontribusi derajatnya.
            sigma_tot[ci] -= ki;

            // Cari komunitas terbaik (gain modularity terbesar). Basis = tetap di ci.
            // ΔQ untuk pindah ke c ∝ w_to[c] - Σ_tot[c] * k_i / 2m.
            let mut best_comm = ci;
            let w_to_ci = w_to.get(&ci).copied().unwrap_or(0.0);
            let mut best_gain = w_to_ci - sigma_tot[ci] * ki / m2;

            for (&c, &w_ic) in &w_to {
                let gain = w_ic - sigma_tot[c] * ki / m2;
                // `>` ketat + tie-break ke id komunitas terkecil demi determinisme.
                if gain > best_gain || (gain == best_gain && c < best_comm) {
                    best_gain = gain;
                    best_comm = c;
                }
            }

            // Tempatkan i di komunitas terpilih.
            sigma_tot[best_comm] += ki;
            if best_comm != ci {
                comm[i] = best_comm;
                improved = true;
            }
        }
    }

    comm
}

/// Hitung modularity Q untuk pelabelan komunitas tertentu.
fn modularity(g: &Graph, comm: &[usize]) -> f64 {
    if g.total == 0.0 {
        return 0.0;
    }
    let m2 = 2.0 * g.total;

    // Σ_in (bobot edge internal, dihitung 2× karena tak-berarah) & Σ_tot per komunitas.
    let mut sigma_in: HashMap<usize, f64> = HashMap::new();
    let mut sigma_tot: HashMap<usize, f64> = HashMap::new();
    for i in 0..g.n {
        *sigma_tot.entry(comm[i]).or_insert(0.0) += g.degree[i];
        for &(j, w) in &g.adj[i] {
            if comm[j] == comm[i] {
                *sigma_in.entry(comm[i]).or_insert(0.0) += w; // tiap edge internal terhitung 2×
            }
        }
    }

    let mut q = 0.0;
    for (c, &tot) in &sigma_tot {
        let inside = sigma_in.get(c).copied().unwrap_or(0.0); // = 2 * bobot internal
        q += inside / m2 - (tot / m2).powi(2);
    }
    q
}

/// Ambang cosine default agar dua memori dianggap "mirip" untuk edge tema.
/// Sengaja cukup tinggi: model multilingual cenderung memberi cosine ~0.4–0.5
/// bahkan untuk pasangan yang hanya bertema longgar, sehingga ambang rendah
/// membuat graf terhubung penuh & semua memori melebur jadi satu tema. 0.6
/// menjaga hanya pasangan yang benar-benar berdekatan makna yang ditautkan.
pub const DEFAULT_SIM_THRESHOLD: f64 = 0.6;

/// Peta embedding opsional: slug → vektor ternormalisasi.
pub type Embeddings = std::collections::HashMap<String, Vec<f32>>;

/// Klaster memori sebuah project menjadi tema via Louvain (graf tautan saja).
pub fn cluster(memories: &[Memory]) -> ClusterResult {
    cluster_ext(memories, None, DEFAULT_SIM_THRESHOLD)
}

/// Seperti [`cluster`], tapi bila `emb` diberikan, graf diperkaya dengan edge
/// kemiripan embedding (cosine ≥ `sim_threshold`) sebelum Louvain — sehingga
/// tema terbentuk dari tautan DAN kedekatan makna.
pub fn cluster_ext(
    memories: &[Memory],
    emb: Option<&Embeddings>,
    sim_threshold: f64,
) -> ClusterResult {
    let link_graph = LinkGraph::build(memories);
    let (g, slugs) = Graph::from_links(&link_graph, emb, sim_threshold);

    let labels = louvain_labels(&g);
    let q = modularity(&g, &labels);

    // Kelompokkan slug per label komunitas.
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

    // Urutkan: komunitas terbesar dulu, lalu alfabetis anggota pertama.
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
        // Dua segitiga {a,b,c} dan {x,y,z} dengan satu jembatan c-x.
        let mems = vec![
            mem("a", &["b", "c"]),
            mem("b", &["c"]),
            mem("c", &["x"]), // jembatan
            mem("x", &["y", "z"]),
            mem("y", &["z"]),
            mem("z", &[]),
        ];
        let res = cluster(&mems);
        assert_eq!(
            res.clusters.len(),
            2,
            "harus 2 komunitas, dapat {:?}",
            res.clusters
        );
        // modularity untuk dua klik yang nyaris terpisah harus positif & cukup tinggi.
        assert!(
            res.modularity > 0.3,
            "modularity rendah: {}",
            res.modularity
        );

        // a,b,c sekomunitas; x,y,z sekomunitas.
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
        // {a,b} satu komunitas, {lone} sendiri → 2 komunitas.
        assert_eq!(res.clusters.len(), 2);
        assert_eq!(res.clusters[0].members, vec!["a", "b"]); // terbesar dulu
        assert_eq!(res.clusters[1].members, vec!["lone"]);
    }

    #[test]
    fn empty_project_no_clusters() {
        let res = cluster(&[]);
        assert_eq!(res.clusters.len(), 0);
        assert_eq!(res.modularity, 0.0);
    }
}
