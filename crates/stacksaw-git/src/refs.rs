//! Atomic ref transactions, checkpoints and undo (§4, §9.5, P4).
//!
//! Mutations shell out to the user's git so hooks, rerere, sequencer
//! semantics and --update-refs behave exactly as users expect. git2 is
//! intentionally not used.

use stacksaw_ssp::git_ref::GitRef;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use crate::error::{GitError, Result};

/// Prefix under which checkpoint refs are written (§9.5 step 1).
pub const CHECKPOINT_PREFIX: &str = "refs/stacksaw/checkpoints";

/// A single ref update in a transaction.
#[derive(Debug, Clone)]
pub struct RefUpdate {
    pub name: GitRef,
    /// Expected current value (optimistic concurrency); None for create.
    pub old: Option<String>,
    /// New value; None to delete.
    pub new: Option<String>,
    /// When true, use an unconditional update/delete (ignore `old`).
    pub no_verify: bool,
}

impl RefUpdate {
    pub fn set(name: impl Into<GitRef>, old: Option<String>, new: impl Into<String>) -> Self {
        RefUpdate {
            name: name.into(),
            old,
            new: Some(new.into()),
            no_verify: false,
        }
    }
}

/// Run git in repo_dir and capture stdout, erroring on nonzero exit.
pub fn git(repo_dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
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

/// Detect the system git version as (major, minor).
pub fn git_version(repo_dir: &Path) -> Result<(u32, u32)> {
    let text = git(repo_dir, &["--version"])?;
    // "git version 2.43.0"
    let ver = text
        .split_whitespace()
        .find(|s| s.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .ok_or_else(|| GitError::Other(format!("unparseable git version: {text:?}")))?;
    let mut parts = ver.split('.');
    let major = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Ok((major, minor))
}

/// True when the system git supports rebase --update-refs (≥ 2.38, §9.5).
pub fn supports_update_refs(repo_dir: &Path) -> Result<bool> {
    let (maj, min) = git_version(repo_dir)?;
    Ok(maj > 2 || (maj == 2 && min >= 38))
}

/// Apply a set of ref updates atomically via git update-ref --stdin
/// (§9.5 step 9). Either all updates apply or none do.
pub fn apply_transaction(repo_dir: &Path, updates: &[RefUpdate]) -> Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    let mut stdin = String::from("start\n");
    for u in updates {
        match (&u.new, &u.old) {
            (Some(new), _) if u.no_verify => {
                stdin.push_str(&format!("update {} {}\n", u.name, new));
            }
            (Some(new), Some(old)) => {
                stdin.push_str(&format!("update {} {} {}\n", u.name, new, old));
            }
            (Some(new), None) => {
                stdin.push_str(&format!("create {} {}\n", u.name, new));
            }
            (None, _) if u.no_verify => {
                stdin.push_str(&format!("delete {}\n", u.name));
            }
            (None, Some(old)) => {
                stdin.push_str(&format!("delete {} {}\n", u.name, old));
            }
            (None, None) => {
                return Err(GitError::Other(format!(
                    "ref update for {} has neither old nor new",
                    u.name
                )));
            }
        }
    }
    stdin.push_str("prepare\ncommit\n");

    use std::io::Write;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["update-ref", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut child_stdin) = child.stdin.take() {
        thread::spawn(move || {
            let _ = child_stdin.write_all(stdin.as_bytes());
        });
    }

    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(GitError::Command {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(())
}

/// A checkpoint: a timestamped snapshot of a set of refs (§9.5 step 1).
#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub id: String,
    pub refs: Vec<(GitRef, String)>,
}

/// Timestamp id form used in the ref namespace, e.g. 2026-07-04T18-40-12Z.
pub fn checkpoint_id_now() -> String {
    let now = jiff::Timestamp::now();
    // Colons are illegal in ref components; use dashes.
    now.strftime("%Y-%m-%dT%H-%M-%SZ").to_string()
}

/// Write a checkpoint for the given refs. Returns the checkpoint id.
pub fn write_checkpoint(repo_dir: &Path, ref_names: &[GitRef]) -> Result<Checkpoint> {
    let id = checkpoint_id_now();
    let mut updates = Vec::new();
    let mut saved = Vec::new();
    for name in ref_names {
        let oid = git(repo_dir, &["rev-parse", name.full()])?
            .trim()
            .to_string();
        let cp_ref = format!("{CHECKPOINT_PREFIX}/{id}/{}", name.leaf());
        updates.push(RefUpdate::set(cp_ref, None, oid.clone()));
        saved.push((name.clone(), oid));
    }
    apply_transaction(repo_dir, &updates)?;
    Ok(Checkpoint { id, refs: saved })
}

/// List available checkpoints, newest first.
pub fn list_checkpoints(repo_dir: &Path) -> Result<Vec<String>> {
    let text = git(
        repo_dir,
        &["for-each-ref", "--format=%(refname)", CHECKPOINT_PREFIX],
    )?;
    let mut ids: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix(&format!("{CHECKPOINT_PREFIX}/")))
        .filter_map(|rest| rest.split('/').next())
        .map(str::to_string)
        .collect();
    ids.sort();
    ids.dedup();
    ids.reverse();
    Ok(ids)
}

/// Restore a checkpoint by moving every recorded ref back atomically (§9.5).
pub fn restore_checkpoint(repo_dir: &Path, id: &str) -> Result<Vec<String>> {
    let prefix = format!("{CHECKPOINT_PREFIX}/{id}");
    let text = git(
        repo_dir,
        &["for-each-ref", "--format=%(refname) %(objectname)", &prefix],
    )?;
    let mut updates = Vec::new();
    let mut restored = Vec::new();
    for line in text.lines() {
        let Some((cp_ref, oid)) = line.split_once(' ') else {
            continue;
        };
        let leaf = cp_ref.strip_prefix(&format!("{prefix}/")).unwrap_or(cp_ref);
        let target = format!("refs/heads/{leaf}");
        // Force the update regardless of current value (undo is authoritative).
        updates.push(RefUpdate {
            name: GitRef::new(target.clone()),
            old: None,
            new: Some(oid.to_string()),
            no_verify: true,
        });
        restored.push(target);
    }
    if updates.is_empty() {
        return Err(GitError::Other(format!("no such checkpoint: {id}")));
    }
    apply_transaction(repo_dir, &updates)?;
    Ok(restored)
}

/// Add a detached scratch worktree (§9.3). Returns its path.
pub fn add_scratch_worktree(repo_dir: &Path, at: &str, dest: &Path) -> Result<PathBuf> {
    git(
        repo_dir,
        &[
            "worktree",
            "add",
            "--detach",
            dest.to_str()
                .ok_or_else(|| GitError::Other("non-utf8 path".into()))?,
            at,
        ],
    )?;
    Ok(dest.to_path_buf())
}

/// Remove a scratch worktree (§9.5 step 10).
pub fn remove_worktree(repo_dir: &Path, dest: &Path) -> Result<()> {
    git(
        repo_dir,
        &[
            "worktree",
            "remove",
            "--force",
            dest.to_str()
                .ok_or_else(|| GitError::Other("non-utf8 path".into()))?,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_id_has_no_colons() {
        let id = checkpoint_id_now();
        assert!(!id.contains(':'), "colons illegal in ref names: {id}");
        assert!(id.ends_with('Z'));
    }

    #[test]
    fn test_apply_transaction_deadlock_prevention() {
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let repo_dir = tmp.path();
        Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(repo_dir)
            .status()
            .unwrap();

        let mut updates = Vec::new();
        for i in 0..2000 {
            updates.push(RefUpdate {
                name: GitRef::new(format!("refs/heads/branch-{}", i)),
                old: Some("invalid-oid".to_string()),
                new: Some("another-invalid-oid".to_string()),
                no_verify: false,
            });
        }
        let result = apply_transaction(repo_dir, &updates);
        assert!(result.is_err());
    }

    #[test]
    fn test_restore_checkpoint_overwrites_existing_branch() {
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let repo_dir = tmp.path();
        Command::new("git")
            .arg("init")
            .arg("-q")
            .arg("-b")
            .arg("main")
            .arg(repo_dir)
            .status()
            .unwrap();

        // Helper to commit
        let commit = |msg: &str| {
            Command::new("git")
                .arg("-C")
                .arg(repo_dir)
                .args(["commit", "--allow-empty", "-m", msg])
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .status()
                .unwrap();
        };

        commit("initial");
        let c1_oid = git(repo_dir, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();

        // Create feat-a branch at c1
        Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .args(["checkout", "-q", "-b", "feat-a"])
            .status()
            .unwrap();

        // Write checkpoint
        let cp = write_checkpoint(repo_dir, &[GitRef::new("refs/heads/feat-a")]).unwrap();

        // Move feat-a to a new commit
        commit("new commit on feat-a");
        let c2_oid = git(repo_dir, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        assert_ne!(c1_oid, c2_oid);

        // Restore checkpoint
        let restored = restore_checkpoint(repo_dir, &cp.id).unwrap();
        assert_eq!(restored, vec!["refs/heads/feat-a"]);

        // Verify feat-a was restored to c1_oid
        let current_oid = git(repo_dir, &["rev-parse", "refs/heads/feat-a"])
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(current_oid, c1_oid);
    }
}
