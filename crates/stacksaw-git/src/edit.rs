//! Edit sessions — the flagship inbound primitive (§10.2).
//!
//! `begin` spins up a detached scratch worktree at the target commit; the
//! caller edits files with its own tools; `finish` amends the commit and
//! restacks all descendants via `--update-refs`, moving refs atomically and
//! reporting the old→new map. Real refs are never touched until `finish`, and
//! a checkpoint is written so `undo` can restore byte-identical refs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{GitError, Result};
use crate::refs::{
    self, add_scratch_worktree, apply_transaction, remove_worktree, write_checkpoint, RefUpdate,
};
use crate::repo::Repo;

/// Persisted edit-session state, stored at `.git/stacksaw/edit/<token>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditSession {
    pub token: String,
    pub commit: String,
    pub worktree: PathBuf,
    /// Branches whose history includes `commit` (they will be restacked).
    pub affected_branches: Vec<String>,
    pub created_at: String,
}

fn edit_dir(git_dir: &Path) -> PathBuf {
    git_dir.join("stacksaw").join("edit")
}

fn session_path(git_dir: &Path, token: &str) -> PathBuf {
    edit_dir(git_dir).join(format!("{token}.json"))
}

/// Result of `edit begin`.
pub struct BeginResult {
    pub session: EditSession,
    pub descendants: u32,
}

/// Start an edit session on `commit` (§10.2).
pub fn begin(repo: &Repo, commit: &str) -> Result<BeginResult> {
    let commit_oid = repo.resolve(commit)?;
    let git_dir = repo.git_dir();

    // Branches whose tip descends from (or equals) the commit are affected.
    let mut affected = Vec::new();
    let mut descendants = 0u32;
    for b in repo.local_branches()? {
        if repo.is_ancestor(commit_oid, b.tip)? {
            affected.push(b.name.clone());
            descendants += repo.commits_between(commit_oid, b.tip)?.len() as u32;
        }
    }

    let token = short_token();
    let worktree = git_dir
        .join("stacksaw")
        .join("worktrees")
        .join(format!("edit-{token}"));
    fs::create_dir_all(worktree.parent().unwrap())?;
    add_scratch_worktree(&git_dir, &commit_oid.to_string(), &worktree)?;

    let session = EditSession {
        token: token.clone(),
        commit: commit_oid.to_string(),
        worktree,
        affected_branches: affected,
        created_at: refs::checkpoint_id_now(),
    };

    fs::create_dir_all(edit_dir(&git_dir))?;
    fs::write(
        session_path(&git_dir, &token),
        serde_json::to_vec_pretty(&session).map_err(|e| GitError::Other(e.to_string()))?,
    )?;

    Ok(BeginResult {
        session,
        descendants,
    })
}

/// A finished edit: rewrites and updated refs (§10.2).
pub struct FinishResult {
    pub rewrites: Vec<(String, String)>,
    pub updated_refs: Vec<String>,
    pub checkpoint: String,
}

/// Finish an edit session: amend the commit from worktree state and restack
/// descendants atomically (§10.2).
pub fn finish(repo: &Repo, token: &str, message: Option<&str>) -> Result<FinishResult> {
    let git_dir = repo.git_dir();
    let session = load_session(&git_dir, token)?;
    let old_oid = session.commit.clone();

    // Checkpoint every affected branch before touching anything.
    let checkpoint = write_checkpoint(&git_dir, &qualify(&session.affected_branches))?;

    // Stage and amend inside the scratch worktree (detached HEAD at old commit).
    refs::git(&session.worktree, &["add", "-A"])?;
    let mut amend_args = vec!["commit", "--amend", "--no-edit", "--allow-empty"];
    if let Some(msg) = message {
        amend_args = vec!["commit", "--amend", "--allow-empty", "-m", msg];
    }
    refs::git(&session.worktree, &amend_args)?;
    let new_oid = refs::git(&session.worktree, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();

    // Replay descendants onto the amended commit for each affected branch,
    // moving intermediate refs with --update-refs where supported.
    let use_update_refs = refs::supports_update_refs(&git_dir)?;
    let mut updated_refs = Vec::new();
    let mut rewrites = vec![(old_oid.clone(), new_oid.clone())];

    // Snapshot the pre-rebase tips so we can report the rewrite map.
    for branch in &session.affected_branches {
        let full = format!("refs/heads/{branch}");
        let before = refs::git(&git_dir, &["rev-parse", &full])?
            .trim()
            .to_string();

        if before == old_oid {
            // The branch pointed exactly at the edited commit: just move it.
            apply_transaction(
                &git_dir,
                &[RefUpdate::set(
                    full.clone(),
                    Some(before.clone()),
                    new_oid.clone(),
                )],
            )?;
        } else {
            // Replay (old..branch] onto the amended commit.
            let mut args = vec![
                "rebase".to_string(),
                "--onto".to_string(),
                new_oid.clone(),
                old_oid.clone(),
                branch.clone(),
            ];
            if use_update_refs {
                args.push("--update-refs".to_string());
            }
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            refs::git(&git_dir, &arg_refs)?;
        }

        let after = refs::git(&git_dir, &["rev-parse", &full])?
            .trim()
            .to_string();
        if after != before {
            updated_refs.push(full);
            rewrites.push((before, after));
        }
    }
    updated_refs.sort();
    updated_refs.dedup();

    // Clean up the worktree and session file.
    let _ = remove_worktree(&git_dir, &session.worktree);
    let _ = fs::remove_file(session_path(&git_dir, token));

    Ok(FinishResult {
        rewrites,
        updated_refs,
        checkpoint: checkpoint.id,
    })
}

/// Abort an edit session: remove the worktree, never touching real refs.
pub fn abort(repo: &Repo, token: &str) -> Result<()> {
    let git_dir = repo.git_dir();
    let session = load_session(&git_dir, token)?;
    let _ = remove_worktree(&git_dir, &session.worktree);
    let _ = fs::remove_file(session_path(&git_dir, token));
    Ok(())
}

fn load_session(git_dir: &Path, token: &str) -> Result<EditSession> {
    let bytes = fs::read(session_path(git_dir, token))
        .map_err(|_| GitError::Other(format!("no such edit session: {token}")))?;
    serde_json::from_slice(&bytes).map_err(|e| GitError::Other(e.to_string()))
}

fn qualify(branches: &[String]) -> Vec<String> {
    branches.iter().map(|b| format!("refs/heads/{b}")).collect()
}

fn short_token() -> String {
    // A short random hex token; uses the process- and time-seeded default.
    let id = blake3::hash(
        format!(
            "{}-{}",
            process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )
        .as_bytes(),
    );
    id.to_hex()[..8].to_string()
}
