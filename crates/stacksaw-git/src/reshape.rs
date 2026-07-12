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

use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::{GitError, Result};
use crate::model::{build_staircases, ModelOptions};
use crate::refs::{self, RefUpdate};
use crate::repo::Repo;
use stacksaw_ssp::git_ref::GitRef;

/// Which way to reshape the selected commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Indent,
    Unindent,
}

/// Everything needed to reverse a reshape or archive
#[derive(Debug, Clone)]
pub struct Undo {
    pub refs: Vec<RefUpdate>,
    pub head: Option<String>,
    pub checkout_head: bool,
}

fn record_refs(repo: &Repo) -> Result<HashMap<String, String>> {
    let git_repo = git_staircase::GitRepo::new(repo.workdir().unwrap_or_else(|| repo.git_dir()).to_path_buf());
    let stdout = git_repo.run(&["for-each-ref", "--format=%(refname) %(objectname)"])
        .map_err(|e| GitError::Other(e.to_string()))?;
    let mut refs = HashMap::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 2 {
            refs.insert(parts[0].to_string(), parts[1].to_string());
        }
    }
    Ok(refs)
}

fn diff_recorded_refs(before: &HashMap<String, String>, after: &HashMap<String, String>) -> Vec<RefUpdate> {
    let mut updates = Vec::new();
    let before_keys: std::collections::HashSet<_> = before.keys().collect();
    let after_keys: std::collections::HashSet<_> = after.keys().collect();
    let all_keys = before_keys.union(&after_keys);

    for refname in all_keys {
        let b = before.get(*refname);
        let a = after.get(*refname);
        if b != a {
            updates.push(RefUpdate {
                no_verify: false,
                name: GitRef::new((*refname).clone()),
                old: a.cloned(),
                new: b.cloned(),
            });
        }
    }
    updates
}

pub fn apply(repo: &Repo, opts: &ModelOptions, target_oid: &str, op: Op) -> Result<Option<Undo>> {
    let git_repo = git_staircase::GitRepo::new(repo.workdir().unwrap_or_else(|| repo.git_dir()).to_path_buf());
    let onto = opts.default_upstream.as_deref();
    
    let staircases = build_staircases(repo, opts)?;
    let stair = staircases
        .iter()
        .find(|s| {
            s.segments
                .iter()
                .any(|seg| seg.commits.iter().any(|c| c.oid == target_oid))
        })
        .ok_or_else(|| GitError::Other("commit is not in a staircase".into()))?;

    if stair.upstream.full().starts_with('(') {
        return Err(GitError::Other("stack has no upstream to reshape against".into()));
    }

    let segs = &stair.segments;
    for (i, seg) in segs.iter().enumerate() {
        let expect = if i == 0 { None } else { Some(i - 1) };
        if seg.parent != expect {
            return Err(GitError::Other("cannot reshape a forked staircase".into()));
        }
    }

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

    let refs_before = record_refs(repo)?;

    let resolve_sc = || -> Result<git_staircase::core::ResolvedStaircase> {
        if let Ok(s) = git_staircase::core::resolve_by_name(&git_repo, &stair.name) {
            Ok(s)
        } else {
            git_staircase::core::resolve_staircase(&git_repo, &stair.name, onto)
                .map_err(|e| GitError::Other(e.to_string()))?
                .map(|sel| sel.staircase)
                .ok_or_else(|| GitError::Other("failed to resolve staircase in git-staircase".into()))
        }
    };

    let mut staircase = resolve_sc()?;

    let old_set: std::collections::HashSet<usize> = boundaries.iter().copied().collect();
    let new_set: std::collections::HashSet<usize> = new_boundaries.iter().copied().collect();

    let removed: Vec<usize> = old_set.difference(&new_set).copied().collect();
    let added: Vec<usize> = new_set.difference(&old_set).copied().collect();

    for &r in &removed {
        let cut_oid = &seq[r - 1];
        let metadata = staircase.metadata();
        if let Some(si) = metadata.steps.iter().position(|s| &s.cut == cut_oid) {
            if si + 1 < metadata.steps.len() {
                git_staircase::core::join(
                    &git_repo,
                    &staircase,
                    si,
                    si + 1,
                    git_staircase::core::JoinOptions {
                        ref_action: git_staircase::core::JoinRefAction::Keep,
                    },
                )
                .map_err(|e| GitError::Other(e.to_string()))?;
                staircase = resolve_sc()?;
            }
        }
    }

    for &a in &added {
        let cut_oid = &seq[a - 1];
        let metadata = staircase.metadata();
        let mut step_index = None;
        let mut prev_cut = git_repo.resolve_commit(&metadata.target)
            .map_err(|e| GitError::Other(e.to_string()))?;
        for (i, step) in metadata.steps.iter().enumerate() {
            if cut_oid == &step.cut {
                step_index = Some(i);
                break;
            }
            if git_repo.is_ancestor(&prev_cut, cut_oid).unwrap_or(false)
                && git_repo.is_ancestor(cut_oid, &step.cut).unwrap_or(false)
            {
                step_index = Some(i);
                break;
            }
            prev_cut = step.cut.clone();
        }

        if let Some(si) = step_index {
            git_staircase::core::split(
                &git_repo,
                &staircase,
                si,
                cut_oid,
                None,
                git_staircase::core::SplitOptions { no_ref: false },
            )
            .map_err(|e| GitError::Other(e.to_string()))?;
            
            staircase = resolve_sc()?;
        }
    }



    let refs_after = record_refs(repo)?;
    let undo_refs = diff_recorded_refs(&refs_before, &refs_after);

    let head = repo.head_branch()?;
    let mut head_fix = None;
    let mut undo_head = None;
    if let Some(ref h) = head {
        let was_in_staircase = stair.segments.iter().any(|seg| seg.branch.leaf() == h);
        let is_still_in_staircase = staircase.metadata().steps.iter().any(|s| s.branch.as_ref() == Some(h));
        if was_in_staircase && !is_still_in_staircase {
            let final_metadata = staircase.metadata();
            if let Some(step) = final_metadata.steps.iter().find(|s| {
                git_repo.is_ancestor(target_oid, &s.cut).unwrap_or(false)
            }) {
                if let Some(ref final_branch) = step.branch {
                    head_fix = Some(final_branch.clone());
                    undo_head = Some(h.clone());
                }
            }
        }
    }

    if let Some(new_head) = &head_fix {
        let dir = repo_dir(repo);
        let _ = refs::git(
            &dir,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{new_head}")],
        );
        if let Some(ref h) = head {
            let _ = git_repo.run(&["branch", "-D", h]);
        }
    }

    Ok(Some(Undo {
        refs: undo_refs,
        head: undo_head,
        checkout_head: false,
    }))
}

