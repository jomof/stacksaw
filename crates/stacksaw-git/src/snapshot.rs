//! Builds the immutable [`Snapshot`] DTO (§2, §5.3) from live repo state.

use std::collections::HashMap;
use std::path::Path;

use std::fs;

use stacksaw_ssp::git_ref::GitRef;
use stacksaw_ssp::types::{
    CommitSummary, ConflictInfo, FileEntry, FileStatus, FindingCounts, RebaseStatus, Snapshot,
    Staircase, SCHEMA_VERSION, WORKTREE_OID,
};

use crate::error::Result;
use crate::model::{build_staircases, ModelOptions};
use crate::numstat::NumstatParser;
use crate::rebase_probe::{probe_rebase, RebaseProbe};
use crate::refs::git;
use crate::repo::Repo;

/// Build a full snapshot at the given generation number (§5.3).
pub fn build_snapshot(repo: &Repo, generation: u64, opts: &ModelOptions) -> Result<Snapshot> {
    let head = repo.head_oid()?.map(|o| GitRef::new(o.to_string()));
    let detached = repo.is_detached().unwrap_or(false);
    let mut staircases = build_staircases(repo, opts)?;

    // Mark the staircase representing HEAD as dirty if the worktree has
    // uncommitted changes (§8.4 `✎` chip), and surface those changes as a
    // virtual commit at its tip (§8.3) so they're browsable like any other
    // commit. HEAD is keyed by the same `head_ref` used to build its staircase —
    // the branch name, or the short HEAD oid when detached — so uncommitted work
    // shows even on a detached HEAD.
    if let (Some(workdir), Ok(Some(head_ref))) = (repo.workdir(), repo.head_ref_label()) {
        let status = worktree_status(&workdir).unwrap_or_default();
        if status.dirty {
            for s in &mut staircases {
                if let Some(seg) = s
                    .segments
                    .iter_mut()
                    .find(|seg| seg.branch.short() == head_ref)
                {
                    s.dirty = true;
                    seg.commits
                        .push(worktree_commit(status.added, status.deleted));
                    break;
                }
            }
        }
    }

    // Fill in per-commit line stats (added/deleted) in one batched git call.
    if let Some(workdir) = repo.workdir() {
        annotate_commit_stats(&workdir, &mut staircases);
    }

    // NB: the rebase-onto-upstream verdict (`Staircase::rebase`) is *not* filled
    // in here — probing shells out to a real (isolated) rebase, which is far too
    // slow for the hot snapshot path. Interactive callers run it in the
    // background (see the host's rebase prober); one-shot callers that want it
    // synchronously call [`annotate_rebase`].

    Ok(Snapshot {
        schema_version: SCHEMA_VERSION,
        generation,
        head,
        detached,
        staircases,
    })
}

/// Synchronously fill in each behind staircase's rebase-onto-upstream verdict by
/// probing it (see [`probe_stair_rebase`]). Blocks on a real rebase per behind
/// stack, so it is for one-shot callers (the CLI) — interactive callers probe in
/// the background instead.
pub fn annotate_rebase(repo: &Repo, staircases: &mut [Staircase]) {
    let Some(workdir) = repo.workdir() else {
        return;
    };
    let common = repo.common_dir();
    for s in staircases.iter_mut() {
        // A dangling child (amended parent) is probed as a restack; otherwise a
        // behind stack is probed as a rebase onto its upstream.
        let oids = if s.segments.iter().any(|seg| seg.stale) {
            restack_probe_oids(s)
        } else if s.behind > 0 {
            rebase_probe_oids(repo, s)
        } else {
            None
        };
        if let Some((onto, base, tip)) = oids {
            match probe_rebase(&workdir, &common, &onto, &base, &tip) {
                Ok(RebaseProbe::Clean) => {
                    s.rebase = RebaseStatus::Clean;
                    s.conflict = None;
                }
                Ok(RebaseProbe::Conflict { commit, paths }) => {
                    s.rebase = RebaseStatus::Conflict;
                    s.conflict = Some(ConflictInfo {
                        commit: commit.unwrap_or_default(),
                        paths,
                    });
                }
                Ok(RebaseProbe::UpToDate) | Err(_) => {
                    s.rebase = RebaseStatus::Unknown;
                    s.conflict = None;
                }
            }
        }
    }
}

