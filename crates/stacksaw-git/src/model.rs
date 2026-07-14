//! Lossless UI projection of canonical `git-staircase` list/show/status data.
//!
//! Discovery, identity, lifecycle, step boundaries, and status all come from
//! `git-staircase`. This module only enriches canonical steps with commit
//! summaries and twin links needed by Stacksaw's review UI.

use std::collections::{BTreeMap, HashMap, HashSet};

use git_staircase::core::refs::StaircaseRefs;
use git_staircase::model::{Discovery, LifecycleState, StaircaseState, StaircaseStatus};
use git_staircase::{ResolvedSelector, ResolvedStaircase};
use rayon::prelude::*;
use stacksaw_ssp::git_ref::GitRef;
use stacksaw_ssp::types::{
    CanonicalSelector, CommitSummary, FindingCounts, IntegrationContext, LayoutState, Lifecycle,
    RebaseStatus, RepresentationKind, ReviewState, Segment, Staircase, StructuralState,
    VerificationState, WORKTREE_OID,
};

use crate::error::{GitError, Result};
use crate::repo::Repo;

/// Options passed to canonical Staircase discovery.
#[derive(Debug, Clone, Default)]
pub struct ModelOptions {
    pub default_upstream: Option<String>,
}

fn staircase_repo(repo: &Repo) -> git_staircase::GitRepo {
    git_staircase::GitRepo::new(
        repo.workdir()
            .unwrap_or_else(|| repo.git_dir())
            .to_path_buf(),
    )
}

fn staircase_error(error: impl std::fmt::Display) -> GitError {
    GitError::Other(error.to_string())
}

/// Build the canonical active Staircase list. Managed records shadow an
/// implicit discovery with the same structural identity, exactly as canonical
/// `git staircase list` does.
pub fn build_staircases(repo: &Repo, opts: &ModelOptions) -> Result<Vec<Staircase>> {
    let git_repo = staircase_repo(repo);
    let onto = opts.default_upstream.as_deref();
    let discoveries =
        git_staircase::core::discover(&git_repo, onto, None, false).map_err(staircase_error)?;
    let family_paths = canonical_family_path_ids(&git_repo, onto);
    let draft = git_staircase::core::get_worktree_draft(&git_repo).ok();

    let mut canonical = BTreeMap::<String, (ResolvedStaircase, RepresentationKind)>::new();
    for metadata in
        git_staircase::core::list_staircases(&git_repo).map_err(staircase_error)?
    {
        let integration = git_repo
            .resolve_commit(&metadata.target)
            .map_err(staircase_error)?;
        let structural =
            git_staircase::core::discovery::compute_implicit_id(
                &git_repo,
                &integration,
                &metadata.steps,
            )
            .map_err(staircase_error)?;
        canonical.insert(
            structural,
            (
                ResolvedStaircase::Managed(metadata),
                RepresentationKind::Managed,
            ),
        );
    }

    for discovery in &discoveries {
        let Discovery::Linear(metadata) = discovery else {
            continue;
        };
        canonical.entry(metadata.id.clone()).or_insert_with(|| {
            let representation = if family_paths.contains(&metadata.id) {
                RepresentationKind::FamilyPath
            } else {
                RepresentationKind::Implicit
            };
            (
                ResolvedStaircase::Implicit(metadata.clone()),
                representation,
            )
        });
    }

    let workdir = repo.workdir().unwrap_or_else(|| repo.git_dir()).to_path_buf();
    let projected = canonical
        .into_values()
        .collect::<Vec<_>>()
        .par_iter()
        .map(|(resolved, representation)| {
            let thread_repo = Repo::open(&workdir)?;
            project_staircase(
                &thread_repo,
                &git_repo,
                resolved,
                *representation,
                Some(&discoveries),
                Some(draft.clone()),
            )
        })
        .collect::<Result<Vec<_>>>()?;

    let mut staircases = projected.into_iter().flatten().collect::<Vec<_>>();
    annotate_twins(repo, &mut staircases)?;
    Ok(staircases)
}

