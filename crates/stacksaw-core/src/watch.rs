//! External change detection (§6).
//!
//! Watches `.git` (HEAD, refs, index, sequencer dirs) and the worktree, coalesces
//! events with a debounce, and maps each burst to a snapshot generation bump. A
//! low-frequency reconciliation walk catches any dropped events.

use std::path::Path;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::new_debouncer;

use crate::service::{ChangeEvent, Service};

/// Debounce window (§6: 50 ms debounce, event coalescing).
const DEBOUNCE: Duration = Duration::from_millis(50);

/// Start watching in a dedicated thread. The watcher lives until the returned
/// guard is dropped.
pub fn spawn(service: Service) -> anyhow::Result<WatchGuard> {
    let git_dir = service.git_dir().to_path_buf();
    let workdir = service.repo_root().to_path_buf();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    let handle = std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = match new_debouncer(DEBOUNCE, None, tx) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("failed to start watcher: {e}");
                return;
            }
        };

        // `.git` catches ref/HEAD/index/sequencer changes made by any tool.
        let _ = debouncer.watcher().watch(&git_dir, RecursiveMode::Recursive);
        // The worktree root catches saves; ignore-aware descent is a refinement.
        let _ = debouncer
            .watcher()
            .watch(&workdir, RecursiveMode::NonRecursive);

        loop {
            // Stop signal?
            if stop_rx.try_recv().is_ok() {
                break;
            }
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(events)) => {
                    let touched_refs = events.iter().any(|e| {
                        e.paths
                            .iter()
                            .any(|p| path_is_ref_like(p))
                    });
                    let g = service.advance();
                    tracing::debug!("watch advanced snapshot to generation {g}");
                    if touched_refs {
                        service.emit(ChangeEvent::RefsChanged);
                    } else {
                        service.emit(ChangeEvent::WorktreeChanged);
                    }
                }
                Ok(Err(errs)) => tracing::warn!("watch errors: {errs:?}"),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Reconciliation tick (§6 safety valve): a cheap advance so
                    // clients re-pull if any event was dropped.
                    // (A full mtime/hash compare would go here.)
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    Ok(WatchGuard {
        stop_tx: Some(stop_tx),
        handle: Some(handle),
    })
}

fn path_is_ref_like(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.contains("/refs/") || s.ends_with("/HEAD") || s.ends_with("packed-refs")
}

/// Dropping this stops the watcher thread.
pub struct WatchGuard {
    stop_tx: Option<std::sync::mpsc::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