/// The oids a rebase-onto-upstream probe needs for staircase `s`: `(onto, base,
/// tip)` — the upstream tip, the fork point, and the stack tip. `None` when the
/// stack has no commits or a rev fails to resolve. Exposed so the host's
/// background prober can key and run probes without re-deriving this.
pub fn rebase_probe_oids(repo: &Repo, s: &Staircase) -> Option<(String, String, String)> {
    // The stack tip is the child-most commit of the deepest segment that has any
    // commits (linear stacks: the last segment; a tree: its deepest leaf).
    let tip = s.segments.iter().rev().find_map(|seg| seg.commits.last())?;
    let upstream_oid = repo.resolve(s.upstream.full()).ok()?;
    let tip_oid = repo.resolve(&tip.oid).ok()?;
    let base = repo.merge_base(tip_oid, upstream_oid).ok()?;
    Some((
        upstream_oid.to_string(),
        base.to_string(),
        tip_oid.to_string(),
    ))
}

/// The oids a *restack* probe needs for a staircase whose first stale segment
/// dangles on an amended parent: `(onto, base, tip)` — the parent's new tip, the
/// stale segment's former base, and the stack tip. Replaying `base..tip` onto
/// `onto` simulates restacking the dangling children. `None` when the staircase
/// has no stale segment or the shape is unexpected. Pure over the DTO — the
/// commits already carry the needed oids.
pub fn restack_probe_oids(s: &Staircase) -> Option<(String, String, String)> {
    let stale = s.segments.iter().find(|seg| seg.stale)?;
    let parent = s.segments.get(stale.parent?)?;
    let onto = parent.commits.last()?.oid.clone(); // parent's new (amended) tip
    let base = stale.commits.first()?.parents.first()?.clone(); // former parent tip
    let tip = s
        .segments
        .iter()
        .rev()
        .find_map(|seg| seg.commits.last())?
        .oid
        .clone();
    Some((onto, base, tip))
}

