//! Server configuration, read from environment variables.
//!
//! - `OBSIDIAN_VAULT_PATH`   : (required) absolute path to the Obsidian Vault folder.
//! - `OBSIDIAN_MEMORY_ROOT`  : (optional) subfolder inside the vault for storing
//!   memories. Default: `memory`.
//! - `OBSIDIAN_DOCS_ROOT`    : (optional) subfolder inside the vault for storing
//!   documents (spec/runbook/brainstorm/worklog). Default: `docs`. Deliberately
//!   OUTSIDE `memory_root` so documents are not indexed into the graph/semantic/MOC.
//! - `OBSIDIAN_DEFAULT_PROJECT`: (optional) default project name when it cannot be
//!   detected from the working directory.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    /// Vault root folder, e.g. `/path/to/Obsidian Vault`.
    pub vault_path: PathBuf,
    /// Subfolder inside the vault where memories are stored (default `memory`).
    pub memory_root: String,
    /// Subfolder inside the vault where documents are stored (default `docs`).
    /// Separate from `memory_root` so documents are not indexed into the graph.
    pub docs_root: String,
    /// Default project name when automatic detection fails.
    pub default_project: Option<String>,
}

impl Config {
    /// Load the configuration from the environment. Errors if `OBSIDIAN_VAULT_PATH`
    /// is not set or is not a valid folder.
    pub fn from_env() -> anyhow::Result<Self> {
        let vault_path = std::env::var("OBSIDIAN_VAULT_PATH").map_err(|_| {
            anyhow::anyhow!(
                "environment variable OBSIDIAN_VAULT_PATH is not set. \
                 Set it to the path of your Obsidian Vault, e.g. \
                 '/path/to/Obsidian Vault'."
            )
        })?;
        let vault_path = PathBuf::from(vault_path);
        if !vault_path.is_dir() {
            anyhow::bail!(
                "OBSIDIAN_VAULT_PATH ('{}') is not a valid folder / was not found.",
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

    /// Absolute path to the memory root folder (`<vault>/<memory_root>`).
    pub fn memory_dir(&self) -> PathBuf {
        self.vault_path.join(&self.memory_root)
    }

    /// Absolute path to a project's folder (`<vault>/<memory_root>/<project>`).
    pub fn project_dir(&self, project: &str) -> PathBuf {
        self.memory_dir().join(project)
    }

    /// Absolute path to a single memory file.
    pub fn memory_file(&self, project: &str, slug: &str) -> PathBuf {
        self.project_dir(project).join(format!("{slug}.md"))
    }

    /// Path to a project's Map of Content (map) file.
    pub fn moc_file(&self, project: &str) -> PathBuf {
        self.project_dir(project).join("_MOC.md")
    }

    /// Absolute path to the documents root folder (`<vault>/<docs_root>`).
    pub fn docs_dir(&self) -> PathBuf {
        self.vault_path.join(&self.docs_root)
    }

    /// Absolute path to a project's documents folder
    /// (`<vault>/<docs_root>/<project>`).
    pub fn docs_project_dir(&self, project: &str) -> PathBuf {
        self.docs_dir().join(project)
    }

    /// Absolute path to a single document file.
    pub fn docs_file(&self, project: &str, slug: &str) -> PathBuf {
        self.docs_project_dir(project).join(format!("{slug}.md"))
    }

    /// Path to a project's document index file (`_DOCS.md`).
    pub fn docs_index_file(&self, project: &str) -> PathBuf {
        self.docs_project_dir(project).join("_DOCS.md")
    }
}

/// Ensure a folder exists (create it recursively if it does not).
pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}
