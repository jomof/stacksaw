//! Builds the immutable [`Snapshot`] DTO (§2, §5.3) from live repo state.

use stacksaw_ssp::types::{Snapshot, SCHEMA_VERSION};

use crate::error::Result;
use crate::model::{build_staircases, ModelOptions};
use crate::refs::git;
use crate::repo::Repo;

/// Build a full snapshot at the given generation number (§5.3).
pub fn build_snapshot(repo: &Repo, generation: u64, opts: &ModelOptions) -> Result<Snapshot> {
    let head = repo.head_oid()?.map(|o| o.to_string());
    let detached = repo.is_detached().unwrap_or(false);
    let mut staircases = build_staircases(repo, opts)?;

    // Mark the staircase containing the current branch as dirty if the worktree
    // has uncommitted changes (§8.4 `✎` chip).
    if let (Some(workdir), Ok(Some(branch))) = (repo.workdir(), repo.head_branch()) {
        let dirty = is_worktree_dirty(&workdir).unwrap_or(false);
        if dirty {
            for s in &mut staircases {
                if s.segments.iter().any(|seg| seg.branch == branch) {
                    s.dirty = true;
                }
            }
        }
    }

    Ok(Snapshot {
        schema_version: SCHEMA_VERSION,
        generation,
        head,
        detached,
        staircases,
    })
}

/// True when `git status --porcelain` reports any changes.
pub fn is_worktree_dirty(workdir: &std::path::Path) -> Result<bool> {
    let out = git(workdir, &["status", "--porcelain"])?;
    Ok(!out.trim().is_empty())
}
