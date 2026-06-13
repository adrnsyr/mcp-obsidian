//! Auto-sync: watch the memory folder and regenerate `_MOC.md` when a memory is
//! edited directly in Obsidian (outside the MCP tools).
//!
//! This feature is **opt-in** via the cargo feature `watch`. Without that feature,
//! the module is empty (no `notify` dependency in the default build).
//!
//! Design:
//! - Watch `<vault>/<memory_root>` recursively using `notify-debouncer-full`
//!   (damping the burst of events from an editor's atomic save).
//! - When a user's `.md` file changes, determine the project from the path and
//!   regenerate that project's `_MOC.md`.
//! - Files we write ourselves (`_MOC.md`, dotfiles such as `.embeddings.json`)
//!   are **ignored** to avoid an endless loop.
//! - Regeneration uses the same `io_lock` as the server, so it does not race
//!   with write operations from the tools.

#[cfg(feature = "watch")]
mod imp {
    use crate::config::{ensure_dir, Config};
    use crate::mapping::regenerate_moc;
    use notify_debouncer_full::notify::RecommendedWatcher;
    use notify_debouncer_full::notify::{EventKind, RecursiveMode};
    use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{mpsc, Mutex};

    /// Watcher handle that must stay alive while the server is running. When
    /// dropped, watching stops (RAII).
    pub struct WatchGuard {
        _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    }

    /// Start watching the memory folder. Returns a guard that must be kept
    /// (not dropped) for as long as the server lives.
    pub fn spawn(config: Config, io_lock: Arc<Mutex<()>>) -> anyhow::Result<WatchGuard> {
        let mem_dir = config.memory_dir();
        ensure_dir(&mem_dir)?; // notify fails if the path does not yet exist

        // Channel: callback (notify thread, synchronous) → consumer (tokio task).
        let (tx, mut rx) = mpsc::channel::<PathBuf>(256);

        let mut debouncer = new_debouncer(
            Duration::from_secs(2),
            None,
            move |res: DebounceEventResult| match res {
                Ok(events) => {
                    for ev in events {
                        if !matches!(
                            ev.kind,
                            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                        ) {
                            continue;
                        }
                        for path in &ev.paths {
                            if is_user_memory(path) {
                                // try_send: never block the notify thread.
                                let _ = tx.try_send(path.clone());
                            }
                        }
                    }
                }
                Err(errors) => {
                    for e in errors {
                        tracing::warn!("watch error: {e:?}");
                    }
                }
            },
        )?;

        debouncer.watch(&mem_dir, RecursiveMode::Recursive)?;
        tracing::info!(dir = %mem_dir.display(), "file watcher active (auto-regenerate _MOC.md)");

        // Consumer: regenerate the map of the project whose memory changed.
        tokio::spawn(async move {
            while let Some(path) = rx.recv().await {
                let Some(project) = project_of_path(&config, &path) else {
                    continue;
                };
                let _guard = io_lock.lock().await;
                match regenerate_moc(&config, &project) {
                    Ok(_) => {
                        tracing::info!(project = %project, "map regenerated (external edit)")
                    }
                    Err(e) => tracing::warn!(project = %project, "failed to regenerate map: {e}"),
                }
            }
        });

        Ok(WatchGuard {
            _debouncer: debouncer,
        })
    }

    /// True if the path is a user's memory file: has the `.md` extension, is not
    /// `_MOC.md`, and has no dotfile component (e.g. `.embeddings.json`,
    /// `.obsidian/`).
    fn is_user_memory(path: &Path) -> bool {
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            return false;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("_MOC.md") {
            return false;
        }
        path.components()
            .filter_map(|c| c.as_os_str().to_str())
            .all(|seg| !seg.starts_with('.'))
    }

    /// Determine the project name from a file path inside the memory folder:
    /// `<vault>/<memory_root>/<project>/<slug>.md` → `project`.
    fn project_of_path(config: &Config, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(config.memory_dir()).ok()?;
        let first = rel.components().next()?;
        let name = first.as_os_str().to_str()?;
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn cfg() -> Config {
            Config {
                vault_path: PathBuf::from("/vault"),
                memory_root: "memory".into(),
                docs_root: "docs".into(),
                default_project: None,
            }
        }

        #[test]
        fn detects_user_memory_files() {
            assert!(is_user_memory(Path::new("/vault/memory/proj/auth-flow.md")));
            // not .md
            assert!(!is_user_memory(Path::new("/vault/memory/proj/note.txt")));
            // our own map file
            assert!(!is_user_memory(Path::new("/vault/memory/proj/_MOC.md")));
            // dotfile / dot folder
            assert!(!is_user_memory(Path::new(
                "/vault/memory/proj/.embeddings.json"
            )));
            assert!(!is_user_memory(Path::new("/vault/.obsidian/x.md")));
        }

        #[test]
        fn derives_project_from_path() {
            let c = cfg();
            assert_eq!(
                project_of_path(&c, Path::new("/vault/memory/demo/auth-flow.md")).as_deref(),
                Some("demo")
            );
            // outside the memory folder → None
            assert_eq!(project_of_path(&c, Path::new("/other/place/file.md")), None);
        }
    }
}

// `WatchGuard` is re-exported as a type (held in main to keep it alive); its name
// is not referenced explicitly elsewhere, so silence the unused-import warning.
#[cfg(feature = "watch")]
#[allow(unused_imports)]
pub use imp::{spawn, WatchGuard};
