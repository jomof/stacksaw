//! Read-only rebase simulation (§4 preview).
//!
//! To answer "would rebasing this stack onto its upstream be clean, or will it
//! conflict?" we replay the stack in an **isolated, reused scratch worktree**
//! with a detached HEAD and then abort. No real ref, and no byte of the user's
//! working tree, is ever touched — so this is safe to run speculatively. The
//! same primitive is the engine a future one-click rebase (the clean case) and
//! assisted rebase (the conflict case) would build on.
//!
//! We run a *real* `git rebase` (sequential cherry-pick semantics) rather than a
//! single `git merge-tree`, because that is exactly what the user will
//! experience — a squashed 3-way merge can disagree with a step-by-step replay.
//! Hooks, `rerere`, and signing are disabled in the probe so it stays inert.

use fs4::fs_std::FileExt;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::executor::GitExecutor;

/// The outcome of simulating a rebase of a stack onto a new base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseProbe {
    /// `base` already equals `onto` — nothing to replay.
    UpToDate,
    /// The replay applied cleanly (a "free" rebase is available).
    Clean,
    /// The replay hit a conflict. Carries where it first breaks: `commit` is the
    /// oid of the original stack commit whose replay failed (the rebase halts
    /// there), and `paths` the files left conflicted at that commit. Either may
    /// be empty if git reported a conflict without the detail.
    Conflict {
        commit: Option<String>,
        paths: Vec<String>,
    },
}

/// Simulate `git rebase --onto <onto> <base> <tip>` in a reused detached scratch
/// worktree. Returns whether the replay would be clean or conflict, without
/// mutating any real ref or the user's working tree.
///
/// `main_workdir` is the repo's main worktree (used to register the scratch
/// worktree); `common_dir` is its common git dir (where the scratch worktree is
/// parked). All three revs are full oids or any revspec `git` accepts.
pub fn probe_rebase(
    main_workdir: &Path,
    common_dir: &Path,
    onto: &str,
    base: &str,
    tip: &str,
) -> Result<RebaseProbe> {
    if onto == base {
        return Ok(RebaseProbe::UpToDate);
    }
    let lock_path = common_dir.join("stacksaw").join("probe.lock");
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;
    let wt = ensure_probe_worktree(main_workdir, common_dir, tip)?;
    reset_worktree(&wt, tip)?;

    match GitExecutor::new(&wt)
        .inert()
        .quiet()
        .args(["rebase", "--onto", onto, base])
        .success()
    {
        Ok(true) => {
            Ok(RebaseProbe::Clean)
        }
        Ok(false) => {
            // Capture *where* it broke before aborting: REBASE_HEAD is the
            // original commit being applied when the rebase stopped (it maps 1:1
            // to a commit in the stack, since we replay the originals), and the
            // `U`-state files are the conflicted paths at that commit.
            let commit = stopped_commit(&wt);
            let paths = conflicted_paths(&wt).unwrap_or_default();
            let _ = GitExecutor::new(&wt)
                .inert()
                .quiet()
                .args(["rebase", "--abort"])
                .status();
            Ok(RebaseProbe::Conflict { commit, paths })
        }
        Err(e) => {
            let _ = GitExecutor::new(&wt)
                .inert()
                .quiet()
                .args(["rebase", "--abort"])
                .status();
            Err(e)
        }
    }
}

/// Path of the reused scratch worktree for this repo, creating and registering
/// it if absent. Parked under the common git dir so it is per-repo and survives
/// across sessions; a stale registration (e.g. the dir was cleaned) is pruned
/// and recreated.
fn ensure_probe_worktree(main_workdir: &Path, common_dir: &Path, start: &str) -> Result<PathBuf> {
    let wt = common_dir.join("stacksaw").join("probe-worktree");
    if wt.join(".git").exists() {
        return Ok(wt);
    }
    if let Some(parent) = wt.parent() {
        fs::create_dir_all(parent).ok();
    }
    // Drop any stale registration pointing at a now-missing directory, then add.
    let _ = GitExecutor::new(main_workdir)
        .inert()
        .quiet()
        .args(["worktree", "prune"])
        .status();
    let wt_str = wt.to_string_lossy().to_string();
    GitExecutor::new(main_workdir)
        .inert()
        .args(["worktree", "add", "--detach", "--force", &wt_str, start])
        .run_captured()?;
    Ok(wt)
}

/// Return the scratch worktree to a clean detached checkout of `rev`, discarding
/// any leftover state from a prior probe (an aborted rebase, stray files).
fn reset_worktree(wt: &Path, rev: &str) -> Result<()> {
    if is_rebase_in_progress(wt) {
        let _ = GitExecutor::new(wt)
            .inert()
            .quiet()
            .args(["rebase", "--abort"])
            .status();
    }
    GitExecutor::new(wt)
        .inert()
        .args(["checkout", "--quiet", "--force", "--detach", rev])
        .run_captured()?;
    let _ = GitExecutor::new(wt)
        .inert()
        .quiet()
        .args(["clean", "-qfdx"])
        .status();
    Ok(())
}

/// The original commit whose replay stopped the rebase, via `REBASE_HEAD` (git
/// points it at the commit being applied when it halts). `None` if the ref is
/// absent (e.g. the apply backend, or git reported a conflict without it).
fn stopped_commit(wt: &Path) -> Option<String> {
    let out = GitExecutor::new(wt)
        .inert()
        .args(["rev-parse", "--verify", "--quiet", "REBASE_HEAD"])
        .run_captured()
        .ok()?;
    (!out.is_empty()).then_some(out)
}

/// Files left in a conflicted (`U`) state after a failed replay.
fn conflicted_paths(wt: &Path) -> Result<Vec<String>> {
    let out = GitExecutor::new(wt)
        .inert()
        .args(["diff", "--name-only", "--diff-filter=U"])
        .run_captured()?;
    Ok(out
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect())
}

fn is_rebase_in_progress(wt: &Path) -> bool {
    if let Some(git_dir) = get_worktree_git_dir(wt) {
        git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists()
    } else {
        true
    }
}

fn get_worktree_git_dir(wt: &Path) -> Option<PathBuf> {
    let git_file = wt.join(".git");
    if !git_file.exists() {
        return None;
    }
    let content = fs::read_to_string(git_file).ok()?;
    let path_str = content.strip_prefix("gitdir:")?.trim();
    Some(PathBuf::from(path_str))
}
