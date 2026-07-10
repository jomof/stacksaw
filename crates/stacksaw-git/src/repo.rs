//! Repository discovery and low-level reads via `gix` (§4: git reads use gix,
//! never libgit2).
//!
//! Two topology queries — [`Repo::merge_base`] and [`Repo::is_ancestor`] — are
//! the exception: they shell out to the user's native `git`, which resolves
//! them in milliseconds where the available gix rev-walk had to enumerate
//! history to the repository root (seconds per call on large repos).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gix::traverse::commit::simple::Sorting;
use gix::{ObjectId, Repository};

use crate::error::{GitError, Result};
use crate::refs;
use stacksaw_ssp::git_ref::GitRef;

/// The 7-char abbreviated form of an object id, for display and labels (e.g. a
/// detached HEAD's staircase name).
fn short_oid(oid: ObjectId) -> String {
    oid.to_string().chars().take(7).collect()
}

/// A branch and its resolved tip / upstream.
#[derive(Debug, Clone)]
pub struct BranchRef {
    /// Short name, e.g. `feat/wire-proto`.
    pub name: String,
    /// Full ref name, e.g. `refs/heads/feat/wire-proto`.
    pub full_name: GitRef,
    pub tip: ObjectId,
    /// Resolved upstream ref name if tracking is configured.
    pub upstream: Option<GitRef>,
}

/// Commit metadata we surface in snapshots (§5.3 `commit/get`).
#[derive(Debug, Clone)]
pub struct CommitMeta {
    pub oid: ObjectId,
    pub subject: String,
    pub body: String,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub parents: Vec<ObjectId>,
    pub change_id: Option<String>,
    pub patch_id: Option<String>,
}

impl CommitMeta {
    pub fn short(&self) -> String {
        self.oid.to_hex_with_len(8).to_string()
    }
}

/// A thin wrapper over an opened `Repository`.
pub struct Repo {
    inner: Repository,
}

