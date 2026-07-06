//! Reshape a linear staircase by moving branch boundaries (indent / unindent).
//!
//! The commit *sequence* is fixed — reshaping never reorders commits, it only
//! changes which commits are branch tips (P4). A staircase over commits
//! `C1…Cn` (oldest→newest) is described by a set of **boundary** positions (the
//! branch tips), with `Cn` always a boundary; the number of boundaries is the
//! number of branches.
//!
//! - **Indent** a commit: fold it (and any later commits of its step) *deeper*,
//!   toward the `feature` tip — merging its step into the next one, or, on a
//!   single (tip) branch, cutting a new step so it becomes its own branch.
//! - **Unindent** a commit: peel it (and any earlier commits of its step)
//!   *shallower*, toward the base — moving it to the prior branch, or creating a
//!   new base branch when there is none.
//!
//! The two are mirror images: indent drops a step's tip boundary and cuts before
//! the commit; unindent drops a step's base boundary and cuts at the commit.
//!
//! Branches are renamed to a contiguous `base-1 … base-(k-1) … base` scheme
//! derived from the stack's tip branch, so the tip keeps its identity and the
//! lower steps are numbered. Every change is checkpointed and applied through
//! one atomic ref transaction; [`apply`] returns the inverse transaction so the
//! host can [`undo`] it.

use std::path::PathBuf;

use crate::error::{GitError, Result};
use crate::model::{build_staircases, ModelOptions};
use crate::refs::{self, RefUpdate};
use crate::repo::Repo;

/// Which way to reshape the selected commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Indent,
    Unindent,
}

/// Everything needed to reverse a reshape or archive: the inverse ref
/// transaction plus the branch `HEAD` should be restored to (when the operation
/// moved it). `checkout_head` distinguishes a pure ref re-point (reshape renames
/// a tip, files unchanged → `symbolic-ref`) from a real checkout (archive landed
/// the user on the base branch, files changed → `git checkout`).
#[derive(Debug, Clone)]
pub struct Undo {
    pub refs: Vec<RefUpdate>,
    pub head: Option<String>,
    /// Restore `head` with a working-tree checkout rather than a bare
    /// `symbolic-ref` (used when the forward op changed the working tree).
    pub checkout_head: bool,
}

