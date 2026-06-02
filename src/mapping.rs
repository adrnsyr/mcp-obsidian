//! Pembuatan peta memori (Map of Content / `_MOC.md`) untuk satu project.
//!
//! Peta dikelompokkan per kategori (`type`), memuat daftar wikilink ke setiap
//! memori beserta deskripsinya, lalu bagian "Relasi" yang merangkum link
//! antar-memori (field `links`). Obsidian Graph View akan otomatis menampilkan
//! keterhubungan ini.

use crate::cluster;
use crate::config::{ensure_dir, Config};
use crate::links;
use crate::memory::{load_all, now_rfc3339, Memory};
use crate::similarity;
use std::collections::BTreeMap;

/// Jumlah saran relasi pintar yang ditampilkan per memori di peta.
const MOC_SUGGEST_TOP: usize = 3;
/// Ambang skor agar saran cukup relevan untuk masuk peta (lebih ketat dari
/// default tool agar peta tidak berisik).
const MOC_SUGGEST_THRESHOLD: f64 = 0.1;

/// Bangun isi teks `_MOC.md` untuk sebuah project.
pub fn build_moc_string(project: &str, memories: &[Memory]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# 🗺️ Peta Memori — `{project}`\n\n"));
    out.push_str(&format!(
        "> Dihasilkan otomatis oleh mcp-obsidian pada {}. \
         Jangan diedit manual — perubahan akan ditimpa.\n\n",
        now_rfc3339()
    ));

    if memories.is_empty() {
        out.push_str("_Belum ada memori untuk project ini._\n");
        return out;
    }

    out.push_str(&format!("Total memori: **{}**\n\n", memories.len()));

    // Kelompokkan per kategori (type).
    let mut by_kind: BTreeMap<String, Vec<&Memory>> = BTreeMap::new();
    for m in memories {
        by_kind.entry(m.front.kind.clone()).or_default().push(m);
    }

    for (kind, items) in &by_kind {
        out.push_str(&format!("## {}\n\n", title_case(kind)));
        for m in items {
            let f = &m.front;
            let tags = if f.tags.is_empty() {
                String::new()
            } else {
                let t: Vec<String> = f.tags.iter().map(|t| format!("#{t}")).collect();
                format!("  _{}_", t.join(" "))
            };
            out.push_str(&format!("- [[{}]] — {}{}\n", f.name, f.description, tags));
        }
        out.push('\n');
    }

    // Bagian relasi eksplisit (field `links`).
    // Graf tautan: gabungan field `links` + [[wikilink]] di body.
    let graph = links::LinkGraph::build(memories);

    // Relasi keluar (outgoing) — hanya yang menunjuk memori yang ada.
    let mut has_relations = false;
    let mut relation_block = String::from("## 🔗 Relasi\n\n");
    for (name, outs) in &graph.forward {
        let valid: Vec<String> = outs
            .iter()
            .filter(|t| graph.existing.contains(*t))
            .map(|t| format!("[[{t}]]"))
            .collect();
        if !valid.is_empty() {
            has_relations = true;
            relation_block.push_str(&format!("- [[{name}]] → {}\n", valid.join(", ")));
        }
    }
    if has_relations {
        out.push_str(&relation_block);
        out.push('\n');
    }

    // Backlink (derived) — siapa menaut siapa, dihitung dari graf.
    if !graph.backward.is_empty() {
        out.push_str("## ⬅️ Backlink\n\n");
        for (name, sources) in &graph.backward {
            let rendered: Vec<String> = sources.iter().map(|s| format!("[[{s}]]")).collect();
            out.push_str(&format!("- [[{name}]] ← {}\n", rendered.join(", ")));
        }
        out.push('\n');
    }

    // Indeks tag.
    let mut by_tag: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in memories {
        for tag in &m.front.tags {
            by_tag
                .entry(tag.clone())
                .or_default()
                .push(m.front.name.clone());
        }
    }
    if !by_tag.is_empty() {
        out.push_str("## 🏷️ Indeks Tag\n\n");
        for (tag, names) in &by_tag {
            let rendered: Vec<String> = names.iter().map(|n| format!("[[{n}]]")).collect();
            out.push_str(&format!("- **#{tag}**: {}\n", rendered.join(", ")));
        }
        out.push('\n');
    }

    // Relasi pintar: saran tautan berdasarkan kemiripan tag + isi (TF-IDF),
    // di luar yang sudah ditautkan manual. Sekadar usulan, bukan link nyata.
    let suggestions = similarity::suggest_all(memories, MOC_SUGGEST_TOP, MOC_SUGGEST_THRESHOLD);
    if !suggestions.is_empty() {
        out.push_str("## 💡 Saran Relasi\n\n");
        out.push_str(
            "> Usulan otomatis berdasarkan kemiripan; jalankan `memory_suggest` \
             dengan `apply: true` untuk menjadikannya tautan.\n\n",
        );
        for (name, sugg) in &suggestions {
            let rendered: Vec<String> = sugg
                .iter()
                .map(|s| format!("[[{}]] ({:.2})", s.name, s.score))
                .collect();
            out.push_str(&format!("- [[{name}]] ⇢ {}\n", rendered.join(", ")));
        }
        out.push('\n');
    }

    // Tema: klaster komunitas (Louvain) pada graf tautan. Hanya tampilkan bila
    // ada lebih dari satu tema bermakna (>1 klaster, minimal satu beranggota >1).
    let clustering = cluster::cluster(memories);
    let meaningful = clustering
        .clusters
        .iter()
        .filter(|c| c.members.len() > 1)
        .count();
    if clustering.clusters.len() > 1 && meaningful >= 1 {
        out.push_str(&format!(
            "## 🧩 Tema (modularity {:.2})\n\n",
            clustering.modularity
        ));
        for (i, c) in clustering.clusters.iter().enumerate() {
            let rendered: Vec<String> = c.members.iter().map(|m| format!("[[{m}]]")).collect();
            out.push_str(&format!("- **Tema {}**: {}\n", i + 1, rendered.join(", ")));
        }
        out.push('\n');
    }

    out
}

/// Regenerasi `_MOC.md` di disk untuk sebuah project.
/// Mengembalikan isi teks peta yang baru ditulis.
pub fn regenerate_moc(config: &Config, project: &str) -> anyhow::Result<String> {
    let memories = load_all(config, project);
    let content = build_moc_string(project, &memories);
    ensure_dir(&config.project_dir(project))?;
    std::fs::write(config.moc_file(project), &content)?;
    Ok(content)
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