fn canonical_family_path_ids(
    repo: &git_staircase::GitRepo,
    onto: Option<&str>,
) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Ok(families) = git_staircase::core::discover(repo, onto, None, true) else {
        return ids;
    };
    for discovery in families {
        let Discovery::Ambiguous(family) = discovery else {
            continue;
        };
        if !family.steps.values().any(|step| step.children.len() > 1) {
            continue;
        }
        let Ok(integration) = repo.resolve_commit(&family.target) else {
            continue;
        };
        for leaf in family
            .steps
            .values()
            .filter(|step| step.children.is_empty())
        {
            let Some(path) = git_staircase::core::inference::extract_path_to(&family, &leaf.name)
            else {
                continue;
            };
            if let Ok(id) = git_staircase::core::discovery::compute_implicit_id(
                repo,
                &integration,
                &path.steps,
            ) {
                ids.insert(id);
            }
        }
    }
    ids
}

fn project_staircase(
    repo: &Repo,
    git_repo: &git_staircase::GitRepo,
    resolved: &ResolvedStaircase,
    representation: RepresentationKind,
    known_discoveries: Option<&[Discovery]>,
    cached_draft: Option<Option<git_staircase::WorktreeDraft>>,
) -> Result<Option<Staircase>> {
    let metadata = resolved.metadata();
    if metadata.steps.is_empty() {
        return Ok(None);
    }
    let status = git_staircase::core::status::get_status_metadata_ext(
        git_repo,
        metadata.clone(),
        !resolved.is_managed(),
        known_discoveries,
        cached_draft,
    )
    .map_err(staircase_error)?;

    let integration_oid = git_repo
        .resolve_commit(&metadata.target)
        .map_err(staircase_error)?;
    let mut current_base = repo.resolve(&integration_oid)?;
    let mut segments = Vec::with_capacity(metadata.steps.len());
    let mut ahead = 0u32;

    // Decomposition follows the exact canonical cuts from `show`; live branch
    // differences are carried separately from canonical `status`.
    for (index, (step, step_status)) in metadata.steps.iter().zip(&status.steps).enumerate() {
        let cut = repo.resolve(&step.cut)?;
        let commits = repo
            .commits_between(current_base, cut)?
            .into_iter()
            .map(|oid| commit_summary(repo, oid))
            .collect::<Result<Vec<_>>>()?;
        ahead += commits.len() as u32;
        let canonical_branch = step
            .branch
            .as_ref()
            .map(|branch| GitRef::new(format!("refs/heads/{branch}")));
        let render_branch = canonical_branch
            .clone()
            .unwrap_or_else(|| GitRef::new(step.name.clone()));
        segments.push(Segment {
            step_id: (!step.id.is_empty()).then(|| step.id.clone()),
            ordinal: (index + 1) as u32,
            cut: step.cut.clone(),
            branch: render_branch,
            canonical_branch,
            parent: (index > 0).then(|| index - 1),
            stale: step_status.is_stale,
            actual_oid: step_status.actual_oid.clone(),
            modified: step_status.is_modified,
            incomplete: step_status.is_incomplete,
            commits,
        });
        current_base = cut;
    }

    let target = repo.resolve(&integration_oid)?;
    let root = repo.resolve(&metadata.steps[0].cut)?;
    let merge_base = repo.merge_base(root, target)?;
    let behind = repo.commits_between(merge_base, target)?.len() as u32;
    let dirty = status.worktree_draft.as_ref().is_some_and(|draft| {
        draft.classification != git_staircase::model::DraftClassification::Clean
    });

    let (record_revision, structure_revision, lifecycle) = if resolved.is_managed() {
        let record = git_staircase::core::read_record(
            git_repo,
            &StaircaseRefs::state_record(&metadata.id),
        )
        .map_err(staircase_error)?;
        (
            Some(record.record_oid),
            Some(record.structure_oid),
            map_lifecycle(record.lifecycle.state),
        )
    } else {
        (None, Some(metadata.id.clone()), Lifecycle::Active)
    };

    let selector = match representation {
        RepresentationKind::Managed => CanonicalSelector {
            lineage_id: Some(metadata.id.clone()),
            structural_key: None,
            path_id: None,
        },
        RepresentationKind::Implicit => CanonicalSelector {
            lineage_id: None,
            structural_key: Some(metadata.id.clone()),
            path_id: None,
        },
        RepresentationKind::FamilyPath => CanonicalSelector {
            lineage_id: None,
            structural_key: Some(metadata.id.clone()),
            path_id: Some(metadata.id.clone()),
        },
    };
    let selector_for_layout = ResolvedSelector {
        staircase: resolved.clone(),
        step_index: None,
    };
    let layout = git_staircase::core::layout_state(git_repo, &selector_for_layout)
        .map(|layout| LayoutState {
            profile: layout.profile,
            base: layout.base,
            state: layout.state,
        })
        .unwrap_or_else(|_| LayoutState {
            profile: metadata.primary_branch_layout.clone(),
            base: metadata.branch_layout_base.clone(),
            state: "unknown".into(),
        });

    Ok(Some(Staircase {
        id: selector.stable_id().map(str::to_string),
        representation,
        lifecycle,
        selector,
        record_revision,
        structure_revision,
        integration: IntegrationContext {
            target: metadata.target.clone(),
            oid: integration_oid,
        },
        structural_state: map_structural_state(status.state()),
        layout_state: layout,
        review_state: review_state(&status),
        verification_state: verification_state(&status),
        canonical_show: serde_json::to_value(metadata).unwrap_or(serde_json::Value::Null),
        canonical_status: serde_json::to_value(&status).unwrap_or(serde_json::Value::Null),
        name: metadata.name.clone(),
        upstream: GitRef::new(metadata.target.clone()),
        ahead,
        behind,
        dirty,
        rebase: RebaseStatus::Unknown,
        conflict: None,
        segments,
    }))
}

