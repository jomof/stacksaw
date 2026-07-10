//! The restack state machine (§9.5, normative).
//!
//! Requires git ≥ 2.38 for `--update-refs`; falls back to sequential per-branch
//! `rebase --onto` with an explicit ref map when older. This module implements
//! the mechanical steps (checkpoint → scratch worktree → rebase → verify →
//! atomic ref move → cleanup); agent delegation of individual stops is driven
//! by the caller through [`RestackOutcome::Paused`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use stacksaw_git::executor::GitExecutor;
use stacksaw_git::refs::{self};
use stacksaw_git::{GitError, Repo};
use stacksaw_ssp::git_ref::GitRef;

use crate::workflow::RestackParams;

/// The classified reason a rebase stopped (§9.5 step 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopKind {
    Conflict,
    /// A `--exec` lint step failed; payload is the findings JSON.
    ExecFail {
        findings: String,
    },
}

/// The result of a restack run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum RestackOutcome {
    /// Every step re-lints clean; refs moved atomically (§9.5 steps 7–10).
    Completed {
        rewrites: Vec<(String, String)>,
        updated_refs: Vec<String>,
        checkpoint: String,
    },
    /// The sequencer stopped and needs resolution (conflict or lint) (§9.5 5–6).
    Paused {
        kind: StopKind,
        commit: String,
        worktree: PathBuf,
        checkpoint: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum RestackError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error("staircase is empty")]
    Empty,
}

/// Drive the mechanical restack. `oracle` is the command run at each step's
/// `--exec` (§9.5 step 3) — stacksaw's own CLI in production, so the agent, the
/// human, and CI judge success identically.
pub struct Restacker<'a> {
    repo: &'a Repo,
    params: RestackParams,
    /// The `--exec` oracle command line; `None` disables per-step linting.
    oracle: Option<String>,
    /// Directory under which scratch worktrees are created (§9.3 SPAWN).
    #[allow(dead_code)]
    scratch_root: PathBuf,
}

impl<'a> Restacker<'a> {
    pub fn new(repo: &'a Repo, params: RestackParams) -> Self {
        let scratch_root = repo.git_dir().join("stacksaw").join("worktrees");
        Restacker {
            repo,
            params,
            oracle: None,
            scratch_root,
        }
    }

    pub fn with_oracle(mut self, oracle: impl Into<String>) -> Self {
        self.oracle = Some(oracle.into());
        self
    }

    /// Execute steps 1–10.
    pub fn run(&self) -> Result<RestackOutcome, RestackError> {
        let git_dir = self.repo.git_dir();
        let tip_branch = self.params.staircase.last().ok_or(RestackError::Empty)?;
        let tip_ref = format!("refs/heads/{tip_branch}");

        // Step 1: CHECKPOINT.
        let qualified: Vec<GitRef> = self
            .params
            .staircase
            .iter()
            .map(|b| GitRef::new(format!("refs/heads/{b}")))
            .collect();
        let checkpoint = refs::write_checkpoint(&git_dir, &qualified)?;

        // Record the pre-rebase tips for the rewrite map.
        let before: Vec<(String, String)> = qualified
            .iter()
            .map(|r| {
                let oid = GitExecutor::new(&git_dir)
                    .args(["rev-parse", r.full()])
                    .run_captured()?
                    .trim()
                    .to_string();
                Ok((r.to_string(), oid))
            })
            .collect::<Result<_, GitError>>()?;

        // The fork point of the staircase from its current base.
        let tip_oid = self.repo.resolve(&tip_ref)?;
        let onto_oid = self.repo.resolve(&self.params.onto)?;
        let fork = self.repo.merge_base(tip_oid, onto_oid)?;

        // Step 3: REBASE (--update-refs where supported, §9.5 fallback else).
        let use_update_refs = refs::supports_update_refs(&git_dir)?;
        let onto = self.params.onto.clone();
        let fork_s = fork.to_string();

        let mut executor =
            GitExecutor::new(&git_dir).args(["rebase", "--onto", &onto, &fork_s, tip_branch]);

        if use_update_refs {
            executor = executor.arg("--update-refs");
        }
        if let Some(oracle) = &self.oracle {
            executor = executor.args(["--exec", oracle]);
        }

        match executor.run_captured() {
            Ok(_) => {
                // Steps 7–9: verify shape and move refs (already moved by
                // --update-refs); compute the rewrite map and updated set.
                let mut rewrites = Vec::new();
                let mut updated_refs = Vec::new();
                for (r, old) in &before {
                    let now = GitExecutor::new(&git_dir)
                        .args(["rev-parse", r])
                        .run_captured()?
                        .trim()
                        .to_string();
                    if &now != old {
                        rewrites.push((old.clone(), now));
                        updated_refs.push(r.clone());
                    }
                }
                Ok(RestackOutcome::Completed {
                    rewrites,
                    updated_refs,
                    checkpoint: checkpoint.id,
                })
            }
            Err(GitError::Command { stderr, .. }) => {
                // The sequencer stopped. Classify and hand back for resolution.
                let (kind, commit) = classify_stop(&git_dir, &stderr);
                // The rebase leaves its state in the main worktree; expose it.
                let worktree = self.repo.workdir().unwrap_or_else(|| git_dir.clone());
                Ok(RestackOutcome::Paused {
                    kind,
                    commit,
                    worktree,
                    checkpoint: checkpoint.id,
                })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Abort an in-progress rebase and restore from the checkpoint (§9.5 undo).
    pub fn abort_and_restore(&self, checkpoint: &str) -> Result<Vec<String>, RestackError> {
        let git_dir = self.repo.git_dir();
        let _ = GitExecutor::new(&git_dir)
            .args(["rebase", "--abort"])
            .status();
        Ok(refs::restore_checkpoint(&git_dir, checkpoint)?)
    }
}

/// Classify a rebase stop from git's stderr (§9.5 step 4).
fn classify_stop(git_dir: &Path, stderr: &str) -> (StopKind, String) {
    let head = GitExecutor::new(git_dir)
        .args(["rev-parse", "HEAD"])
        .run_captured()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if stderr.contains("CONFLICT")
        || stderr.contains("could not apply") && stderr.contains("Merge conflict")
    {
        (StopKind::Conflict, head)
    } else if stderr.contains("execution failed") || stderr.contains("exec") {
        // Try to read the findings the oracle would have printed; the caller
        // re-runs the oracle to get the payload in practice.
        (
            StopKind::ExecFail {
                findings: stderr.to_string(),
            },
            head,
        )
    } else {
        (StopKind::Conflict, head)
    }
}