pub fn undo(repo: &Repo, u: &Undo) -> Result<()> {
    let dir = repo_dir(repo);
    refs::apply_transaction(&dir, &u.refs)?;
    if let Some(head) = &u.head {
        if u.checkout_head {
            let _ = refs::git(&dir, &["checkout", "-q", head]);
        } else {
            let _ = refs::git(
                &dir,
                &["symbolic-ref", "HEAD", &format!("refs/heads/{head}")],
            );
        }
    }
    Ok(())
}

fn repo_dir(repo: &Repo) -> PathBuf {
    repo.workdir().unwrap_or_else(|| repo.git_dir())
}

fn indent(boundaries: &[usize], n: usize, pos: usize) -> Option<Vec<usize>> {
    let hi = boundaries.iter().copied().filter(|&b| b >= pos).min()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_merges_a_step_into_the_next_one() {
        assert_eq!(indent(&[1, 2, 6], 6, 2), Some(vec![1, 6]));
        assert_eq!(indent(&[1, 3, 6], 6, 3), Some(vec![1, 2, 6]));
        assert_eq!(indent(&[1, 3, 6], 6, 2), Some(vec![1, 6]));
    }

    #[test]
    fn indent_cuts_a_new_step_on_a_single_branch() {
        assert_eq!(indent(&[6], 6, 3), Some(vec![2, 6]));
        assert_eq!(indent(&[2, 6], 6, 4), Some(vec![2, 3, 6]));
    }

    #[test]
    fn indent_no_ops_on_the_first_commit_of_a_single_branch() {
        assert_eq!(indent(&[6], 6, 1), None);
        assert_eq!(indent(&[3, 6], 6, 1), Some(vec![6]));
    }

    #[test]
    fn unindent_moves_a_step_head_into_the_prior_step() {
        assert_eq!(unindent(&[2, 4, 6], 5), Some(vec![2, 5, 6]));
        assert_eq!(unindent(&[2, 6], 4), Some(vec![4, 6]));
        assert_eq!(unindent(&[2, 4, 6], 6), Some(vec![2, 6]));
    }

    #[test]
    fn unindent_of_the_first_step_creates_a_prior_step() {
        assert_eq!(unindent(&[6], 3), Some(vec![3, 6]));
        assert_eq!(unindent(&[6], 6), None);
    }
}

