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

    // Fill in per-commit line stats (added/deleted) in one batched git call.
    if let Some(workdir) = repo.workdir() {
        annotate_commit_stats(&workdir, &mut staircases);
    }

    Ok(Snapshot {
        schema_version: SCHEMA_VERSION,
        generation,
        head,
        detached,
        staircases,
    })
}

/// Fill in each commit's `added`/`deleted` line totals using a single
/// `git show --numstat` over every displayed commit (one process, not one per
/// commit). Failures leave the counts at zero.
fn annotate_commit_stats(workdir: &Path, staircases: &mut [stacksaw_ssp::types::Staircase]) {
    let oids: Vec<String> = staircases
        .iter()
        .flat_map(|s| s.segments.iter())
        .flat_map(|seg| seg.commits.iter())
        .map(|c| c.oid.clone())
        .collect();
    if oids.is_empty() {
        return;
    }
    let mut args: Vec<&str> = vec!["show", "--numstat", "--format=%x1e%H", "-M"];
    args.extend(oids.iter().map(String::as_str));
    let Ok(out) = git(workdir, &args) else {
        return;
    };

    // Records are separated by RS (\x1e); each starts with the full hash on its
    // own line, followed by numstat rows.
    let mut totals: std::collections::HashMap<&str, (u32, u32)> = std::collections::HashMap::new();
    for record in out.split('\u{1e}') {
        let record = record.trim_start_matches('\n');
        let mut lines = record.lines();
        let Some(hash) = lines.next() else { continue };
        if hash.is_empty() {
            continue;
        }
        let (mut add, mut del) = (0u32, 0u32);
        for line in lines {
            let mut parts = line.split('\t');
            if let (Some(a), Some(d)) = (parts.next(), parts.next()) {
                add += a.parse::<u32>().unwrap_or(0);
                del += d.parse::<u32>().unwrap_or(0);
            }
        }
        totals.insert(hash, (add, del));
    }

    for s in staircases.iter_mut() {
        for seg in &mut s.segments {
            for c in &mut seg.commits {
                if let Some(&(add, del)) = totals.get(c.oid.as_str()) {
                    c.added = add;
                    c.deleted = del;
                }
            }
        }
    }
}

/// True when `git status --porcelain` reports any changes.
pub fn is_worktree_dirty(workdir: &std::path::Path) -> Result<bool> {
    let out = git(workdir, &["status", "--porcelain"])?;
    Ok(!out.trim().is_empty())
}

/// The files changed by a commit vs its first parent (§8.1 Files column), each
/// with its added/deleted line counts. Root commits show every file as added.
/// `rev` may be any revspec (oid, ref).
pub fn changed_files(workdir: &Path, rev: &str) -> Result<Vec<FileEntry>> {
    // `git show --name-status` diffs against the first parent and, for a root
    // commit, lists the whole tree as added — exactly what the column wants.
    let status_out = git(workdir, &["show", "--name-status", "--format=", "-M", rev])?;
    // `--numstat` gives `added\tdeleted\tpath` per file (binary files use `-`).
    let numstat_out = git(workdir, &["show", "--numstat", "--format=", "-M", rev])?;
    let counts = parse_numstat(&numstat_out);

    let mut files = Vec::new();
    for line in status_out.lines() {
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
        let (added, deleted) = counts.get(&path).copied().unwrap_or((0, 0));
        files.push(FileEntry {
            // Keep just the leading status letter (e.g. `R100` → `R`).
            status: status.chars().next().unwrap_or('?').to_string(),
            path,
            added,
            deleted,
        });
    }
    Ok(files)
}

/// Parse `git --numstat` output into `path -> (added, deleted)`. Binary files
/// (`-`/`-`) map to `(0, 0)`. Rename rows (`old => new`) key on the new path.
fn parse_numstat(out: &str) -> std::collections::HashMap<String, (u32, u32)> {
    let mut map = std::collections::HashMap::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(a), Some(d), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let added = a.parse::<u32>().unwrap_or(0);
        let deleted = d.parse::<u32>().unwrap_or(0);
        map.insert(normalize_numstat_path(path), (added, deleted));
    }
    map
}

/// Normalize a numstat path, resolving rename notations to the new path:
/// `old => new` and the braced `pre/{old => new}/post` form.
fn normalize_numstat_path(path: &str) -> String {
    if let Some(open) = path.find('{') {
        if let Some(close) = path.find('}') {
            if let Some(arrow) = path[open..close].find(" => ") {
                let mid_start = open + arrow + " => ".len();
                let new_mid = &path[mid_start..close];
                return format!("{}{}{}", &path[..open], new_mid, &path[close + 1..]);
            }
        }
    }
    if let Some((_, new)) = path.split_once(" => ") {
        return new.to_string();
    }
    path.to_string()
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
