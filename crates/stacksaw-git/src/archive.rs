//! Archive whole stacks by moving their branch refs out of `refs/heads/`.
//!
//! A "branch" is only a ref under `refs/heads/`, but Git treats *any* ref under
//! `refs/` as a reachability root. So archiving a stack is just parking each of
//! its `refs/heads/<name>` refs at `refs/stacksaw/archive/<name>`: the commits
//! stay reachable (safe from `gc`), but the branches disappear from the Stacks
//! model, which enumerates only `refs/heads/*` (§2). Restore later by hand with
//! `git branch <name> refs/stacksaw/archive/<name>`, or [`undo`] this session.
//!
//! Like reshape, archiving goes through the P4 mutation path: checkpoint the
//! affected heads, then one atomic `git update-ref --stdin` transaction. It
//! returns the inverse transaction (a [`reshape::Undo`]) so the host can undo it
//! with the same LIFO stack.

use std::path::PathBuf;

use crate::error::{GitError, Result};
use crate::refs::{self, RefUpdate};
use crate::repo::Repo;
use crate::reshape::Undo;

/// Prefix under which archived branch tips are parked.
pub const ARCHIVE_PREFIX: &str = "refs/stacksaw/archive";

/// Archive the given local branches: move each `refs/heads/<name>` to
/// `refs/stacksaw/archive/<name>` atomically. Names that are not real local
/// branches (e.g. a synthetic detached-HEAD row) are skipped. Returns
/// `Some(undo)` when refs moved, `None` when nothing did. Errors when HEAD sits
/// on one of the branches (archiving it would strand the checkout).
pub fn archive(repo: &Repo, branches: &[String]) -> Result<Option<Undo>> {
    let dir = repo_dir(repo);

    // Resolve the real heads and their current tips (authoritative, not from a
    // possibly-stale snapshot). Skip names without a live `refs/heads/` ref.
    let mut heads: Vec<(String, String)> = Vec::new();
    for name in branches {
        let full = format!("refs/heads/{name}");
        match refs::git(&dir, &["rev-parse", "--verify", "--quiet", &full]) {
            Ok(oid) if !oid.trim().is_empty() => heads.push((name.clone(), oid.trim().to_string())),
            _ => continue,
        }
    }
    if heads.is_empty() {
        return Ok(None);
    }

    // If HEAD is on a branch we're about to archive, land the user on the
    // stack's base branch first (dropping the head otherwise strands HEAD).
    // Recorded so undo checks the original branch back out.
    let mut head_restore: Option<String> = None;
    if let Some(h) = repo.head_branch()? {
        if heads.iter().any(|(name, _)| *name == h) {
            let Some(base) = landing_branch(repo, &dir, &h, &heads) else {
                return Err(GitError::Other(
                    "no local base branch to land on — check out elsewhere before archiving".into(),
                ));
            };
            if is_dirty(&dir)? {
                return Err(GitError::Other(
                    "commit or stash changes before archiving the checked-out stack".into(),
                ));
            }
            refs::git(&dir, &["checkout", "-q", &base])?;
            head_restore = Some(h.clone());
        }
    }

    // Checkpoint the heads (P4) before moving them.
    let existing: Vec<String> = heads.iter().map(|(name, _)| format!("refs/heads/{name}")).collect();
    let _ = refs::write_checkpoint(&dir, &existing);

    // Forward: create the archive ref, delete the head. Inverse: recreate the
    // head, delete the archive ref.
    let mut fwd = Vec::with_capacity(heads.len() * 2);
    let mut inv = Vec::with_capacity(heads.len() * 2);
    for (name, oid) in &heads {
        let head = format!("refs/heads/{name}");
        let arch = format!("{ARCHIVE_PREFIX}/{name}");
        fwd.push(RefUpdate {
            name: arch.clone(),
            old: None,
            new: Some(oid.clone()),
        });
        fwd.push(RefUpdate {
            name: head.clone(),
            old: Some(oid.clone()),
            new: None,
        });
        inv.push(RefUpdate {
            name: head,
            old: None,
            new: Some(oid.clone()),
        });
        inv.push(RefUpdate {
            name: arch,
            old: Some(oid.clone()),
            new: None,
        });
    }
    refs::apply_transaction(&dir, &fwd)?;

    Ok(Some(Undo {
        refs: inv,
        checkout_head: head_restore.is_some(),
        head: head_restore,
    }))
}

fn repo_dir(repo: &Repo) -> PathBuf {
    repo.workdir().unwrap_or_else(|| repo.git_dir())
}

/// The local branch to land on when archiving the checked-out stack: the
/// checked-out branch's upstream, resolved to a `refs/heads/*` branch that
/// exists and is not itself being archived. `None` when the upstream is a
/// remote-only tracking ref with no matching local branch.
fn landing_branch(repo: &Repo, dir: &std::path::Path, head: &str, heads: &[(String, String)]) -> Option<String> {
    let up = repo.tracking_upstream(head)?;
    // `refs/heads/main` → `main`; `refs/remotes/origin/main` → `main`.
    let cand = up
        .strip_prefix("refs/heads/")
        .map(str::to_string)
        .or_else(|| up.rsplit('/').next().map(str::to_string))?;
    if heads.iter().any(|(name, _)| *name == cand) {
        return None;
    }
    match refs::git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{cand}")]) {
        Ok(oid) if !oid.trim().is_empty() => Some(cand),
        _ => None,
    }
}

/// Whether the working tree has uncommitted changes (so a landing checkout would
/// clobber or fail).
fn is_dirty(dir: &std::path::Path) -> Result<bool> {
    Ok(!refs::git(dir, &["status", "--porcelain"])?.trim().is_empty())
}
