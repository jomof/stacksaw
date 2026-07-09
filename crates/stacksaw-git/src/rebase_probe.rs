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

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{GitError, Result};

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

/// Config flags that keep the probe inert: no hooks fire, `rerere` neither
/// consults nor records, and nothing tries to sign. Applied to every probe git
/// call via `-c`.
const INERT: &[&str] = &[
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "rerere.enabled=false",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "advice.mergeConflict=false",
];

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
    let wt = ensure_probe_worktree(main_workdir, common_dir, tip)?;
    reset_worktree(&wt, tip)?;

    match run_probe_git(&wt, &["rebase", "--onto", onto, base]) {
        Ok(true) => {
            // Leave the worktree in a known state for the next probe.
            let _ = reset_worktree(&wt, tip);
            Ok(RebaseProbe::Clean)
        }
        Ok(false) => {
            // Capture *where* it broke before aborting: REBASE_HEAD is the
            // original commit being applied when the rebase stopped (it maps 1:1
            // to a commit in the stack, since we replay the originals), and the
            // `U`-state files are the conflicted paths at that commit.
            let commit = stopped_commit(&wt);
            let paths = conflicted_paths(&wt).unwrap_or_default();
            let _ = run_probe_git(&wt, &["rebase", "--abort"]);
            Ok(RebaseProbe::Conflict { commit, paths })
        }
        Err(e) => {
            let _ = run_probe_git(&wt, &["rebase", "--abort"]);
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
    let _ = run_probe_git(main_workdir, &["worktree", "prune"]);
    let wt_str = wt.to_string_lossy().to_string();
    run_probe_git_checked(
        main_workdir,
        &["worktree", "add", "--detach", "--force", &wt_str, start],
    )?;
    Ok(wt)
}

/// Return the scratch worktree to a clean detached checkout of `rev`, discarding
/// any leftover state from a prior probe (an aborted rebase, stray files).
fn reset_worktree(wt: &Path, rev: &str) -> Result<()> {
    let _ = run_probe_git(wt, &["rebase", "--abort"]);
    run_probe_git_checked(wt, &["checkout", "--quiet", "--force", "--detach", rev])?;
    run_probe_git_checked(wt, &["reset", "--quiet", "--hard", rev])?;
    let _ = run_probe_git(wt, &["clean", "-qfdx"]);
    Ok(())
}

/// The original commit whose replay stopped the rebase, via `REBASE_HEAD` (git
/// points it at the commit being applied when it halts). `None` if the ref is
/// absent (e.g. the apply backend, or git reported a conflict without it).
fn stopped_commit(wt: &Path) -> Option<String> {
    let out = capture_probe_git(wt, &["rev-parse", "--verify", "--quiet", "REBASE_HEAD"]).ok()?;
    let oid = out.trim();
    (!oid.is_empty()).then(|| oid.to_string())
}

/// Files left in a conflicted (`U`) state after a failed replay.
fn conflicted_paths(wt: &Path) -> Result<Vec<String>> {
    let out = capture_probe_git(wt, &["diff", "--name-only", "--diff-filter=U"])?;
    Ok(out
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect())
}

/// Run a probe git command, returning `Ok(true)` on success, `Ok(false)` on a
/// nonzero exit (e.g. a rebase conflict), and `Err` only when git cannot be
/// spawned. Output is discarded.
fn run_probe_git(dir: &Path, args: &[&str]) -> Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(INERT)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

/// Like [`run_probe_git`] but errors on a nonzero exit (for setup steps that
/// must succeed, e.g. registering or resetting the worktree).
fn run_probe_git_checked(dir: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(INERT)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(GitError::Command {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(())
}

/// Run a probe git command and capture stdout, erroring on a nonzero exit.
fn capture_probe_git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(INERT)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(GitError::Command {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
