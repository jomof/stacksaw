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



/// Options controlling staircase construction.
#[derive(Debug, Clone, Default)]
pub struct ModelOptions {
    /// Fallback upstream when a branch has no tracking configuration.
    pub default_upstream: Option<String>,
}

/// Build all staircases in the repository using git-staircase discovery.
pub fn build_staircases(repo: &Repo, opts: &ModelOptions) -> Result<Vec<Staircase>> {
    let t_start = std::time::Instant::now();
    let branches = repo.local_branches()?;
    let head_oid = repo.head_oid()?;
    let t_branches = t_start.elapsed();
    if branches.is_empty() && head_oid.is_none() {
        return Ok(Vec::new());
    }

    let git_repo = git_staircase::GitRepo::new(repo.workdir().unwrap_or_else(|| repo.git_dir()).to_path_buf());
    let mut onto_candidates = if let Some(ref default) = opts.default_upstream {
        let mut c = vec![default.clone()];
        if let Some(local_name) = GitRef::new(default).tracking_local_name() {
            c.push(format!("refs/heads/{local_name}"));
        }
        c
    } else {
        Vec::new()
    };
    onto_candidates.extend(vec![
        "refs/heads/main".to_string(),
        "refs/heads/master".to_string(),
    ]);
    if let Ok(remotes) = repo.remote_target_candidates() {
        onto_candidates.extend(remotes);
    }

    let onto_resolved = onto_candidates.into_iter().find(|c| {
        git_repo.resolve_commit_opt(c).unwrap_or(None).is_some()
    });

    let mut staircases = Vec::new();

    // 1. Run discovery once and fetch worktree draft once
    let t_disc_start = std::time::Instant::now();
    let discoveries = git_staircase::core::discover(&git_repo, onto_resolved.as_deref(), None, false)
        .map_err(|e| crate::error::GitError::Other(e.to_string()))?;
    let t_discoveries = t_disc_start.elapsed();
    let cached_draft = git_staircase::core::draft::get_worktree_draft(&git_repo).ok();

    // 2. Process managed staircases first
    let managed = git_staircase::core::persistence::list_staircases(&git_repo)
        .map_err(|e| crate::error::GitError::Other(e.to_string()))?;
    for m in managed {
        let resolved = git_staircase::ResolvedStaircase::Managed(m);
        if let Some(s) = map_staircase(repo, &git_repo, &resolved, Some(&discoveries), Some(cached_draft.clone()))? {
            staircases.push(s);
        }
    }

    // 3. Process discovered staircases next in parallel
    use rayon::prelude::*;
    let t_map_start = std::time::Instant::now();
    let unmanaged_candidates: Vec<_> = discoveries
        .iter()
        .filter_map(|d| match d {
            Discovery::Linear(metadata) => {
                let already_shown = metadata.steps.iter().any(|step| {
                    let branch_name = step.branch.as_deref().unwrap_or(&step.name);
                    branch_is_shown(&staircases, branch_name)
                });
                if !already_shown {
                    Some(git_staircase::ResolvedStaircase::Implicit(metadata.clone()))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();

    let workdir = repo.workdir().unwrap_or_else(|| repo.git_dir());
    let mapped_results: Vec<Option<Staircase>> = unmanaged_candidates
        .par_iter()
        .map(|resolved| {
            let thread_repo = Repo::open(&workdir)?;
            map_staircase(
                &thread_repo,
                &git_repo,
                resolved,
                Some(&discoveries),
                Some(cached_draft.clone()),
            )
        })
        .collect::<Result<Vec<_>>>()?;

    for s in mapped_results.into_iter().flatten() {
        staircases.push(s);
    }
    let t_map = t_map_start.elapsed();

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
    let t_twins_start = std::time::Instant::now();
    annotate_twins(repo, &mut staircases)?;
    let t_twins = t_twins_start.elapsed();

    // Open on the checked-out state: move the staircase representing HEAD to front
    if let Some(head) = &head_ref {
        if let Some(pos) = staircases
            .iter()
            .position(|s| s.segments.iter().any(|seg| seg.branch.short() == head))
        {
            staircases.swap(0, pos);
        }
    }

    tracing::debug!(
        "build_staircases total={:?}, local_branches={:?}, discover={:?}, map_staircases={:?}, annotate_twins={:?}",
        t_start.elapsed(),
        t_branches,
        t_discoveries,
        t_map,
        t_twins
    );

    Ok(staircases)
}

fn map_staircase(
    repo: &Repo,
    git_repo: &git_staircase::GitRepo,
    resolved: &git_staircase::ResolvedStaircase,
    known_discoveries: Option<&[git_staircase::model::Discovery]>,
    cached_draft: Option<Option<git_staircase::model::WorktreeDraft>>,
) -> Result<Option<Staircase>> {
    let t_start = std::time::Instant::now();
    let metadata = resolved.metadata();
    let t_status_start = std::time::Instant::now();
    let status = git_staircase::core::status::get_status_metadata_ext(
        git_repo,
        metadata.clone(),
        !resolved.is_managed(),
        known_discoveries,
        cached_draft,
    )
    .map_err(|e| crate::error::GitError::Other(e.to_string()))?;
    let t_status = t_status_start.elapsed();

    let mut segments = Vec::new();
    let mut total_ahead = 0u32;

    let target_oid_str = git_repo.resolve_commit(&metadata.target)
        .map_err(|e| crate::error::GitError::Other(e.to_string()))?;
    let target_oid = repo.resolve(&target_oid_str)?;

    let mut current_base = target_oid;

    let t_loop_start = std::time::Instant::now();
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
    let t_loop = t_loop_start.elapsed();

    if status.steps.is_empty() {
        return Ok(None);
    }

    // Calculate behind
    let t_behind_start = std::time::Instant::now();
    let root_tip_str = status.steps.first().and_then(|s| s.actual_oid.as_ref()).unwrap_or(&metadata.steps[0].cut);
    let root_tip = repo.resolve(root_tip_str)?;
    let root_base = repo.merge_base(root_tip, target_oid)?;
    let behind = repo.commits_between(root_base, target_oid)?.len() as u32;
    let t_behind = t_behind_start.elapsed();

    tracing::debug!(
        "map_staircase({}) total={:?}, get_status={:?}, step_loop={:?}, calc_behind={:?}",
        metadata.name,
        t_start.elapsed(),
        t_status,
        t_loop,
        t_behind
    );

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

    let t_patch_start = std::time::Instant::now();
    let patch_ids = repo.patch_ids(&needs_patch_id)?;
    tracing::debug!("annotate_twins patch_ids for {} commits took {:?}", needs_patch_id.len(), t_patch_start.elapsed());

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