/// Apply an indent/unindent of `target_oid` in whichever staircase contains it.
/// Returns `Some(undo)` when refs moved, `None` when the op was a no-op. Errors
/// when the stack can't be reshaped (forked, no upstream, HEAD on a non-tip
/// branch of the stack, or `target_oid` unknown).
pub fn apply(
    repo: &Repo,
    opts: &ModelOptions,
    target_oid: &str,
    op: Op,
) -> Result<Option<Undo>> {
    let dir = repo_dir(repo);
    let staircases = build_staircases(repo, opts)?;
    let stair = staircases
        .iter()
        .find(|s| {
            s.segments
                .iter()
                .any(|seg| seg.commits.iter().any(|c| c.oid == target_oid))
        })
        .ok_or_else(|| GitError::Other("commit is not in a staircase".into()))?;

    // A synthetic (rootless / detached) stack has no real branches to renumber.
    if stair.upstream.starts_with('(') {
        return Err(GitError::Other(
            "stack has no upstream to reshape against".into(),
        ));
    }

    // Only linear stacks (a path of segments) can be indented; a fork has no
    // single "next" branch.
    let segs = &stair.segments;
    for (i, seg) in segs.iter().enumerate() {
        let expect = if i == 0 { None } else { Some(i - 1) };
        if seg.parent != expect {
            return Err(GitError::Other("cannot reshape a forked staircase".into()));
        }
    }

    // Flatten to the linear commit sequence and the current boundary positions
    // (1-indexed: each segment's tip is at the running commit count).
    let mut seq: Vec<String> = Vec::new();
    let mut boundaries: Vec<usize> = Vec::new();
    for seg in segs {
        for c in &seg.commits {
            seq.push(c.oid.clone());
        }
        boundaries.push(seq.len());
    }
    let n = seq.len();
    if n == 0 {
        return Ok(None);
    }
    let pos = seq
        .iter()
        .position(|o| o == target_oid)
        .ok_or_else(|| GitError::Other("commit not found in sequence".into()))?
        + 1;

    let Some(new_boundaries) = (match op {
        Op::Indent => indent(&boundaries, n, pos),
        Op::Unindent => unindent(&boundaries, pos),
    }) else {
        return Ok(None);
    };

    // Desired name→oid layout, and the current one, keyed by short branch name.
    let tip_branch = segs.last().unwrap().branch.clone();
    let base = strip_numeric_suffix(&tip_branch);
    let desired = layout(&seq, &new_boundaries, &base);
    let current: Vec<(String, String)> = segs
        .iter()
        .map(|seg| (seg.branch.clone(), seg.commits.last().unwrap().oid.clone()))
        .collect();

    // HEAD safety: reshaping renames/deletes lower branches, so only allow it
    // when HEAD is off this stack or sits on its tip (which always survives, at
    // the same commit; only its name may change to `base`).
    let head = repo.head_branch()?;
    let mut head_fix: Option<String> = None;
    let mut undo_head: Option<String> = None;
    if let Some(h) = &head {
        if current.iter().any(|(name, _)| name == h) {
            if *h != tip_branch {
                return Err(GitError::Other(
                    "check out the tip branch to reshape this stack".into(),
                ));
            }
            if base != *h {
                head_fix = Some(base.clone());
                undo_head = Some(h.clone());
            }
        }
    }

    // Forward + inverse ref updates over the union of current and desired names.
    let (updates, undo_refs) = diff_refs(&current, &desired);
    if updates.is_empty() {
        return Ok(None);
    }

    // Checkpoint the existing branches before moving them (P4), then apply.
    let existing: Vec<String> = current
        .iter()
        .map(|(name, _)| format!("refs/heads/{name}"))
        .collect();
    let _ = refs::write_checkpoint(&dir, &existing);
    refs::apply_transaction(&dir, &updates)?;

    // Keep the renumbered branches in the same upstream group (best-effort).
    if let Some(up) = tip_upstream(repo, &tip_branch) {
        for (name, _) in &desired {
            let _ = refs::git(&dir, &["branch", &format!("--set-upstream-to={up}"), name]);
        }
    }

    // Follow a tip rename so the checked-out branch stays valid.
    if let Some(new_head) = &head_fix {
        let _ = refs::git(&dir, &["symbolic-ref", "HEAD", &format!("refs/heads/{new_head}")]);
    }

    Ok(Some(Undo {
        refs: undo_refs,
        head: undo_head,
        checkout_head: false,
    }))
}

/// Reverse a reshape or archive by replaying its inverse ref transaction and
/// restoring HEAD (a bare re-point for a rename, a checkout when the working
/// tree had changed). Shared by both operations' undo stack.
pub fn undo(repo: &Repo, u: &Undo) -> Result<()> {
    let dir = repo_dir(repo);
    refs::apply_transaction(&dir, &u.refs)?;
    if let Some(head) = &u.head {
        if u.checkout_head {
            let _ = refs::git(&dir, &["checkout", "-q", head]);
        } else {
            let _ = refs::git(&dir, &["symbolic-ref", "HEAD", &format!("refs/heads/{head}")]);
        }
    }
    Ok(())
}

fn repo_dir(repo: &Repo) -> PathBuf {
    repo.workdir().unwrap_or_else(|| repo.git_dir())
}