impl Repo {
    /// Discover the repository at or above `path`.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let inner = gix::discover(path.as_ref())
            .map_err(|_| GitError::NotARepo(path.as_ref().to_path_buf()))?;
        Ok(Repo { inner })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let inner = gix::open(path.as_ref())
            .map_err(|_| GitError::NotARepo(path.as_ref().to_path_buf()))?;
        Ok(Repo { inner })
    }

    /// The common git dir. Linked worktrees share this, so it keys the core
    /// service (§3.1).
    pub fn common_dir(&self) -> PathBuf {
        self.inner.common_dir().to_path_buf()
    }

    pub fn git_dir(&self) -> PathBuf {
        self.inner.git_dir().to_path_buf()
    }

    /// The main worktree path, if this repo has one.
    pub fn workdir(&self) -> Option<PathBuf> {
        self.inner.work_dir().map(|p| p.to_path_buf())
    }

    /// Resolve `HEAD` to an oid, or `None` for an unborn branch.
    pub fn head_oid(&self) -> Result<Option<ObjectId>> {
        match self.inner.head_id() {
            Ok(id) => Ok(Some(id.detach())),
            Err(_) => Ok(None),
        }
    }

    /// The short name of the current branch, or `None` if detached/unborn.
    pub fn head_branch(&self) -> Result<Option<String>> {
        let head = self
            .inner
            .head()
            .map_err(|e| GitError::Reference(e.to_string()))?;
        Ok(head.referent_name().and_then(|n| {
            let r = GitRef::new(n.as_bstr().to_string());
            r.is_local_branch().then(|| r.leaf().to_string())
        }))
    }

    pub fn is_detached(&self) -> Result<bool> {
        let head = self
            .inner
            .head()
            .map_err(|e| GitError::Reference(e.to_string()))?;
        Ok(head.referent_name().is_none())
    }

    /// A stable label for the checked-out state, used to key the staircase that
    /// represents HEAD: the branch name when on a branch, otherwise (detached)
    /// the short HEAD oid. `None` for an unborn HEAD (no commit yet).
    pub fn head_ref_label(&self) -> Result<Option<String>> {
        if let Some(branch) = self.head_branch()? {
            return Ok(Some(branch));
        }
        Ok(self.head_oid()?.map(short_oid))
    }

    /// Enumerate local branches (`refs/heads/*`) with resolved tips/upstreams.
    pub fn local_branches(&self) -> Result<Vec<BranchRef>> {
        let platform = self
            .inner
            .references()
            .map_err(|e| GitError::Reference(e.to_string()))?;
        let iter = platform
            .prefixed("refs/heads/")
            .map_err(|e| GitError::Reference(e.to_string()))?;

        let mut out = Vec::new();
        for r in iter {
            let mut r = r.map_err(|e| GitError::Reference(e.to_string()))?;
            let full_name = r.name().as_bstr().to_string();
            let short = GitRef::new(&full_name).leaf().to_string();
            let tip = r
                .peel_to_id_in_place()
                .map_err(|e| GitError::Reference(e.to_string()))?
                .detach();
            let upstream = self.tracking_upstream(&short);
            out.push(BranchRef {
                name: short,
                full_name: GitRef::new(full_name),
                tip,
                upstream,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Resolve `branch.<name>.merge`/`remote` tracking to an upstream ref name.
    pub fn tracking_upstream(&self, branch: &str) -> Option<GitRef> {
        let config = self.inner.config_snapshot();
        let merge_key = format!("branch.{branch}.merge");
        let remote_key = format!("branch.{branch}.remote");
        let merge = config.string(merge_key.as_str()).map(|s| s.to_string())?;
        let remote = config.string(remote_key.as_str()).map(|s| s.to_string());
        let r_merge = GitRef::new(&merge);
        let merge_short = r_merge.leaf();
        match remote {
            Some(remote) if remote != "." => {
                Some(GitRef::new(format!("refs/remotes/{remote}/{merge_short}")))
            }
            _ => Some(GitRef::new(merge)),
        }
    }

    /// Resolve a revspec (branch name, ref, or oid) to an object id.
    pub fn resolve(&self, rev: &str) -> Result<ObjectId> {
        let spec = self
            .inner
            .rev_parse_single(rev)
            .map_err(|e| GitError::Revwalk(format!("resolve {rev:?}: {e}")))?;
        Ok(spec.detach())
    }

    /// The merge base of two commits (§2 segment base).
    ///
    /// Shells out to native `git merge-base` (see the module note): the previous
    /// gix rev-walk built a full ancestor set back to the repository root, which
    /// cost seconds per call on large histories.
    pub fn merge_base(&self, a: ObjectId, b: ObjectId) -> Result<ObjectId> {
        if a == b {
            return Ok(a);
        }
        let dir = self.workdir().unwrap_or_else(|| self.git_dir());
        let (a_hex, b_hex) = (a.to_string(), b.to_string());
        let stdout = refs::git(&dir, &["merge-base", &a_hex, &b_hex])?;
        let hex = stdout.trim();
        ObjectId::from_hex(hex.as_bytes())
            .map_err(|e| GitError::Odb(format!("parse merge base oid {hex:?}: {e}")))
    }

    /// Read commit metadata (message, author, parents, `Change-Id`).
    /// Compute git patch-ids for multiple commits in one batch (§2 twins).
    pub fn patch_ids(&self, oids: &[String]) -> Result<HashMap<String, String>> {
        if oids.is_empty() {
            return Ok(HashMap::new());
        }
        let dir = self.workdir().unwrap_or_else(|| self.git_dir());

        let mut show = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["show", "--no-color"])
            .args(oids)
            .stdout(Stdio::piped())
            .spawn()?;

        let patch_id = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .arg("patch-id")
            .stdin(show.stdout.take().unwrap())
            .output()?;

        let mut map = HashMap::new();
        for line in String::from_utf8_lossy(&patch_id.stdout).lines() {
            let mut parts = line.split_whitespace();
            if let (Some(patch_id), Some(oid)) = (parts.next(), parts.next()) {
                map.insert(oid.to_string(), patch_id.to_string());
            }
        }
        Ok(map)
    }

    pub fn commit_meta(&self, oid: ObjectId) -> Result<CommitMeta> {
        let commit = self
            .inner
            .find_commit(oid)
            .map_err(|e| GitError::Odb(format!("find commit {oid}: {e}")))?;
        let message_raw = commit
            .message_raw()
            .map_err(|e| GitError::Odb(e.to_string()))?
            .to_string();
        let (subject, body) = split_message(&message_raw);
        let author = commit.author().map_err(|e| GitError::Odb(e.to_string()))?;
        let author_time = author.time.seconds;
        let parents = commit.parent_ids().map(|id| id.detach()).collect();
        let change_id = extract_change_id(&message_raw);
        Ok(CommitMeta {
            oid,
            subject,
            body,
            author_name: author.name.to_string(),
            author_email: author.email.to_string(),
            author_time,
            parents,
            change_id,
            patch_id: None,
        })
    }

    /// Compute the ordered commits in `base..tip` (child-most last), stopping at
    /// `base`. Correct for the linear/tree-shaped stacks stacksaw targets (§2).
    pub fn commits_between(&self, base: ObjectId, tip: ObjectId) -> Result<Vec<ObjectId>> {
        if base == tip {
            return Ok(vec![]);
        }
        // Walk ancestors of tip, pruning `base` and everything below it. The
        // `selected` filter returns false to exclude a commit and its ancestry.
        let base_ref = base;
        let walk = self
            .inner
            .rev_walk([tip])
            .sorting(Sorting::BreadthFirst)
            .selected(move |id| id != base_ref.as_ref())
            .map_err(|e| GitError::Revwalk(e.to_string()))?;
        let mut oids = Vec::new();
        for info in walk {
            let info = info.map_err(|e| GitError::Revwalk(e.to_string()))?;
            oids.push(info.id().detach());
        }
        // rev-walk yields child-first; reverse to parent-before-child (§7.2).
        oids.reverse();
        Ok(oids)
    }

    /// All commits reachable from `tip`, ordered parent-before-child. Used when
    /// a branch has no resolvable upstream and we still want to show its full
    /// stack (§2, §8: the current branch is always visible).
    pub fn commits_reachable(&self, tip: ObjectId) -> Result<Vec<ObjectId>> {
        let walk = self
            .inner
            .rev_walk([tip])
            .sorting(Sorting::BreadthFirst)
            .all()
            .map_err(|e| GitError::Revwalk(e.to_string()))?;
        let mut oids = Vec::new();
        for info in walk {
            let info = info.map_err(|e| GitError::Revwalk(e.to_string()))?;
            oids.push(info.id().detach());
        }
        oids.reverse();
        Ok(oids)
    }

    /// True when `ancestor` is an ancestor of (or equal to) `descendant`.
    ///
    /// Shells out to native `git merge-base --is-ancestor` for the same
    /// performance reason as [`Repo::merge_base`]. That command exits `0` when
    /// the relation holds, `1` when it does not, and `>1` on a real error; we
    /// capture its output (rather than inheriting stdio) so nothing leaks onto
    /// the TUI's raw-mode terminal.
    pub fn is_ancestor(&self, ancestor: ObjectId, descendant: ObjectId) -> Result<bool> {
        if ancestor == descendant {
            return Ok(true);
        }
        let dir = self.workdir().unwrap_or_else(|| self.git_dir());
        let (a_hex, d_hex) = (ancestor.to_string(), descendant.to_string());
        let out = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["merge-base", "--is-ancestor", &a_hex, &d_hex])
            .output()?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            code => Err(GitError::Command {
                code: code.unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            }),
        }
    }

    /// Prior tip oids of `branch` from its reflog, newest first, EXCLUDING the
    /// current tip (and consecutive duplicates). Empty when the branch has no
    /// reflog. Used to recover a child's intended parent after that parent was
    /// amended/rebased — the child still descends from one of these old tips
    /// (§4 restack detection).
    pub fn reflog_oids(&self, branch: &str) -> Vec<ObjectId> {
        let dir = self.workdir().unwrap_or_else(|| self.git_dir());
        let refname = format!("refs/heads/{branch}");
        let Ok(out) = refs::git(&dir, &["reflog", "show", "--format=%H", &refname]) else {
            return Vec::new();
        };
        let mut oids: Vec<ObjectId> = out
            .lines()
            .filter_map(|l| ObjectId::from_hex(l.trim().as_bytes()).ok())
            .collect();
        if !oids.is_empty() {
            oids.remove(0); // the first entry is the current tip
        }
        oids.dedup();
        oids
    }
    /// Read the content of a file at a specific commit.
    pub fn read_blob(&self, commit_oid: ObjectId, path: &str) -> Result<Option<String>> {
        let commit = self
            .inner
            .find_commit(commit_oid)
            .map_err(|e| GitError::Odb(format!("find commit {commit_oid}: {e}")))?;
        let tree = commit
            .tree()
            .map_err(|e| GitError::Odb(format!("get tree for {commit_oid}: {e}")))?;

        let mut buf = Vec::new();
        let entry = match tree.lookup_entry_by_path(path, &mut buf) {
            Ok(Some(entry)) => entry,
            _ => return Ok(None),
        };

        let object = entry.object().map_err(|e| {
            GitError::Odb(format!("get object for path {path} in {commit_oid}: {e}"))
        })?;

        let blob = match object.try_into_blob() {
            Ok(blob) => blob,
            Err(_) => return Ok(None),
        };

        Ok(Some(String::from_utf8_lossy(&blob.data).to_string()))
    }
    /// Read the content of multiple files at a specific commit.
    pub fn read_blobs(&self, commit_oid: ObjectId, paths: &[&str]) -> Result<Vec<Option<String>>> {
        let commit = self
            .inner
            .find_commit(commit_oid)
            .map_err(|e| GitError::Odb(format!("find commit {commit_oid}: {e}")))?;
        let tree = commit
            .tree()
            .map_err(|e| GitError::Odb(format!("get tree for {commit_oid}: {e}")))?;

        let mut buf = Vec::new();
        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let entry = match tree.lookup_entry_by_path(path, &mut buf) {
                Ok(Some(entry)) => entry,
                _ => {
                    out.push(None);
                    continue;
                }
            };

            let object = match entry.object() {
                Ok(obj) => obj,
                Err(_) => {
                    out.push(None);
                    continue;
                }
            };

            let blob = match object.try_into_blob() {
                Ok(blob) => blob,
                Err(_) => {
                    out.push(None);
                    continue;
                }
            };

            out.push(Some(String::from_utf8_lossy(&blob.data).to_string()));
        }
        Ok(out)
    }
}

/// Split a commit message into (subject, body).
fn split_message(msg: &str) -> (String, String) {
    let msg = msg.trim_start_matches('\n');
    match msg.split_once('\n') {
        Some((subject, rest)) => (
            subject.trim_end().to_string(),
            rest.trim_start_matches('\n').to_string(),
        ),
        None => (msg.trim_end().to_string(), String::new()),
    }
}

/// Extract a Gerrit-style `Change-Id:` trailer for twin detection (§2).
fn extract_change_id(msg: &str) -> Option<String> {
    msg.lines().rev().find_map(|line| {
        line.strip_prefix("Change-Id:")
            .map(|v| v.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_subject_and_body() {
        let (s, b) = split_message("Add codec\n\nDetails here\nmore");
        assert_eq!(s, "Add codec");
        assert_eq!(b, "Details here\nmore");
    }

    #[test]
    fn extracts_change_id() {
        let msg = "Do thing\n\nBody\n\nChange-Id: Iabc123\n";
        assert_eq!(extract_change_id(msg).as_deref(), Some("Iabc123"));
    }
}
