//! Project name detection & sanitization.
//!
//! Priority order for determining the project:
//! 1. An explicit `project` argument in the tool call.
//! 2. The `OBSIDIAN_DEFAULT_PROJECT` environment variable.
//! 3. The folder name (basename) of the working directory the server runs in.

use crate::config::Config;

/// Turn an arbitrary string into a slug safe for file/folder names:
/// lowercase, only `a-z 0-9 -`, with spaces/underscores becoming `-`.
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
        // other characters are ignored
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Determine the active project from an optional argument + config/environment.
/// Returns a sanitized slug.
pub fn resolve_project(config: &Config, explicit: Option<&str>) -> anyhow::Result<String> {
    // 1. explicit argument
    if let Some(p) = explicit {
        let s = slugify(p);
        if !s.is_empty() {
            return Ok(s);
        }
    }

    // 2. default from env
    if let Some(p) = &config.default_project {
        let s = slugify(p);
        if !s.is_empty() {
            return Ok(s);
        }
    }

    // 3. basename of the working directory
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(name) = cwd.file_name().and_then(|n| n.to_str()) {
            let s = slugify(name);
            if !s.is_empty() {
                return Ok(s);
            }
        }
    }

    anyhow::bail!(
        "could not determine project: provide a 'project' argument, \
         or set OBSIDIAN_DEFAULT_PROJECT."
    )
}

/// List all projects that have a memory folder.
pub fn list_projects(config: &Config) -> Vec<String> {
    list_dirs_in(&config.memory_dir())
}

/// List all projects that have a document folder.
pub fn list_doc_projects(config: &Config) -> Vec<String> {
    list_dirs_in(&config.docs_dir())
}

/// Names of the (sorted) subfolders inside a directory; empty if there are none.
fn list_dirs_in(dir: &std::path::Path) -> Vec<String> {
    let mut projects = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
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