/// Fold `[pos..]` toward the `feature` tip by cutting a new branch just before
/// `pos` (so the commits before it keep their step). When `pos`'s step has a
/// deeper neighbour its tip boundary is dropped too, merging the step into the
/// next one; on the tip step there is nothing to drop, so the cut simply creates
/// a new step. `None` when it would be a no-op (the very first commit, or no
/// change). Mirror image of [`unindent`].
fn indent(boundaries: &[usize], n: usize, pos: usize) -> Option<Vec<usize>> {
    let hi = boundaries.iter().copied().filter(|&b| b >= pos).min()?;
    // Drop this step's tip boundary only when a deeper step exists to absorb it.
    let mut set: Vec<usize> = boundaries
        .iter()
        .copied()
        .filter(|&b| !(b == hi && hi < n))
        .collect();
    if pos > 1 && !set.contains(&(pos - 1)) {
        set.push(pos - 1);
    }
    set.sort_unstable();
    set.dedup();
    if set == boundaries {
        None
    } else {
        Some(set)
    }
}

/// Move `[a..=pos]` (the head of `pos`'s step) into the prior step, creating a
/// prior step if there is none. `None` when it would be a no-op.
fn unindent(boundaries: &[usize], pos: usize) -> Option<Vec<usize>> {
    let prev = boundaries.iter().copied().filter(|&x| x < pos).max();
    let a = prev.map(|p| p + 1).unwrap_or(1);
    let mut set: Vec<usize> = boundaries.to_vec();
    if a > 1 {
        set.retain(|&x| x != a - 1);
    }
    if !set.contains(&pos) {
        set.push(pos);
    }
    set.sort_unstable();
    set.dedup();
    if set == boundaries {
        None
    } else {
        Some(set)
    }
}

/// The desired `name → tip-oid` layout for `boundaries`: base steps numbered
/// `base-1 … base-(k-1)`, the tip step named `base`.
fn layout(seq: &[String], boundaries: &[usize], base: &str) -> Vec<(String, String)> {
    let k = boundaries.len();
    boundaries
        .iter()
        .enumerate()
        .map(|(j, &pos)| {
            let name = if j + 1 < k {
                format!("{base}-{}", j + 1)
            } else {
                base.to_string()
            };
            (name, seq[pos - 1].clone())
        })
        .collect()
}

/// Forward and inverse ref updates to turn `current` into `desired`. A name in
/// both keeps or moves; only in `current` is deleted; only in `desired` is
/// created.
fn diff_refs(
    current: &[(String, String)],
    desired: &[(String, String)],
) -> (Vec<RefUpdate>, Vec<RefUpdate>) {
    use std::collections::BTreeMap;
    let before: BTreeMap<&str, &str> = current.iter().map(|(n, o)| (n.as_str(), o.as_str())).collect();
    let after: BTreeMap<&str, &str> = desired.iter().map(|(n, o)| (n.as_str(), o.as_str())).collect();
    let mut names: Vec<&str> = before.keys().chain(after.keys()).copied().collect();
    names.sort_unstable();
    names.dedup();

    let mut fwd = Vec::new();
    let mut inv = Vec::new();
    for name in names {
        let b = before.get(name).map(|s| s.to_string());
        let a = after.get(name).map(|s| s.to_string());
        if a == b {
            continue;
        }
        let full = format!("refs/heads/{name}");
        fwd.push(RefUpdate {
            name: full.clone(),
            old: b.clone(),
            new: a.clone(),
        });
        inv.push(RefUpdate {
            name: full,
            old: a,
            new: b,
        });
    }
    (fwd, inv)
}

/// Strip a trailing `-<digits>` so a numbered tip (`feature-3`) yields the base
/// (`feature`); names ending in a non-numeric segment (`my-stack`) are kept.
fn strip_numeric_suffix(name: &str) -> String {
    if let Some(idx) = name.rfind('-') {
        let suffix = &name[idx + 1..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return name[..idx].to_string();
        }
    }
    name.to_string()
}

