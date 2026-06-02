//! Deteksi & sanitasi nama project.
//!
//! Urutan prioritas penentuan project:
//! 1. Argumen `project` eksplisit pada tool call.
//! 2. Environment `OBSIDIAN_DEFAULT_PROJECT`.
//! 3. Nama folder (basename) dari working directory tempat server dijalankan.

use crate::config::Config;

/// Ubah string sembarang menjadi slug aman untuk nama file/folder:
/// lowercase, hanya `a-z 0-9 -`, spasi/underscore jadi `-`.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.trim().chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if (c == '-' || c == '_' || c.is_whitespace()) && !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
        // karakter lain diabaikan
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Tentukan project yang aktif dari argumen opsional + konfigurasi/lingkungan.
/// Mengembalikan slug yang sudah disanitasi.
pub fn resolve_project(config: &Config, explicit: Option<&str>) -> anyhow::Result<String> {
    // 1. argumen eksplisit
    if let Some(p) = explicit {
        let s = slugify(p);
        if !s.is_empty() {
            return Ok(s);
        }
    }

    // 2. default dari env
    if let Some(p) = &config.default_project {
        let s = slugify(p);
        if !s.is_empty() {
            return Ok(s);
        }
    }

    // 3. basename working directory
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(name) = cwd.file_name().and_then(|n| n.to_str()) {
            let s = slugify(name);
            if !s.is_empty() {
                return Ok(s);
            }
        }
    }

    anyhow::bail!(
        "tidak bisa menentukan project: berikan argumen 'project', \
         atau set OBSIDIAN_DEFAULT_PROJECT."
    )
}

/// Daftar semua project yang punya folder memori.
pub fn list_projects(config: &Config) -> Vec<String> {
    let dir = config.memory_dir();
    let mut projects = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    projects.push(name.to_string());
                }
            }
        }
    }
    projects.sort();
    projects
}