fn map_lifecycle(state: LifecycleState) -> Lifecycle {
    match state {
        LifecycleState::Active => Lifecycle::Active,
        LifecycleState::Archived => Lifecycle::Archived,
    }
}

fn map_structural_state(state: StaircaseState) -> StructuralState {
    match state {
        StaircaseState::Clean => StructuralState::Clean,
        StaircaseState::Incomplete => StructuralState::Incomplete,
        StaircaseState::Diverged => StructuralState::Diverged,
        StaircaseState::Ambiguous => StructuralState::Ambiguous,
        StaircaseState::Stale => StructuralState::Stale,
    }
}

fn review_state(status: &StaircaseStatus) -> ReviewState {
    let configured = status
        .metadata
        .user_metadata
        .as_ref()
        .is_some_and(|metadata| {
            metadata.extensions.keys().any(|key| {
                key.contains("review") || key.contains("github") || key.contains("gerrit")
            })
        });
    if configured {
        ReviewState::Configured
    } else {
        ReviewState::Unconfigured
    }
}

fn verification_state(status: &StaircaseStatus) -> VerificationState {
    if status.metadata.verification_policy.is_none() {
        return VerificationState::Unconfigured;
    }
    match status.verification_results.as_deref() {
        None | Some([]) => VerificationState::Pending,
        Some(results) if results.iter().all(|result| result.success) => VerificationState::Passed,
        Some(_) => VerificationState::Failed,
    }
}

/// Resolve any canonical selector carried by SSP.
pub fn resolve_canonical_selector(
    repo: &Repo,
    selector: &CanonicalSelector,
    opts: &ModelOptions,
) -> Result<ResolvedSelector> {
    let git_repo = staircase_repo(repo);
    let staircase = if let Some(id) = &selector.lineage_id {
        git_staircase::core::resolve_by_id(&git_repo, id)
    } else if let Some(key) = selector
        .path_id
        .as_ref()
        .or(selector.structural_key.as_ref())
    {
        git_staircase::core::resolve_by_structural_key(
            &git_repo,
            key,
            opts.default_upstream.as_deref(),
        )
    } else {
        return Err(GitError::Other("empty canonical staircase selector".into()));
    }
    .map_err(staircase_error)?;
    Ok(ResolvedSelector {
        staircase,
        step_index: None,
    })
}

