//! Katalog **MCP Prompt** siap-pakai untuk bekerja dengan memori.
//!
//! Prompt adalah template instruksi yang bisa dipilih pengguna di klien MCP.
//! Modul ini mendefinisikan katalog (nama, deskripsi, argumen) dan merender
//! teks pesan berdasarkan isi memori yang relevan. Logika pembacaan memori
//! diberikan oleh pemanggil (server) lewat closure/data, agar modul ini murni.

/// Identitas prompt yang dikenal server.
pub mod names {
    pub const SUMMARIZE_PROJECT: &str = "summarize-project";
    pub const REVIEW_DECISIONS: &str = "review-decisions";
    pub const ONBOARD: &str = "onboard";
}

/// Metadata satu prompt untuk ditampilkan di `list_prompts`.
pub struct PromptSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// (nama_argumen, deskripsi, required)
    pub arguments: &'static [(&'static str, &'static str, bool)],
}

/// Katalog seluruh prompt yang didukung.
pub const CATALOG: &[PromptSpec] = &[
    PromptSpec {
        name: names::SUMMARIZE_PROJECT,
        description: "Rangkum semua memori sebuah project menjadi ikhtisar singkat.",
        arguments: &[(
            "project",
            "Nama project (opsional; auto-detect bila kosong).",
            false,
        )],
    },
    PromptSpec {
        name: names::REVIEW_DECISIONS,
        description: "Tinjau semua memori bertipe 'decision' dan nilai apakah masih relevan.",
        arguments: &[(
            "project",
            "Nama project (opsional; auto-detect bila kosong).",
            false,
        )],
    },
    PromptSpec {
        name: names::ONBOARD,
        description: "Jelaskan project ini kepada anggota baru berdasarkan memori yang ada.",
        arguments: &[(
            "project",
            "Nama project (opsional; auto-detect bila kosong).",
            false,
        )],
    },
];

/// Ringkasan satu memori untuk dirender ke dalam teks prompt.
pub struct MemoryBrief {
    pub name: String,
    pub kind: String,
    pub description: String,
}

/// Render isi pesan prompt `summarize-project`.
pub fn render_summarize(project: &str, mems: &[MemoryBrief]) -> String {
    let mut s = format!(
        "Kamu sedang meninjau basis memori project \"{project}\". \
         Berikut daftar memori (judul — tipe — deskripsi):\n\n"
    );
    append_list(&mut s, mems);
    s.push_str(
        "\nTugas: tulis ikhtisar singkat (3–6 kalimat) yang menjelaskan project ini, \
         tema-tema utamanya, dan keputusan penting yang sudah diambil. Akhiri dengan \
         daftar 'hal yang masih perlu diperjelas' bila ada.",
    );
    s
}

/// Render isi pesan prompt `review-decisions`.
pub fn render_review_decisions(project: &str, decisions: &[MemoryBrief]) -> String {
    if decisions.is_empty() {
        return format!(
            "Project \"{project}\" belum punya memori bertipe 'decision'. \
             Sarankan keputusan apa saja yang sebaiknya didokumentasikan \
             berdasarkan konteks yang kamu tahu."
        );
    }
    let mut s =
        format!("Tinjau keputusan-keputusan (type=decision) pada project \"{project}\":\n\n");
    append_list(&mut s, decisions);
    s.push_str(
        "\nUntuk tiap keputusan: nilai apakah (a) masih relevan, (b) perlu ditinjau ulang, \
         atau (c) sudah usang. Beri alasan singkat dan rekomendasi tindak lanjut.",
    );
    s
}

/// Render isi pesan prompt `onboard`.
pub fn render_onboard(project: &str, mems: &[MemoryBrief]) -> String {
    let mut s = format!(
        "Seorang anggota baru bergabung ke project \"{project}\". \
         Berikut memori yang tersedia:\n\n"
    );
    append_list(&mut s, mems);
    s.push_str(
        "\nTugas: tulis penjelasan onboarding yang ramah pemula — mulai dari gambaran besar, \
         lalu komponen penting, lalu di mana mereka bisa mulai berkontribusi. \
         Rujuk memori terkait dengan menyebut judulnya.",
    );
    s
}

fn append_list(s: &mut String, mems: &[MemoryBrief]) {
    if mems.is_empty() {
        s.push_str("_(belum ada memori)_\n");
        return;
    }
    for m in mems {
        s.push_str(&format!("- {} — [{}] {}\n", m.name, m.kind, m.description));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn briefs() -> Vec<MemoryBrief> {
        vec![
            MemoryBrief {
                name: "auth-flow".into(),
                kind: "project".into(),
                description: "autentikasi".into(),
            },
            MemoryBrief {
                name: "pakai-rust".into(),
                kind: "decision".into(),
                description: "kenapa rust".into(),
            },
        ]
    }

    #[test]
    fn catalog_has_three_prompts() {
        assert_eq!(CATALOG.len(), 3);
        assert!(CATALOG.iter().any(|p| p.name == names::SUMMARIZE_PROJECT));
    }

    #[test]
    fn summarize_lists_memories() {
        let out = render_summarize("demo", &briefs());
        assert!(out.contains("demo"));
        assert!(out.contains("auth-flow"));
        assert!(out.contains("[decision]"));
    }

    #[test]
    fn review_decisions_handles_empty() {
        let out = render_review_decisions("demo", &[]);
        assert!(out.contains("belum punya memori bertipe 'decision'"));
    }
}
