//! The staircase / segment-tree model (§2). Builds [`Staircase`] DTOs from the
//! repository's local branches and their upstreams.

use std::collections::HashMap;

use stacksaw_ssp::types::{
    CommitSummary, FindingCounts, RebaseStatus, Segment, Staircase, WORKTREE_OID,
};

use crate::error::Result;
use crate::repo::Repo;
use stacksaw_ssp::git_ref::GitRef;

use git_staircase::model::Discovery;
use git_staircase::core::get_status_metadata;



/// Options controlling staircase construction.
#[derive(Debug, Clone, Default)]
pub struct ModelOptions {
    /// Fallback upstream when a branch has no tracking configuration.
    pub default_upstream: Option<String>,
}

/// Build all staircases in the repository using git-staircase discovery.
pub fn build_staircases(repo: &Repo, opts: &ModelOptions) -> Result<Vec<Staircase>> {
    let branches = repo.local_branches()?;
    let head_oid = repo.head_oid()?;
    if branches.is_empty() && head_oid.is_none() {
        return Ok(Vec::new());
    }

    let git_repo = git_staircase::GitRepo::new(repo.workdir().unwrap_or_else(|| repo.git_dir()).to_path_buf());
    let onto_candidates = if let Some(ref default) = opts.default_upstream {
        let mut c = vec![default.clone()];
        if let Some(local_name) = GitRef::new(default).tracking_local_name() {
            c.push(format!("refs/heads/{local_name}"));
        }
        c.push("refs/heads/main".to_string());
        c.push("refs/heads/master".to_string());
        c
    } else {
        vec![
            "refs/heads/main".to_string(),
            "refs/heads/master".to_string(),
        ]
    };

    let onto_resolved = onto_candidates.into_iter().find(|c| {
        git_repo.resolve_commit_opt(c).unwrap_or(None).is_some()
    });

    let mut staircases = Vec::new();

    // 1. Process managed staircases first
    let managed = git_staircase::core::persistence::list_staircases(&git_repo)
        .map_err(|e| crate::error::GitError::Other(e.to_string()))?;
    for m in managed {
        let resolved = git_staircase::ResolvedStaircase::Managed(m);
        if let Some(s) = map_staircase(repo, &git_repo, &resolved)? {
            staircases.push(s);
        }
    }

    // 2. Process discovered staircases next
    let discoveries = git_staircase::core::discover(&git_repo, onto_resolved.as_deref())
        .map_err(|e| crate::error::GitError::Other(e.to_string()))?;

    for d in discoveries {
        match d {
            Discovery::Linear(metadata) => {
                let already_shown = metadata.steps.iter().any(|step| {
                    let branch_name = step.branch.as_deref().unwrap_or(&step.name);
                    branch_is_shown(&staircases, branch_name)
                });

                if !already_shown {
                    let resolved = git_staircase::ResolvedStaircase::Implicit(metadata);
                    if let Some(s) = map_staircase(repo, &git_repo, &resolved)? {
                        staircases.push(s);
                    }
                }
            }
            Discovery::Ambiguous(_family) => {
                // Ignore families/forks for now, aligning with linear-only pure behavior
            }
        }
    }

    // Always surface the checked-out state as a staircase (detached HEAD handling)
    let head_ref = repo.head_ref_label().ok().flatten();

    if let Some(head) = &head_ref {
        if !branch_is_shown(&staircases, head) {
            let synthetic = match branches.iter().find(|b| &b.name == head) {
                Some(b) => {
                    let label = b
                        .upstream
                        .clone()
                        .or_else(|| {
                            opts.default_upstream
                                .as_ref()
                                .map(|s| GitRef::new(s.clone()))
                        })
                        .map(|u| short_upstream(&u))
                        .unwrap_or_else(|| "(root)".to_string());
                    Some(build_rootless_staircase(repo, b.tip, head, &label)?)
                }
                None => Some(detached_staircase(head)),
            };
            if let Some(s) = synthetic {
                staircases.push(s);
            }
        }
    }

    // Detect twins across all staircases by Change-Id trailer (§2).
    annotate_twins(repo, &mut staircases)?;

    // Open on the checked-out state: move the staircase representing HEAD to front
    if let Some(head) = &head_ref {
        if let Some(pos) = staircases
            .iter()
            .position(|s| s.segments.iter().any(|seg| seg.branch.short() == head))
        {
            staircases.swap(0, pos);
        }
    }

    Ok(staircases)
}