/// The tip branch's upstream in short form (`origin/main`, `main`), for
/// re-tracking the renumbered branches.
fn tip_upstream(repo: &Repo, tip: &str) -> Option<String> {
    repo.tracking_upstream(tip).map(|u| {
        u.strip_prefix("refs/remotes/")
            .or_else(|| u.strip_prefix("refs/heads/"))
            .unwrap_or(&u)
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_merges_a_step_into_the_next_one() {
        // feature-1|feature-2|feature = [1][2][3..6]; indent C2 → [1][2..6].
        assert_eq!(indent(&[1, 2, 6], 6, 2), Some(vec![1, 6]));
        // A step's tip commit alone moves one step deeper.
        // [1][2,3][4..6]: indent C3 (tip of the middle step) → [1][2][3..6].
        assert_eq!(indent(&[1, 3, 6], 6, 3), Some(vec![1, 2, 6]));
        // A step's base commit takes its whole step deeper.
        // [1][2,3][4..6]: indent C2 → [1][2..6] (middle step gone).
        assert_eq!(indent(&[1, 3, 6], 6, 2), Some(vec![1, 6]));
    }

    #[test]
    fn indent_cuts_a_new_step_on_a_single_branch() {
        // A single branch is the tip step: indent C3 cuts before it, so
        // [c1,c2] become a step and [c3..c6] stay the tip.
        assert_eq!(indent(&[6], 6, 3), Some(vec![2, 6]));
        // Indenting a commit already in the tip step likewise peels a step off.
        assert_eq!(indent(&[2, 6], 6, 4), Some(vec![2, 3, 6]));
    }

    #[test]
    fn indent_no_ops_on_the_first_commit_of_a_single_branch() {
        // Nothing precedes C1 and there is no step to cut, so it is a no-op.
        assert_eq!(indent(&[6], 6, 1), None);
        // But C1 as the base step of a taller stack folds that step deeper.
        assert_eq!(indent(&[3, 6], 6, 1), Some(vec![6]));
    }

    #[test]
    fn unindent_moves_a_step_head_into_the_prior_step() {
        // First commit of the tip step moves up one.
        assert_eq!(unindent(&[2, 4, 6], 5), Some(vec![2, 5, 6]));
        // A later commit drags the earlier commits of its step along.
        assert_eq!(unindent(&[2, 6], 4), Some(vec![4, 6]));
        // The last commit of a step merges the whole step upward.
        assert_eq!(unindent(&[2, 4, 6], 6), Some(vec![2, 6]));
    }

    #[test]
    fn unindent_of_the_first_step_creates_a_prior_step() {
        // Single branch, unindent C3 → a new base step [1..3] appears.
        assert_eq!(unindent(&[6], 3), Some(vec![3, 6]));
        // Unindenting the tip of the sole step is a no-op.
        assert_eq!(unindent(&[6], 6), None);
    }

    #[test]
    fn names_number_lower_steps_and_leave_the_tip_bare() {
        let seq: Vec<String> = (1..=6).map(|i| format!("c{i}")).collect();
        let l = layout(&seq, &[2, 4, 6], "feature");
        assert_eq!(
            l,
            vec![
                ("feature-1".to_string(), "c2".to_string()),
                ("feature-2".to_string(), "c4".to_string()),
                ("feature".to_string(), "c6".to_string()),
            ]
        );
        // One boundary → just the bare tip name.
        assert_eq!(
            layout(&seq, &[6], "feature"),
            vec![("feature".to_string(), "c6".to_string())]
        );
    }

    #[test]
    fn strips_only_numeric_suffixes() {
        assert_eq!(strip_numeric_suffix("feature"), "feature");
        assert_eq!(strip_numeric_suffix("feature-3"), "feature");
        assert_eq!(strip_numeric_suffix("step-12"), "step");
        assert_eq!(strip_numeric_suffix("my-stack"), "my-stack");
        assert_eq!(strip_numeric_suffix("feat/use-proto"), "feat/use-proto");
    }
}
