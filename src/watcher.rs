//! Auto-sync: pantau folder memori dan regenerasi `_MOC.md` saat memori diedit
//! langsung di Obsidian (di luar tool MCP).
//!
//! Fitur ini **opt-in** lewat cargo feature `watch`. Tanpa feature itu, modul
//! kosong (tidak ada dependency `notify` di build default).
//!
//! Desain:
//! - Pantau `<vault>/<memory_root>` rekursif memakai `notify-debouncer-full`
//!   (meredam ledakan event dari atomic-save editor).
//! - Saat ada perubahan file `.md` milik user, tentukan project dari path lalu
//!   regenerasi `_MOC.md` project itu.
//! - File yang kita tulis sendiri (`_MOC.md`, dotfile seperti `.embeddings.json`)
//!   **diabaikan** agar tidak terjadi loop tak-berujung.
//! - Regenerasi memakai `io_lock` yang sama dengan server, supaya tidak balapan
//!   dengan operasi tulis dari tool.

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

    /// Handle watcher yang harus tetap hidup selama server berjalan. Bila
    /// di-drop, pemantauan berhenti (RAII).
    pub struct WatchGuard {
        _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    }

    /// Mulai memantau folder memori. Mengembalikan guard yang harus disimpan
    /// (jangan di-drop) selama server hidup.
    pub fn spawn(config: Config, io_lock: Arc<Mutex<()>>) -> anyhow::Result<WatchGuard> {
        let mem_dir = config.memory_dir();
        ensure_dir(&mem_dir)?; // notify gagal bila path belum ada

        // Channel: callback (thread notify, sinkron) → consumer (task tokio).
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
                                // try_send: jangan pernah blok thread notify.
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
        tracing::info!(dir = %mem_dir.display(), "file watcher aktif (auto-regen _MOC.md)");

        // Consumer: regenerasi peta project yang memorinya berubah.
        tokio::spawn(async move {
            while let Some(path) = rx.recv().await {
                let Some(project) = project_of_path(&config, &path) else {
                    continue;
                };
                let _guard = io_lock.lock().await;
                match regenerate_moc(&config, &project) {
                    Ok(_) => {
                        tracing::info!(project = %project, "peta diregenerasi (edit eksternal)")
                    }
                    Err(e) => tracing::warn!(project = %project, "gagal regen peta: {e}"),
                }
            }
        });

        Ok(WatchGuard {
            _debouncer: debouncer,
        })
    }

    /// True bila path adalah file memori milik user: berekstensi `.md`, bukan
    /// `_MOC.md`, dan tidak ada komponen dotfile (mis. `.embeddings.json`,
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

    /// Tentukan nama project dari path file di dalam folder memori:
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
                default_project: None,
            }
        }

        #[test]
        fn detects_user_memory_files() {
            assert!(is_user_memory(Path::new("/vault/memory/proj/auth-flow.md")));
            // bukan .md
            assert!(!is_user_memory(Path::new("/vault/memory/proj/note.txt")));
            // file peta kita sendiri
            assert!(!is_user_memory(Path::new("/vault/memory/proj/_MOC.md")));
            // dotfile / folder dot
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
            // di luar folder memori → None
            assert_eq!(project_of_path(&c, Path::new("/other/place/file.md")), None);
        }
    }
}

// `WatchGuard` di-re-export sebagai tipe (disimpan di main agar hidup); namanya
// tak dirujuk eksplisit di tempat lain, jadi bungkam unused-import.
#[cfg(feature = "watch")]
#[allow(unused_imports)]
pub use imp::{spawn, WatchGuard};