fn map_staircase(
    repo: &Repo,
    git_repo: &git_staircase::GitRepo,
    resolved: &git_staircase::ResolvedStaircase,
) -> Result<Option<Staircase>> {
    let metadata = resolved.metadata();
    let status = get_status_metadata(git_repo, metadata.clone(), !resolved.is_managed())
        .map_err(|e| crate::error::GitError::Other(e.to_string()))?;

    let mut segments = Vec::new();
    let mut total_ahead = 0u32;

    let target_oid_str = git_repo.resolve_commit(&metadata.target)
        .map_err(|e| crate::error::GitError::Other(e.to_string()))?;
    let target_oid = repo.resolve(&target_oid_str)?;

    let mut current_base = target_oid;

    for (i, step_status) in status.steps.iter().enumerate() {
        let step = &metadata.steps[i];
        let branch_name = step.branch.as_deref().unwrap_or(&step.name);
        let full_branch_ref = format!("refs/heads/{}", branch_name);

        let tip_oid_str = step_status.actual_oid.as_ref().unwrap_or(&step.cut);
        let tip_oid = repo.resolve(tip_oid_str)?;

        let base_oid = if step_status.is_stale {
            let expected_parent_cut = if i == 0 {
                target_oid_str.clone()
            } else {
                metadata.steps[i - 1].cut.clone()
            };
            repo.resolve(&expected_parent_cut)?
        } else {
            current_base
        };

        let oids = repo.commits_between(base_oid, tip_oid)?;
        let mut commits = Vec::with_capacity(oids.len());
        for oid in oids {
            commits.push(commit_summary(repo, oid)?);
        }
        total_ahead += commits.len() as u32;

        segments.push(Segment {
            branch: GitRef::new(full_branch_ref),
            parent: if i == 0 { None } else { Some(i - 1) },
            stale: if i == 0 { false } else { step_status.is_stale },
            commits,
        });

        current_base = tip_oid;
    }

    if status.steps.is_empty() {
        return Ok(None);
    }

    // Calculate behind
    let root_tip_str = status.steps.first().and_then(|s| s.actual_oid.as_ref()).unwrap_or(&metadata.steps[0].cut);
    let root_tip = repo.resolve(root_tip_str)?;
    let root_base = repo.merge_base(root_tip, target_oid)?;
    let behind = repo.commits_between(root_base, target_oid)?.len() as u32;

    let rebase_status = match status.state() {
        git_staircase::model::StaircaseState::Clean => RebaseStatus::Clean,
        _ => RebaseStatus::Unknown,
    };

    Ok(Some(Staircase {
        name: metadata.name.clone(),
        upstream: GitRef::new(metadata.target.clone()),
        ahead: total_ahead,
        behind,
        dirty: false,
        rebase: rebase_status,
        conflict: None,
        segments,
    }))
}

fn branch_is_shown(staircases: &[Staircase], branch: &str) -> bool {
    staircases
        .iter()
        .any(|s| s.segments.iter().any(|seg| seg.branch.short() == branch))
}

fn short_upstream(name: &str) -> String {
    GitRef::new(name).short().to_string()
}

fn build_rootless_staircase(
    repo: &Repo,
    tip: gix::ObjectId,
    name: &str,
    upstream_label: &str,
) -> Result<Staircase> {
    let oids = repo.commits_reachable(tip, Some(100))?;
    let mut commits = Vec::with_capacity(oids.len());
    for oid in oids {
        commits.push(commit_summary(repo, oid)?);
    }
    let ahead = commits.len() as u32;
    Ok(Staircase {
        name: name.to_string(),
        upstream: GitRef::new(upstream_label),
        ahead,
        behind: 0,
        dirty: false,
        rebase: RebaseStatus::default(),
        conflict: None,
        segments: vec![Segment {
            branch: GitRef::new(name),
            parent: None,
            stale: false,
            commits,
        }],
    })
}

fn detached_staircase(name: &str) -> Staircase {
    Staircase {
        name: name.to_string(),
        upstream: GitRef::new(name),
        ahead: 0,
        behind: 0,
        dirty: false,
        rebase: RebaseStatus::default(),
        conflict: None,
        segments: vec![Segment {
            branch: GitRef::new(name),
            parent: None,
            stale: false,
            commits: Vec::new(),
        }],
    }
}

fn commit_summary(repo: &Repo, oid: gix::ObjectId) -> Result<CommitSummary> {
    let meta = repo.commit_meta(oid)?;
    Ok(CommitSummary {
        oid: meta.oid.to_string(),
        short: meta.short(),
        subject: meta.subject.clone(),
        author: meta.author_name.clone(),
        author_time: meta.author_time,
        parents: meta.parents.iter().map(|p| p.to_string()).collect(),
        change_id: meta.change_id.clone(),
        patch_id: meta.patch_id.clone(),
        finding_counts: FindingCounts::default(),
        twins: vec![],
        added: 0,
        deleted: 0,
    })
}

fn annotate_twins(repo: &Repo, staircases: &mut [Staircase]) -> Result<()> {
    let mut needs_patch_id = Vec::new();
    for s in staircases.iter() {
        for seg in &s.segments {
            for c in &seg.commits {
                if c.change_id.is_none() && c.oid != WORKTREE_OID {
                    needs_patch_id.push(c.oid.clone());
                }
            }
        }
    }

    let patch_ids = repo.patch_ids(&needs_patch_id)?;

    let mut by_change: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_patch: HashMap<String, Vec<String>> = HashMap::new();

    for s in staircases.iter_mut() {
        for seg in &mut s.segments {
            for c in &mut seg.commits {
                if let Some(cid) = &c.change_id {
                    by_change
                        .entry(cid.clone())
                        .or_default()
                        .push(c.oid.clone());
                } else if let Some(pid) = patch_ids.get(&c.oid) {
                    c.patch_id = Some(pid.clone());
                    by_patch.entry(pid.clone()).or_default().push(c.oid.clone());
                }
            }
        }
    }

    for s in staircases.iter_mut() {
        for seg in &mut s.segments {
            for c in &mut seg.commits {
                let twins = if let Some(cid) = &c.change_id {
                    by_change.get(cid)
                } else if let Some(pid) = &c.patch_id {
                    by_patch.get(pid)
                } else {
                    None
                };
                if let Some(oids) = twins {
                    c.twins = oids.iter().filter(|o| **o != c.oid).cloned().collect();
                }
            }
        }
    }
    Ok(())
}