/// Resolve and project a selected canonical structure.
pub fn resolve_staircase(
    repo: &Repo,
    selector: &CanonicalSelector,
    opts: &ModelOptions,
) -> Result<Option<Staircase>> {
    let git_repo = staircase_repo(repo);
    let resolved = resolve_canonical_selector(repo, selector, opts)?;
    let representation = if resolved.is_managed() {
        RepresentationKind::Managed
    } else if selector.path_id.is_some() {
        RepresentationKind::FamilyPath
    } else {
        RepresentationKind::Implicit
    };
    let discoveries =
        git_staircase::core::discover(&git_repo, opts.default_upstream.as_deref(), None, false)
            .ok();
    let draft = git_staircase::core::get_worktree_draft(&git_repo).ok();
    project_staircase(
        repo,
        &git_repo,
        &resolved.staircase,
        representation,
        discoveries.as_deref(),
        Some(draft),
    )
}

/// Compatibility helper for callers that already hold an implicit structural
/// key. Managed lineage IDs are also accepted.
pub fn resolve_staircase_by_structural_key(
    repo: &Repo,
    key: &str,
    opts: &ModelOptions,
) -> Result<Option<Staircase>> {
    let selector = if key.starts_with("implicit@") {
        CanonicalSelector {
            lineage_id: None,
            structural_key: Some(key.to_string()),
            path_id: None,
        }
    } else {
        CanonicalSelector {
            lineage_id: Some(key.to_string()),
            structural_key: None,
            path_id: None,
        }
    };
    resolve_staircase(repo, &selector, opts)
}

fn commit_summary(repo: &Repo, oid: gix::ObjectId) -> Result<CommitSummary> {
    let meta = repo.commit_meta(oid)?;
    Ok(CommitSummary {
        oid: meta.oid.to_string(),
        short: meta.short(),
        subject: meta.subject,
        author: meta.author_name,
        author_time: meta.author_time,
        parents: meta.parents.iter().map(ToString::to_string).collect(),
        change_id: meta.change_id,
        patch_id: meta.patch_id,
        finding_counts: FindingCounts::default(),
        twins: Vec::new(),
        added: 0,
        deleted: 0,
    })
}

fn annotate_twins(repo: &Repo, staircases: &mut [Staircase]) -> Result<()> {
    let needs_patch_id = staircases
        .iter()
        .flat_map(|staircase| &staircase.segments)
        .flat_map(|segment| &segment.commits)
        .filter(|commit| commit.change_id.is_none() && commit.oid != WORKTREE_OID)
        .map(|commit| commit.oid.clone())
        .collect::<Vec<_>>();
    let patch_ids = repo.patch_ids(&needs_patch_id)?;
    let mut by_change = HashMap::<String, Vec<String>>::new();
    let mut by_patch = HashMap::<String, Vec<String>>::new();

    for commit in staircases
        .iter_mut()
        .flat_map(|staircase| &mut staircase.segments)
        .flat_map(|segment| &mut segment.commits)
    {
        if let Some(change_id) = &commit.change_id {
            by_change
                .entry(change_id.clone())
                .or_default()
                .push(commit.oid.clone());
        } else if let Some(patch_id) = patch_ids.get(&commit.oid) {
            commit.patch_id = Some(patch_id.clone());
            by_patch
                .entry(patch_id.clone())
                .or_default()
                .push(commit.oid.clone());
        }
    }

    for commit in staircases
        .iter_mut()
        .flat_map(|staircase| &mut staircase.segments)
        .flat_map(|segment| &mut segment.commits)
    {
        let matches = commit
            .change_id
            .as_ref()
            .and_then(|id| by_change.get(id))
            .or_else(|| commit.patch_id.as_ref().and_then(|id| by_patch.get(id)));
        if let Some(matches) = matches {
            commit.twins = matches
                .iter()
                .filter(|oid| *oid != &commit.oid)
                .cloned()
                .collect();
        }
    }
    Ok(())
}
