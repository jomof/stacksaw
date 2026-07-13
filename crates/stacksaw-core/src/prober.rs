//! Background rebase/restack probing owned by the core service (§4 preview).
//!
//! Verdicts are computed off-thread and cached by `(onto, base, tip)`; a cache
//! miss returns `Unknown` until the worker reports back, at which point the
//! service bumps the snapshot generation so subscribers re-pull.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use stacksaw_git::rebase_probe::{probe_rebase, RebaseProbe};
use stacksaw_ssp::types::{ConflictInfo, RebaseStatus, Staircase};

/// The exact inputs to one probe.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProbeKey {
    pub onto: String,
    pub base: String,
    pub tip: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Verdict {
    pub status: RebaseStatus,
    pub conflict: Option<ConflictInfo>,
}

/// Shared probe cache + worker. Cloneable via `Arc`.
#[derive(Clone)]
pub struct RebaseProber {
    inner: Arc<ProberInner>,
}

struct ProberInner {
    cache_path: PathBuf,
    cache: Mutex<HashMap<ProbeKey, Verdict>>,
    in_flight: Mutex<HashSet<ProbeKey>>,
    jobs: Sender<ProbeKey>,
    results: Mutex<Receiver<(ProbeKey, Verdict)>>,
}

fn load_cache(cache_path: &Path) -> HashMap<ProbeKey, Verdict> {
    if let Ok(data) = fs::read_to_string(cache_path) {
        if let Ok(vec) = serde_json::from_str::<Vec<(ProbeKey, Verdict)>>(&data) {
            return vec.into_iter().collect();
        }
    }
    HashMap::new()
}

fn save_cache(cache_path: &Path, cache: &HashMap<ProbeKey, Verdict>) {
    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let vec: Vec<(&ProbeKey, &Verdict)> = cache.iter().collect();
    if let Ok(json) = serde_json::to_string_pretty(&vec) {
        let _ = fs::write(cache_path, json);
    }
}

impl RebaseProber {
    pub fn new(workdir: PathBuf, common: PathBuf) -> Self {
        let cache_path = common.join("stacksaw").join("probe_cache.json");
        let initial_cache = load_cache(&cache_path);
        let (jobs, job_rx) = std::sync::mpsc::channel::<ProbeKey>();
        let (result_tx, results) = std::sync::mpsc::channel::<(ProbeKey, Verdict)>();
        std::thread::Builder::new()
            .name("core-rebase-prober".into())
            .spawn(move || {
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
                        break;
                    }
                }
            })
            .expect("spawn core-rebase-prober thread");
        RebaseProber {
            inner: Arc::new(ProberInner {
                cache_path,
                cache: Mutex::new(initial_cache),
                in_flight: Mutex::new(HashSet::new()),
                jobs,
                results: Mutex::new(results),
            }),
        }
    }

    /// Apply cached verdicts to staircases and enqueue any missing probes.
    pub fn annotate(&self, repo: &stacksaw_git::Repo, staircases: &mut [Staircase]) {
        for s in staircases.iter_mut() {
            let oids = if s.segments.iter().any(|seg| seg.stale) {
                stacksaw_git::restack_probe_oids(s)
            } else if s.behind > 0 {
                stacksaw_git::rebase_probe_oids(repo, s)
            } else {
                s.rebase = RebaseStatus::Unknown;
                s.conflict = None;
                continue;
            };
            let Some((onto, base, tip)) = oids else {
                s.rebase = RebaseStatus::Unknown;
                s.conflict = None;
                continue;
            };
            let key = ProbeKey { onto, base, tip };
            let v = self.verdict(key);
            s.rebase = v.status;
            s.conflict = v.conflict;
        }
    }

    fn verdict(&self, key: ProbeKey) -> Verdict {
        if let Some(v) = self.inner.cache.lock().unwrap().get(&key) {
            return v.clone();
        }
        let mut in_flight = self.inner.in_flight.lock().unwrap();
        if in_flight.insert(key.clone()) {
            let _ = self.inner.jobs.send(key);
        }
        Verdict::default()
    }

    /// Fold finished probes into the cache and save to disk. Returns true when
    /// at least one verdict arrived (caller should bump generation).
    pub fn drain(&self) -> bool {
        let mut changed = false;
        let results = self.inner.results.lock().unwrap();
        while let Ok((key, verdict)) = results.try_recv() {
            self.inner
                .cache
                .lock()
                .unwrap()
                .insert(key.clone(), verdict);
            self.inner.in_flight.lock().unwrap().remove(&key);
            changed = true;
        }
        if changed {
            let cache = self.inner.cache.lock().unwrap().clone();
            save_cache(&self.inner.cache_path, &cache);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_prober_cache_persistence() {
        let tmp = tempdir().unwrap();
        let common = tmp.path().join(".git");
        let workdir = tmp.path().to_path_buf();
        fs::create_dir_all(&common).unwrap();

        let key = ProbeKey {
            onto: "onto_commit".to_string(),
            base: "base_commit".to_string(),
            tip: "tip_commit".to_string(),
        };

        let verdict = Verdict {
            status: RebaseStatus::Clean,
            conflict: None,
        };

        let cache_path = common.join("stacksaw").join("probe_cache.json");
        let mut map = HashMap::new();
        map.insert(key.clone(), verdict.clone());
        save_cache(&cache_path, &map);

        let prober = RebaseProber::new(workdir, common);
        let loaded = prober.verdict(key);
        assert_eq!(loaded.status, RebaseStatus::Clean);
    }
}
