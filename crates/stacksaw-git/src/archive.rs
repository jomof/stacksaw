//! Archive whole stacks using git-staircase core archival.

use std::path::PathBuf;

use crate::error::{GitError, Result};
use crate::model::ModelOptions;
use crate::refs::{self, RefUpdate};
use crate::repo::Repo;
use crate::reshape::Undo;
use stacksaw_ssp::git_ref::GitRef;

/// Prefix under which archived branch tips are parked.
pub const ARCHIVE_PREFIX: &str = "refs/staircase-archive";

/// Archive the given local branches using git-staircase.
pub fn archive(repo: &Repo, opts: &ModelOptions, branches: &[String]) -> Result<Option<Undo>> {
    let dir = repo_dir(repo);
    let git_repo = git_staircase::GitRepo::new(dir.clone());

    if branches.is_empty() {
        return Ok(None);
    }

    // Resolve real heads and their OIDs before archiving
    let mut heads: Vec<(String, String)> = Vec::new();
    for name in branches {
        let full = if name.starts_with("refs/heads/") {
            name.clone()
        } else {
            format!("refs/heads/{name}")
        };
        let short = full.strip_prefix("refs/heads/").unwrap_or(&full).to_string();
        if let Ok(oid) = refs::git(&dir, &["rev-parse", "--verify", "--quiet", &full]) {
            let oid = oid.trim().to_string();
            if !oid.is_empty() {
                heads.push((short, oid));
            }
        }
    }
    if heads.is_empty() {
        return Ok(None);
    }

    // Record HEAD state
    let head_branch = repo.head_branch()?;
    let is_head_archived = head_branch
        .as_ref()
        .map(|h| heads.iter().any(|(b, _)| b == h))
        .unwrap_or(false);

    if is_head_archived && is_dirty(&dir)? {
        return Err(GitError::Other(
            "commit or stash changes before archiving the checked-out stack".into(),
        ));
    }

    let archive_opts = git_staircase::core::ArchiveOptions {
        reason: Some("Archived from Stacksaw".to_string()),
        dry_run: false,
        snapshot_drafts: false,
        detach_dirty_worktrees: true,
        leave_worktrees: false,
    };

    let mut archived_any = false;

    let onto = opts
        .default_upstream
        .as_deref()
        .map(|u| u.strip_prefix("refs/heads/").unwrap_or(u));

    let selector = branches
        .iter()
        .rev()
        .find_map(|b| {
            let short = b.strip_prefix("refs/heads/").unwrap_or(b);
            git_staircase::core::resolve_staircase(&git_repo, short, onto)
                .ok()
                .flatten()
                .or_else(|| {
                    git_staircase::core::resolve_staircase(&git_repo, short, None)
                        .ok()
                        .flatten()
                })
        })
        .or_else(|| {
            let step_names: Vec<String> = heads.iter().map(|(b, _)| b.clone()).collect();
            git_staircase::core::resolution::resolve_explicit_staircase(&git_repo, &step_names, onto)
                .or_else(|_| {
                    git_staircase::core::resolution::resolve_explicit_staircase(
                        &git_repo, &step_names, None,
                    )
                })
                .ok()
                .map(|staircase| git_staircase::core::ResolvedSelector {
                    staircase,
                    step_index: None,
                })
        })
        .or_else(|| {
            let oid = heads.last()?.1.clone();
            let target = heads.first()?.0.clone();
            let name = heads.last()?.0.clone();
            let steps = heads
                .iter()
                .map(|(b, o)| git_staircase::Step {
                    id: String::new(),
                    name: b.clone(),
                    cut: o.clone(),
                    branch: Some(b.clone()),
                })
                .collect();
            let meta = git_staircase::StaircaseMetadata {
                landing_policy: None,
                id: format!("implicit@{}", &oid[..16.min(oid.len())]),
                name,
                target,
                steps,
                verification_policy: None,
                primary_branch_layout: None,
                branch_layout_base: None,
                user_metadata: None,
                lifecycle: None,
            };
            Some(git_staircase::core::ResolvedSelector {
                staircase: git_staircase::core::ResolvedStaircase::Implicit(meta),
                step_index: None,
            })
        });

    let mut archived_id = None;

    if let Some(sel) = selector {
        let _ = git_staircase::core::persistence::write_metadata(&git_repo, sel.staircase.metadata());
        if let Ok(res) = git_staircase::core::archive_staircase(&git_repo, &sel, &archive_opts) {
            archived_any = true;
            archived_id = Some(res.archived_staircase_id);
        }
    }

    if archived_any {
        if is_head_archived {
            let land_target = opts
                .default_upstream
                .as_deref()
                .map(|u| u.strip_prefix("refs/heads/").unwrap_or(u))
                .unwrap_or("main");
            if refs::git(&dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{land_target}")]).is_ok() {
                let _ = refs::git(&dir, &["checkout", "-q", land_target]);
            }
        }

        let mut inv = Vec::new();
        for (name, oid) in &heads {
            inv.push(RefUpdate {
                no_verify: false,
                name: GitRef::new(format!("refs/heads/{name}")),
                old: None,
                new: Some(oid.clone()),
            });
        }

        if let Some(id) = archived_id {
            let prefix = format!("refs/staircase-archive/{id}/");
            if let Ok(text) = refs::git(&dir, &["for-each-ref", "--format=%(refname) %(objectname)", &prefix]) {
                for line in text.lines() {
                    if let Some((refname, oid)) = line.split_once(' ') {
                        inv.push(RefUpdate {
                            no_verify: false,
                            name: GitRef::new(refname.to_string()),
                            old: Some(oid.trim().to_string()),
                            new: None,
                        });
                    }
                }
            }
        }

        Ok(Some(Undo {
            refs: inv,
            checkout_head: is_head_archived,
            head: head_branch,
        }))
    } else {
        Ok(None)
    }
}

fn repo_dir(repo: &Repo) -> PathBuf {
    repo.workdir().unwrap_or_else(|| repo.git_dir())
}

fn is_dirty(dir: &std::path::Path) -> Result<bool> {
    Ok(!refs::git(dir, &["status", "--porcelain"])?
        .trim()
        .is_empty())
}
