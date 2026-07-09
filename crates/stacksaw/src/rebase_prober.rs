//! Background rebase probing for the interactive TUI.
//!
//! Probing shells out to a real (isolated) rebase — far too slow to run on the
//! snapshot hot path or per frame. This runs probes on a worker thread and
//! caches each verdict by its exact inputs `(onto, base, tip)`, so a verdict is
//! computed once and reused until the oids actually change. The host asks for a
//! verdict every time it reconciles the snapshot; a cache miss returns `Unknown`
//! immediately and enqueues the work, and the verdict "pops in" on a later frame
//! once the worker reports back (the host redraws when [`drain`] returns true).
//!
//! [`drain`]: RebaseProber::drain

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use stacksaw_git::rebase_probe::{probe_rebase, RebaseProbe};
use stacksaw_ssp::types::{ConflictInfo, RebaseStatus};

/// A probe outcome: the coarse verdict plus, on a conflict, *where* it breaks.
#[derive(Debug, Clone, Default)]
pub struct Verdict {
    pub status: RebaseStatus,
    pub conflict: Option<ConflictInfo>,
}

/// The exact inputs to one probe: replay `tip` (rooted at `base`) onto `onto`.
/// Two probes with the same key have the same verdict, so this is the cache key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProbeKey {
    pub onto: String,
    pub base: String,
    pub tip: String,
}

/// Owns the probe worker thread plus the verdict cache. Dropped (closing the
/// worker) when the session ends or the repo switches.
pub struct RebaseProber {
    cache: HashMap<ProbeKey, Verdict>,
    in_flight: HashSet<ProbeKey>,
    jobs: Sender<ProbeKey>,
    results: Receiver<(ProbeKey, Verdict)>,
}

impl RebaseProber {
    /// Spawn the worker for a repo. `workdir` is the main worktree (to register
    /// the scratch worktree) and `common` its common git dir (where the scratch
    /// worktree is parked).
    pub fn new(workdir: PathBuf, common: PathBuf) -> Self {
        let (jobs, job_rx) = mpsc::channel::<ProbeKey>();
        let (result_tx, results) = mpsc::channel::<(ProbeKey, Verdict)>();
        thread::Builder::new()
            .name("rebase-prober".into())
            .spawn(move || {
                // Exits when `jobs` is dropped (the recv errors).
                while let Ok(key) = job_rx.recv() {
                    let verdict =
                        match probe_rebase(&workdir, &common, &key.onto, &key.base, &key.tip) {
                            Ok(RebaseProbe::Clean) => Verdict {
                                status: RebaseStatus::Clean,
                                conflict: None,
                            },
                            Ok(RebaseProbe::Conflict { commit, paths }) => Verdict {
                                status: RebaseStatus::Conflict,
                                conflict: Some(ConflictInfo {
                                    commit: commit.unwrap_or_default(),
                                    paths,
                                }),
                            },
                            Ok(RebaseProbe::UpToDate) | Err(_) => Verdict::default(),
                        };
                    if result_tx.send((key, verdict)).is_err() {
                        break; // host is gone
                    }
                }
            })
            .expect("spawn rebase-prober thread");
        RebaseProber {
            cache: HashMap::new(),
            in_flight: HashSet::new(),
            jobs,
            results,
        }
    }

    /// The cached verdict for `key`, or the default (`Unknown`, no detail) while
    /// it is (now) being probed in the background. Enqueues the probe on a cache
    /// miss (deduped against in-flight work).
    pub fn verdict(&mut self, key: ProbeKey) -> Verdict {
        if let Some(v) = self.cache.get(&key) {
            return v.clone();
        }
        if self.in_flight.insert(key.clone()) {
            // A full queue only happens if the worker died; ignore — the verdict
            // just stays Unknown.
            let _ = self.jobs.send(key);
        }
        Verdict::default()
    }

    /// Fold any finished probes into the cache. Returns true when at least one
    /// verdict arrived, so the host knows to re-apply verdicts and redraw.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Ok((key, verdict)) = self.results.try_recv() {
            self.cache.insert(key.clone(), verdict);
            self.in_flight.remove(&key);
            changed = true;
        }
        changed
    }
}