/// The virtual "uncommitted changes" commit for the tip of the current branch.
/// Carries the sentinel [`WORKTREE_OID`]; the UI renders it distinctly and the
/// host resolves its files/diffs against the working tree.
fn worktree_commit(added: u32, deleted: u32) -> CommitSummary {
    CommitSummary {
        oid: WORKTREE_OID.to_string(),
        short: WORKTREE_OID.to_string(),
        subject: "Uncommitted changes".to_string(),
        author: String::new(),
        author_time: 0,
        parents: vec![],
        change_id: None,
        patch_id: None,
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
    let entries = NumstatParser::parse(&out);
    let add = entries.iter().map(|e| e.added).sum();
    let del = entries.iter().map(|e| e.deleted).sum();
    Ok((add, del))
}

/// Fill in each commit's `added`/`deleted` line totals using a single
/// `git show --numstat` over every displayed commit (one process, not one per
/// commit). Failures leave the counts at zero. The virtual worktree commit is
/// skipped (its churn is filled in at injection time).
fn annotate_commit_stats(workdir: &Path, staircases: &mut [Staircase]) {
    let oids: Vec<String> = staircases
        .iter()
        .flat_map(|s| s.segments.iter())
        .flat_map(|seg| seg.commits.iter())
        .map(|c| c.oid.clone())
        .filter(|oid| oid != WORKTREE_OID)
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
    let mut totals: HashMap<&str, (u32, u32)> = HashMap::new();
    for record in out.split('\u{1e}') {
        let record = record.trim_start_matches('\n');
        if record.is_empty() {
            continue;
        }
        let (hash, numstat) = record.split_once('\n').unwrap_or((record, ""));
        if hash.is_empty() {
            continue;
        }
        let entries = NumstatParser::parse(numstat);
        let add = entries.iter().map(|e| e.added).sum();
        let del = entries.iter().map(|e| e.deleted).sum();
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

#[derive(Default)]
struct WorktreeStatus {
    dirty: bool,
    added: u32,
    deleted: u32,
    has_tracked_changes: bool,
}

/// Determine if the worktree is dirty and compute line churn for tracked files.
/// Uses one or two git calls depending on the state, optimizing for the clean
/// and untracked-only cases.
fn worktree_status(workdir: &Path) -> Result<WorktreeStatus> {
    let out = git(workdir, &["status", "--porcelain"])?;
    if out.is_empty() {
        return Ok(WorktreeStatus::default());
    }

    let mut status = WorktreeStatus {
        dirty: true,
        ..Default::default()
    };

    // Untracked files start with '?? '; anything else is a tracked change.
    status.has_tracked_changes = out.lines().any(|l| !l.starts_with("?? "));

    if status.has_tracked_changes {
        let (add, del) = worktree_churn(workdir).unwrap_or((0, 0));
        status.added = add;
        status.deleted = del;
    }

    Ok(status)
}

/// True when `git status --porcelain` reports any changes.
pub fn is_worktree_dirty(workdir: &Path) -> Result<bool> {
    Ok(worktree_status(workdir)?.dirty)
}

/// The files changed by a commit vs its first parent (§8.1 Files column), each
/// with its added/deleted line counts. Root commits show every file as added.
/// `rev` may be any revspec (oid, ref).
pub fn changed_files(workdir: &Path, rev: &str) -> Result<Vec<FileEntry>> {
    if rev == WORKTREE_OID {
        return worktree_changed_files(workdir);
    }
    // Combine name-status and numstat into a single call using --summary.
    // Numstat provides line counts and path (including renames), and summary
    // provides the extra info needed to distinguish Added from Modified.
    let out = git(
        workdir,
        &["show", "--numstat", "--summary", "--format=", "-M", rev],
    )?;
    Ok(parse_combined_status(&out))
}

fn parse_combined_status(out: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    let mut map: HashMap<String, usize> = HashMap::new();

    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() == 3 {
            // Numstat line: added \t deleted \t path
            let added = parts[0].parse::<u32>().unwrap_or(0);
            let deleted = parts[1].parse::<u32>().unwrap_or(0);
            let raw_path = parts[2];
            let path = NumstatParser::normalize_path(raw_path);
            let status = if raw_path.contains(" => ") {
                FileStatus::Renamed
            } else {
                FileStatus::Modified
            };

            map.insert(path.clone(), entries.len());
            entries.push(FileEntry {
                path,
                added,
                deleted,
                status,
            });
        } else if line.starts_with("create mode") {
            if let Some(path) = line.split_whitespace().last() {
                if let Some(&idx) = map.get(path) {
                    entries[idx].status = FileStatus::Added;
                }
            }
        } else if line.starts_with("delete mode") {
            if let Some(path) = line.split_whitespace().last() {
                if let Some(&idx) = map.get(path) {
                    entries[idx].status = FileStatus::Deleted;
                }
            }
        }
    }
    entries
}

/// The files changed in the working tree vs `HEAD` (§8.3 virtual worktree
/// commit): tracked adds/mods/dels/renames from `git diff HEAD`, plus untracked
/// files (listed as added, with their line count).
fn worktree_changed_files(workdir: &Path) -> Result<Vec<FileEntry>> {
    // Use --numstat and --summary to get tracked changes in one call.
    let out = git(workdir, &["diff", "HEAD", "--numstat", "--summary", "-M"])?;
    let mut files = parse_combined_status(&out);

    // Untracked files never appear in `git diff HEAD`; list them as additions.
    if let Ok(others) = git(workdir, &["ls-files", "--others", "--exclude-standard"]) {
        for path in others.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let added = fs::read_to_string(workdir.join(path))
                .map(|c| c.lines().count() as u32)
                .unwrap_or(0);
            files.push(FileEntry {
                status: FileStatus::Added,
                path: path.to_string(),
                added,
                deleted: 0,
            });
        }
    }
    Ok(files)
}

/// The unified diff for a single `path` introduced by commit `rev`, vs its
/// first parent (§8.5 Diff column). Uses effectively unlimited context
/// (`--unified=100000`) so the Diff column can show the whole file with the
/// changed lines highlighted, not just the hunks around them. Returns the raw
/// `git show` patch body for just that pathspec (empty when unchanged there).
pub fn file_diff(workdir: &Path, rev: &str, path: &str) -> Result<String> {
    if rev == WORKTREE_OID {
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
    if rev == WORKTREE_OID {
        return Ok(fs::read_to_string(workdir.join(path)).unwrap_or_default());
    }
    git(workdir, &["show", &format!("{rev}:{path}")])
}

/// The full commit message (subject + body) of `rev` (§8.1). Backs the virtual
/// "commit message" row shown at the top of the Files column. The virtual
/// worktree commit has no message, so a short explanatory note is shown.
pub fn commit_message(workdir: &Path, rev: &str) -> Result<String> {
    if rev == WORKTREE_OID {
        return Ok(
            "Uncommitted changes\n\nThese edits are in your working tree and have not been              committed yet."
                .to_string(),
        );
    }
    git(workdir, &["show", "-s", "--format=%B", rev])
}
