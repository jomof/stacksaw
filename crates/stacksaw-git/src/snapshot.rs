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
    // has uncommitted changes (§8.4 `✎` chip), and surface those changes as a
    // virtual commit at the branch tip (§8.3) so they're browsable like any
    // other commit.
    if let (Some(workdir), Ok(Some(branch))) = (repo.workdir(), repo.head_branch()) {
        let dirty = is_worktree_dirty(&workdir).unwrap_or(false);
        if dirty {
            let (added, deleted) = worktree_churn(&workdir).unwrap_or((0, 0));
            for s in &mut staircases {
                if let Some(seg) = s.segments.iter_mut().find(|seg| seg.branch == branch) {
                    s.dirty = true;
                    seg.commits.push(worktree_commit(added, deleted));
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

/// The virtual "uncommitted changes" commit for the tip of the current branch.
/// Carries the sentinel [`WORKTREE_OID`]; the UI renders it distinctly and the
/// host resolves its files/diffs against the working tree.
fn worktree_commit(added: u32, deleted: u32) -> stacksaw_ssp::types::CommitSummary {
    use stacksaw_ssp::types::{CommitSummary, FindingCounts, WORKTREE_OID};
    CommitSummary {
        oid: WORKTREE_OID.to_string(),
        short: WORKTREE_OID.to_string(),
        subject: "Uncommitted changes".to_string(),
        author: String::new(),
        author_time: 0,
        parents: vec![],
        change_id: None,
        finding_counts: FindingCounts::default(),
        twins: vec![],
        added,
        deleted,
    }
}

/// Total lines added/deleted in the working tree vs `HEAD` (tracked changes),
/// used as the churn for the virtual worktree commit.
fn worktree_churn(workdir: &Path) -> Result<(u32, u32)> {
    let out = git(workdir, &["diff", "HEAD", "--numstat", "-M"])?;
    let (mut add, mut del) = (0u32, 0u32);
    for line in out.lines() {
        let mut parts = line.split('\t');
        if let (Some(a), Some(d)) = (parts.next(), parts.next()) {
            add += a.parse::<u32>().unwrap_or(0);
            del += d.parse::<u32>().unwrap_or(0);
        }
    }
    Ok((add, del))
}

/// Fill in each commit's `added`/`deleted` line totals using a single
/// `git show --numstat` over every displayed commit (one process, not one per
/// commit). Failures leave the counts at zero. The virtual worktree commit is
/// skipped (its churn is filled in at injection time).
fn annotate_commit_stats(workdir: &Path, staircases: &mut [stacksaw_ssp::types::Staircase]) {
    let oids: Vec<String> = staircases
        .iter()
        .flat_map(|s| s.segments.iter())
        .flat_map(|seg| seg.commits.iter())
        .map(|c| c.oid.clone())
        .filter(|oid| oid != stacksaw_ssp::types::WORKTREE_OID)
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
    if rev == stacksaw_ssp::types::WORKTREE_OID {
        return worktree_changed_files(workdir);
    }
    // `git show --name-status` diffs against the first parent and, for a root
    // commit, lists the whole tree as added — exactly what the column wants.
    let status_out = git(workdir, &["show", "--name-status", "--format=", "-M", rev])?;
    // `--numstat` gives `added\tdeleted\tpath` per file (binary files use `-`).
    let numstat_out = git(workdir, &["show", "--numstat", "--format=", "-M", rev])?;
    let counts = parse_numstat(&numstat_out);
    Ok(parse_name_status(&status_out, &counts))
}

/// The files changed in the working tree vs `HEAD` (§8.3 virtual worktree
/// commit): tracked adds/mods/dels/renames from `git diff HEAD`, plus untracked
/// files (listed as added, with their line count).
fn worktree_changed_files(workdir: &Path) -> Result<Vec<FileEntry>> {
    let status_out = git(workdir, &["diff", "HEAD", "--name-status", "-M"])?;
    let numstat_out = git(workdir, &["diff", "HEAD", "--numstat", "-M"])?;
    let counts = parse_numstat(&numstat_out);
    let mut files = parse_name_status(&status_out, &counts);

    // Untracked files never appear in `git diff HEAD`; list them as additions.
    if let Ok(others) = git(workdir, &["ls-files", "--others", "--exclude-standard"]) {
        for path in others.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let added = std::fs::read_to_string(workdir.join(path))
                .map(|c| c.lines().count() as u32)
                .unwrap_or(0);
            files.push(FileEntry {
                status: "A".to_string(),
                path: path.to_string(),
                added,
                deleted: 0,
            });
        }
    }
    Ok(files)
}

/// Parse `git ... --name-status` output into [`FileEntry`]s, pulling per-file
/// line counts from a `parse_numstat` map.
fn parse_name_status(
    status_out: &str,
    counts: &std::collections::HashMap<String, (u32, u32)>,
) -> Vec<FileEntry> {
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
    files
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
    if rev == stacksaw_ssp::types::WORKTREE_OID {
        // Working tree vs HEAD, full context, for the virtual worktree commit.
        return git(
            workdir,
            &[
                "diff",
                "HEAD",
                "-M",
                "--no-color",
                "--unified=100000",
                "--",
                path,
            ],
        );
    }
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
/// where a diff would just be every line prefixed with `+`. For the virtual
/// worktree commit the content is read from disk (covers untracked files).
pub fn file_content(workdir: &Path, rev: &str, path: &str) -> Result<String> {
    if rev == stacksaw_ssp::types::WORKTREE_OID {
        return Ok(std::fs::read_to_string(workdir.join(path)).unwrap_or_default());
    }
    git(workdir, &["show", &format!("{rev}:{path}")])
}

/// The full commit message (subject + body) of `rev` (§8.1). Backs the virtual
/// "commit message" row shown at the top of the Files column. The virtual
/// worktree commit has no message, so a short explanatory note is shown.
pub fn commit_message(workdir: &Path, rev: &str) -> Result<String> {
    if rev == stacksaw_ssp::types::WORKTREE_OID {
        return Ok(
            "Uncommitted changes\n\nThese edits are in your working tree and have not been \
             committed yet."
                .to_string(),
        );
    }
    git(workdir, &["show", "-s", "--format=%B", rev])
}
