//! Konfigurasi server, dibaca dari environment variable.
//!
//! - `OBSIDIAN_VAULT_PATH`   : (wajib) path absolut ke folder Obsidian Vault.
//! - `OBSIDIAN_MEMORY_ROOT`  : (opsional) subfolder di dalam vault untuk menyimpan
//!   memori. Default: `memory`.
//! - `OBSIDIAN_DOCS_ROOT`    : (opsional) subfolder di dalam vault untuk menyimpan
//!   dokumen (spec/runbook/brainstorm/worklog). Default: `docs`. Sengaja DI LUAR
//!   `memory_root` agar dokumen tidak ikut terindeks ke graf/semantic/MOC.
//! - `OBSIDIAN_DEFAULT_PROJECT`: (opsional) nama project default jika tidak bisa
//!   dideteksi dari working directory.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    /// Root folder vault, mis. `/path/to/Obsidian Vault`.
    pub vault_path: PathBuf,
    /// Subfolder di dalam vault tempat memori disimpan (default `memory`).
    pub memory_root: String,
    /// Subfolder di dalam vault tempat dokumen disimpan (default `docs`).
    /// Terpisah dari `memory_root` agar dokumen tidak terindeks ke graf.
    pub docs_root: String,
    /// Nama project default bila deteksi otomatis gagal.
    pub default_project: Option<String>,
}

impl Config {
    /// Muat konfigurasi dari environment. Error bila `OBSIDIAN_VAULT_PATH`
    /// tidak diset atau bukan folder yang valid.
    pub fn from_env() -> anyhow::Result<Self> {
        let vault_path = std::env::var("OBSIDIAN_VAULT_PATH").map_err(|_| {
            anyhow::anyhow!(
                "environment variable OBSIDIAN_VAULT_PATH belum diset. \
                 Set ke path Obsidian Vault kamu, mis. \
                 '/path/to/Obsidian Vault'."
            )
        })?;
        let vault_path = PathBuf::from(vault_path);
        if !vault_path.is_dir() {
            anyhow::bail!(
                "OBSIDIAN_VAULT_PATH ('{}') bukan folder yang valid / tidak ditemukan.",
                vault_path.display()
            );
        }

        let memory_root =
            std::env::var("OBSIDIAN_MEMORY_ROOT").unwrap_or_else(|_| "memory".to_string());

        let docs_root = std::env::var("OBSIDIAN_DOCS_ROOT").unwrap_or_else(|_| "docs".to_string());

        let default_project = std::env::var("OBSIDIAN_DEFAULT_PROJECT")
            .ok()
            .filter(|s| !s.is_empty());

        Ok(Self {
            vault_path,
            memory_root,
            docs_root,
            default_project,
        })
    }

    /// Path absolut ke folder root memori (`<vault>/<memory_root>`).
    pub fn memory_dir(&self) -> PathBuf {
        self.vault_path.join(&self.memory_root)
    }

    /// Path absolut ke folder sebuah project (`<vault>/<memory_root>/<project>`).
    pub fn project_dir(&self, project: &str) -> PathBuf {
        self.memory_dir().join(project)
    }

    /// Path absolut ke sebuah file memori.
    pub fn memory_file(&self, project: &str, slug: &str) -> PathBuf {
        self.project_dir(project).join(format!("{slug}.md"))
    }

    /// Path ke file Map of Content (peta) sebuah project.
    pub fn moc_file(&self, project: &str) -> PathBuf {
        self.project_dir(project).join("_MOC.md")
    }

    /// Path absolut ke folder root dokumen (`<vault>/<docs_root>`).
    pub fn docs_dir(&self) -> PathBuf {
        self.vault_path.join(&self.docs_root)
    }

    /// Path absolut ke folder dokumen sebuah project
    /// (`<vault>/<docs_root>/<project>`).
    pub fn docs_project_dir(&self, project: &str) -> PathBuf {
        self.docs_dir().join(project)
    }

    /// Path absolut ke sebuah file dokumen.
    pub fn docs_file(&self, project: &str, slug: &str) -> PathBuf {
        self.docs_project_dir(project).join(format!("{slug}.md"))
    }

    /// Path ke file indeks dokumen (`_DOCS.md`) sebuah project.
    pub fn docs_index_file(&self, project: &str) -> PathBuf {
        self.docs_project_dir(project).join("_DOCS.md")
    }
}

/// Pastikan sebuah folder ada (buat rekursif bila belum).
pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}
