//! Builds the immutable [`Snapshot`] DTO (§2, §5.3) from live repo state.

use std::path::Path;

use stacksaw_ssp::types::{FileEntry, Snapshot, SCHEMA_VERSION};

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

/// The files changed by a commit vs its first parent (§8.1 Files column). Root
/// commits show every file as added. `rev` may be any revspec (oid, ref).
pub fn changed_files(workdir: &Path, rev: &str) -> Result<Vec<FileEntry>> {
    // `git show --name-status` diffs against the first parent and, for a root
    // commit, lists the whole tree as added — exactly what the column wants.
    let out = git(
        workdir,
        &["show", "--name-status", "--format=", "-M", rev],
    )?;
    let mut files = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let Some(status) = parts.next() else { continue };
        // Renames/copies emit `R100\told\tnew`; the new path is what we show.
        let path = parts.last().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        files.push(FileEntry {
            // Keep just the leading status letter (e.g. `R100` → `R`).
            status: status.chars().next().unwrap_or('?').to_string(),
            path,
        });
    }
    Ok(files)
}

/// The unified diff for a single `path` introduced by commit `rev`, vs its
/// first parent (§8.5 Diff column). Uses effectively unlimited context
/// (`--unified=100000`) so the Diff column can show the whole file with the
/// changed lines highlighted, not just the hunks around them. Returns the raw
/// `git show` patch body for just that pathspec (empty when unchanged there).
pub fn file_diff(workdir: &Path, rev: &str, path: &str) -> Result<String> {
    git(
        workdir,
        &[
            "show",
            "--format=",
            "-M",
            "--no-color",
            "--unified=100000",
            rev,
            "--",
            path,
        ],
    )
}

/// The full content of `path` as of commit `rev` (§8.5). Used for added files,
/// where a diff would just be every line prefixed with `+`.
pub fn file_content(workdir: &Path, rev: &str, path: &str) -> Result<String> {
    git(workdir, &["show", &format!("{rev}:{path}")])
}

/// The full commit message (subject + body) of `rev` (§8.1). Backs the virtual
/// "commit message" row shown at the top of the Files column.
pub fn commit_message(workdir: &Path, rev: &str) -> Result<String> {
    git(workdir, &["show", "-s", "--format=%B", rev])
}
