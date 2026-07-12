//! Restack state machine: rebasing a staircase onto upstream.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use stacksaw_ssp::git_ref::GitRef;

use crate::executor::GitExecutor;
use crate::refs;
use crate::{GitError, Repo};

/// Parameters for restacking a staircase onto an upstream target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestackParams {
    /// The staircase's branch refs, root -> tip order.
    pub staircase: Vec<String>,
    /// The ref to rebase onto (upstream).
    pub onto: String,
}

/// The classified reason a rebase stopped.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopKind {
    Conflict,
}

/// The result of a restack run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum RestackOutcome {
    /// Every step succeeded; refs moved atomically.
    Completed {
        rewrites: Vec<(String, String)>,
        updated_refs: Vec<String>,
        checkpoint: String,
    },
    /// The sequencer stopped due to conflict.
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

/// Drive the mechanical restack.
pub struct Restacker<'a> {
    repo: &'a Repo,
    params: RestackParams,
}

impl<'a> Restacker<'a> {
    pub fn new(repo: &'a Repo, params: RestackParams) -> Self {
        Restacker { repo, params }
    }

    /// Execute the restack.
    pub fn run(&self) -> Result<RestackOutcome, RestackError> {
        let git_dir = self.repo.git_dir();
        let tip_branch = self.params.staircase.last().ok_or(RestackError::Empty)?;
        let tip_ref = format!("refs/heads/{tip_branch}");

        // Checkpoint pre-rebase refs.
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

        let use_update_refs = refs::supports_update_refs(&git_dir)?;
        let onto = self.params.onto.clone();
        let fork_s = fork.to_string();

        let mut executor =
            GitExecutor::new(&git_dir).args(["rebase", "--onto", &onto, &fork_s, tip_branch]);

        if use_update_refs {
            executor = executor.arg("--update-refs");
        }

        match executor.run_captured() {
            Ok(_) => {
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
                let (kind, commit) = classify_stop(&git_dir, &stderr);
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

    /// Abort an in-progress rebase and restore from the checkpoint.
    pub fn abort_and_restore(&self, checkpoint: &str) -> Result<Vec<String>, RestackError> {
        let git_dir = self.repo.git_dir();
        let _ = GitExecutor::new(&git_dir)
            .args(["rebase", "--abort"])
            .status();
        Ok(refs::restore_checkpoint(&git_dir, checkpoint)?)
    }
}

fn classify_stop(git_dir: &Path, _stderr: &str) -> (StopKind, String) {
    let head = GitExecutor::new(git_dir)
        .args(["rev-parse", "HEAD"])
        .run_captured()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    (StopKind::Conflict, head)
}
