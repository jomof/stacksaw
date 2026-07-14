//! The per-repo core service state (§3.1, P2 one source of truth).
//!
//! The **only** crate that touches `stacksaw_git` for live repo operations.
//! UI and CLI clients talk to this through the [`crate::core::Core`] handle.



use anyhow::{anyhow, bail, Result};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use stacksaw_git::edit;
use stacksaw_git::model::{self, ModelOptions};
use stacksaw_git::refs::{self, git};
use stacksaw_git::{
    build_snapshot, changed_files, commit_message, file_content, file_diff, snapshot,
    DiffProcessor, Repo,
};
use stacksaw_ssp::types::{
    CanonicalSelector, ChangeView, CommitDetail, CommitRecord, EditBegin, EditFinish, MutatePlan,
    MutateResult, Rewrite, Snapshot, SCHEMA_VERSION, WORKTREE_OID,
};
use tokio::sync::broadcast;
use tokio::task::spawn_blocking;

use crate::config::Config;
use crate::prober::RebaseProber;

/// A broadcastable change event (§5.3 notifications).
#[derive(Debug, Clone)]
pub enum ChangeEvent {
    SnapshotAdvanced { generation: u64 },
    RefsChanged,
    WorktreeChanged,
}

/// Shared, cloneable handle to the running service.
#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}

struct Inner {
    repo_root: PathBuf,
    git_dir: PathBuf,
    config: Config,
    generation: AtomicU64,
    events: broadcast::Sender<ChangeEvent>,
    prober: OnceLock<RebaseProber>,
}

impl Service {
    pub fn new(repo_root: PathBuf, git_dir: PathBuf, config: Config) -> Self {
        let (events, _) = broadcast::channel(256);
        Service {
            inner: Arc::new(Inner {
                repo_root,
                git_dir,
                config,
                generation: AtomicU64::new(1),
                events,
                prober: OnceLock::new(),
            }),
        }
    }

    pub fn repo_root(&self) -> &Path {
        &self.inner.repo_root
    }
    pub fn git_dir(&self) -> &Path {
        &self.inner.git_dir
    }
    pub fn config(&self) -> &Config {
        &self.inner.config
    }
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        self.inner.events.subscribe()
    }

    /// Bump the generation and broadcast `snapshot/didAdvance` (§6).
    pub fn advance(&self) -> u64 {
        let g = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self
            .inner
            .events
            .send(ChangeEvent::SnapshotAdvanced { generation: g });
        g
    }

    pub fn emit(&self, event: ChangeEvent) {
        let _ = self.inner.events.send(event);
    }

    fn model_options(&self) -> ModelOptions {
        let upstream = &self.inner.config.upstream.default;
        let candidate = if upstream.is_empty() {
            None
        } else if upstream.starts_with("refs/") {
            Some(upstream.clone())
        } else {
            Some(format!("refs/remotes/{upstream}"))
        };
        let default_upstream = candidate.filter(|candidate| {
            git_staircase::GitRepo::new(self.inner.repo_root.clone())
                .resolve_commit_opt(candidate)
                .ok()
                .flatten()
                .is_some()
        });
        ModelOptions { default_upstream }
    }

    fn prober(&self) -> Option<&RebaseProber> {
        self.inner.prober.get()
    }

    fn ensure_prober(&self, repo: &Repo) -> Option<&RebaseProber> {
        let workdir = repo.workdir()?;
        let common = repo.common_dir();
        self.inner
            .prober
            .get_or_init(|| RebaseProber::new(workdir, common));
        self.inner.prober.get()
    }

    /// Drain finished background probes; bumps generation when verdicts arrive.
    pub fn drain_prober(&self) -> bool {
        let Some(prober) = self.prober() else {
            return false;
        };
        if prober.drain() {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Fast, lightweight initial snapshot for instant UI rendering before full
    /// model discovery finishes (§5.3).
    pub fn fast_snapshot(&self) -> Snapshot {
        let repo = Repo::open(&self.inner.repo_root).ok();
        let head_oid = repo
            .as_ref()
            .and_then(|repo| repo.head_oid().ok().flatten())
            .map(|oid| oid.to_string());
        let detached = repo
            .as_ref()
            .and_then(|repo| repo.is_detached().ok())
            .unwrap_or(false);
        let checkout = head_oid
            .as_ref()
            .map(|oid| stacksaw_ssp::types::CheckoutContext {
                head_oid: oid.clone(),
                branch: (!detached)
                    .then(|| {
                        repo.as_ref()
                            .and_then(|repo| repo.head_ref_label().ok().flatten())
                            .map(stacksaw_ssp::git_ref::GitRef::new)
                    })
                    .flatten(),
                detached,
            });
        let head = head_oid.map(stacksaw_ssp::git_ref::GitRef::new);
        Snapshot {
            schema_version: SCHEMA_VERSION,
            generation: self.generation(),
            head,
            detached,
            checkout,
            staircases: vec![],
        }
    }

    /// Build a fresh snapshot at the current generation, applying cached probe
    /// verdicts and enqueueing any missing probes.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        self.drain_prober();
        let repo_root = self.inner.repo_root.clone();
        let generation = self.generation();
        let opts = self.model_options();
        let mut snap = spawn_blocking(move || -> Result<Snapshot> {
            let repo = Repo::open(&repo_root)?;
            Ok(build_snapshot(&repo, generation, &opts)?)
        })
        .await??;

        let repo = Repo::open(&self.inner.repo_root)?;
        if let Some(prober) = self.ensure_prober(&repo) {
            prober.annotate(&repo, &mut snap.staircases);
        }
        // Discovery can overlap a watcher invalidation. Stamp the generation at
        // handoff so a freshly returned snapshot is not born stale.
        snap.generation = self.generation();
        Ok(snap)
    }

    /// Files changed by a commit (§8.1).
    pub async fn commit_detail(&self, oid: &str) -> Result<CommitDetail> {
        let repo_root = self.inner.repo_root.clone();
        let generation = self.generation();
        let oid = oid.to_string();
        let oid_for_detail = oid.clone();
        let files =
            spawn_blocking(move || -> Result<_> { Ok(changed_files(&repo_root, &oid)?) }).await??;
        Ok(CommitDetail {
            oid: oid_for_detail,
            generation,
            files,
        })
    }

    /// Commit metadata for porcelain `show` (§5.3 `commit/get`).
    pub async fn commit_show(&self, rev: &str) -> Result<CommitRecord> {
        let repo_root = self.inner.repo_root.clone();
        let rev = rev.to_string();
        spawn_blocking(move || -> Result<CommitRecord> {
            let repo = Repo::open(&repo_root)?;
            let oid = repo.resolve(&rev)?;
            let meta = repo.commit_meta(oid)?;
            Ok(CommitRecord {
                oid: meta.oid.to_string(),
                short: meta.short(),
                subject: meta.subject,
                body: meta.body,
                author: meta.author_name,
                author_email: meta.author_email,
                change_id: meta.change_id,
                parents: meta.parents.iter().map(|p| p.to_string()).collect(),
            })
        })
        .await?
    }

    /// The change under review for one file (or the commit message row).
    pub async fn change_view(&self, commit: &str, path: &str) -> Result<ChangeView> {
        let repo_root = self.inner.repo_root.clone();
        let commit = commit.to_string();
        let path = path.to_string();
        spawn_blocking(move || -> Result<ChangeView> {
            if path == "commit message" {
                return Ok(ChangeView::Message {
                    text: commit_message(&repo_root, &commit)?,
                });
            }
            let files = changed_files(&repo_root, &commit)?;
            let entry = files.iter().find(|f| f.path == path);
            let is_added = entry.is_some_and(|e| e.status.as_char() == 'A')
                || (commit != WORKTREE_OID && {
                    // Root commit files are all added.
                    let out = git(
                        &repo_root,
                        &["show", "--name-status", "--format=", "-M", &commit],
                    )?;
                    DiffProcessor::parse_name_status(&out)
                        .iter()
                        .any(|(p, s)| p == &path && s == &stacksaw_ssp::types::FileStatus::Added)
                });
            if is_added {
                Ok(ChangeView::AddedFile {
                    path: path.clone(),
                    content: file_content(&repo_root, &commit, &path)?,
                })
            } else {
                Ok(ChangeView::ModifiedDiff {
                    path: path.clone(),
                    diff: file_diff(&repo_root, &commit, &path)?,
                })
            }
        })
        .await?
    }

    /// Unified diff for a range (CLI `diff`).
    pub async fn diff_range(&self, args: &[&str]) -> Result<String> {
        let repo_root = self.inner.repo_root.clone();
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        spawn_blocking(move || -> Result<String> {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            Ok(git(&repo_root, &refs)?)
        })
        .await?
    }

    /// Range-diff between two refs (CLI `interdiff`).
    pub async fn diff_interdiff(&self, a: &str, b: &str) -> Result<String> {
        let repo_root = self.inner.repo_root.clone();
        let (a, b) = (a.to_string(), b.to_string());
        spawn_blocking(move || -> Result<String> { Ok(git(&repo_root, &["range-diff", &a, &b])?) })
            .await?
    }

    /// Apply a domain mutation plan (§4).
    pub async fn mutate(
        &self,
        plan: MutatePlan,
        if_generation: Option<u64>,
    ) -> Result<MutateResult> {
        if let Some(expected) = if_generation {
            if self.generation() != expected {
                bail!(
                    "stale generation: expected {expected}, have {}",
                    self.generation()
                );
            }
        }
        let repo_root = self.inner.repo_root.clone();
        let opts = self.model_options();
        let checkpoint = spawn_blocking(move || -> Result<String> {
            let repo = Repo::open(&repo_root)?;
            let git_repo = git_staircase::GitRepo::new(repo_root.clone());
            let checkpoint = refs::write_repository_checkpoint(&repo_root)?.id;
            match plan {
                MutatePlan::Reshape { target_oid, op } => {
                    apply_reshape_adapter(&repo, &git_repo, &opts, &target_oid, &op)?;
                }
                MutatePlan::Archive { branches } => {
                    let selector = resolve_branch_set(&repo, &opts, &branches)?;
                    archive_canonical(&git_repo, selector, None, Some("Archived from Stacksaw"))?;
                }
                MutatePlan::Split {
                    selector,
                    expected_record_revision,
                    step_id,
                    at_commit,
                    new_step_name,
                    no_ref,
                } => {
                    let selector = resolve_and_lease(
                        &repo,
                        &git_repo,
                        &opts,
                        &selector,
                        expected_record_revision.as_deref(),
                    )?;
                    let step = step_index(&selector, &step_id)?;
                    git_staircase::core::split(
                        &git_repo,
                        &selector.staircase,
                        step,
                        &at_commit,
                        new_step_name.as_deref(),
                        git_staircase::core::SplitOptions { no_ref },
                    )?;
                }
                MutatePlan::Join {
                    selector,
                    expected_record_revision,
                    lower_step_id,
                    upper_step_id,
                    keep_retired_ref,
                } => {
                    let selector = resolve_and_lease(
                        &repo,
                        &git_repo,
                        &opts,
                        &selector,
                        expected_record_revision.as_deref(),
                    )?;
                    let lower = step_index(&selector, &lower_step_id)?;
                    let upper = step_index(&selector, &upper_step_id)?;
                    git_staircase::core::join(
                        &git_repo,
                        &selector.staircase,
                        lower,
                        upper,
                        git_staircase::core::JoinOptions {
                            ref_action: if keep_retired_ref {
                                git_staircase::core::JoinRefAction::Keep
                            } else {
                                git_staircase::core::JoinRefAction::Delete
                            },
                        },
                    )?;
                }
                MutatePlan::CanonicalArchive {
                    selector,
                    expected_record_revision,
                    reason,
                } => {
                    let selector = resolve_and_lease(
                        &repo,
                        &git_repo,
                        &opts,
                        &selector,
                        expected_record_revision.as_deref(),
                    )?;
                    archive_canonical(
                        &git_repo,
                        selector,
                        expected_record_revision.as_deref(),
                        reason.as_deref(),
                    )?;
                }
                MutatePlan::Rebase {
                    selector,
                    expected_record_revision,
                    onto,
                    leave_upper_steps_stale,
                } => {
                    let selector = resolve_and_lease(
                        &repo,
                        &git_repo,
                        &opts,
                        &selector,
                        expected_record_revision.as_deref(),
                    )?;
                    git_staircase::core::rebase(
                        &git_repo,
                        &selector.staircase,
                        &onto,
                        git_staircase::core::RebaseOptions {
                            leave_upper_steps_stale,
                        },
                    )?;
                }
                MutatePlan::Restack {
                    selector,
                    expected_record_revision,
                    from_step_id,
                } => {
                    let selector = resolve_and_lease(
                        &repo,
                        &git_repo,
                        &opts,
                        &selector,
                        expected_record_revision.as_deref(),
                    )?;
                    if let Some(step_id) = from_step_id {
                        let from = step_index(&selector, &step_id)?;
                        git_staircase::core::restack_from(
                            &git_repo,
                            &selector.staircase,
                            from,
                            false,
                        )?;
                    } else {
                        git_staircase::core::restack(
                            &git_repo,
                            &selector.staircase,
                            git_staircase::core::RebaseOptions {
                                leave_upper_steps_stale: false,
                            },
                        )?;
                    }
                }
                MutatePlan::Name { selector, name } => {
                    let selector =
                        model::resolve_canonical_selector(&repo, &selector, &opts)?;
                    git_staircase::core::name_staircase(
                        &git_repo,
                        &selector,
                        &name,
                        false,
                    )?;
                }
                MutatePlan::Rename {
                    selector,
                    expected_record_revision,
                    name,
                } => {
                    let selector = resolve_and_lease(
                        &repo,
                        &git_repo,
                        &opts,
                        &selector,
                        expected_record_revision.as_deref(),
                    )?;
                    git_staircase::core::rename_staircase(
                        &git_repo,
                        &selector,
                        &name,
                        false,
                    )?;
                }
            }
            Ok(checkpoint)
        })
        .await??;
        let g = self.advance();
        self.emit(ChangeEvent::RefsChanged);
        Ok(MutateResult {
            generation: g,
            checkpoint,
            preview: None,
        })
    }

    /// Restore a checkpoint (§4 undo). Defaults to the newest when `checkpoint`
    /// is omitted.
    pub async fn undo(&self, checkpoint: Option<&str>) -> Result<MutateResult> {
        let repo_root = self.inner.repo_root.clone();
        let id = checkpoint.map(str::to_string);
        let restored = spawn_blocking(move || -> Result<String> {
            let ids = refs::list_checkpoints(&repo_root)?;
            let restore_id = match id {
                Some(c) => c,
                None => ids
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("no checkpoints to restore"))?,
            };
            refs::restore_checkpoint(&repo_root, &restore_id)?;
            Ok(restore_id)
        })
        .await??;
        let g = self.advance();
        self.emit(ChangeEvent::RefsChanged);
        Ok(MutateResult {
            generation: g,
            checkpoint: restored,
            preview: None,
        })
    }

    pub async fn checkpoints_list(&self) -> Result<Vec<String>> {
        let repo_root = self.inner.repo_root.clone();
        spawn_blocking(move || Ok(refs::list_checkpoints(&repo_root)?)).await?
    }

    pub async fn current_branch(&self) -> Result<Option<String>> {
        let repo_root = self.inner.repo_root.clone();
        spawn_blocking(move || {
            let repo = Repo::open(&repo_root)?;
            Ok(repo.head_branch()?)
        })
        .await?
    }



    pub async fn worktree_dirty(&self) -> Result<bool> {
        let repo_root = self.inner.repo_root.clone();
        spawn_blocking(move || Ok(snapshot::is_worktree_dirty(&repo_root)?)).await?
    }

    pub async fn edit_begin(&self, commit: &str) -> Result<EditBegin> {
        let repo_root = self.inner.repo_root.clone();
        let commit = commit.to_string();
        spawn_blocking(move || -> Result<EditBegin> {
            let repo = Repo::open(&repo_root)?;
            let r = edit::begin(&repo, &commit)?;
            Ok(EditBegin {
                schema_version: SCHEMA_VERSION,
                token: r.session.token,
                worktree: r.session.worktree.display().to_string(),
                commit: r.session.commit,
                descendants: r.descendants,
            })
        })
        .await?
    }

    pub async fn edit_finish(&self, token: &str, message: Option<&str>) -> Result<EditFinish> {
        let repo_root = self.inner.repo_root.clone();
        let token = token.to_string();
        let message = message.map(str::to_string);
        let result = spawn_blocking(move || -> Result<EditFinish> {
            let repo = Repo::open(&repo_root)?;
            let r = edit::finish(&repo, &token, message.as_deref())?;
            Ok(EditFinish {
                schema_version: SCHEMA_VERSION,
                rewrites: r
                    .rewrites
                    .into_iter()
                    .map(|(old, new)| Rewrite { old, new })
                    .collect(),
                updated_refs: r.updated_refs.into_iter().map(Into::into).collect(),
                checkpoint: r.checkpoint,
            })
        })
        .await??;
        self.advance();
        self.emit(ChangeEvent::RefsChanged);
        Ok(result)
    }

    pub async fn edit_abort(&self, token: &str) -> Result<()> {
        let repo_root = self.inner.repo_root.clone();
        let token = token.to_string();
        spawn_blocking(move || -> Result<()> {
            let repo = Repo::open(&repo_root)?;
            edit::abort(&repo, &token)?;
            Ok(())
        })
        .await?
    }
}

fn resolve_and_lease(
    repo: &Repo,
    git_repo: &git_staircase::GitRepo,
    opts: &ModelOptions,
    selector: &CanonicalSelector,
    expected_record: Option<&str>,
) -> Result<git_staircase::ResolvedSelector> {
    let resolved = model::resolve_canonical_selector(repo, selector, opts)?;
    if let Some(expected) = expected_record {
        if !resolved.is_managed() {
            bail!("an implicit structural key has no record revision lease");
        }
        let actual = git_staircase::core::read_record(
            git_repo,
            &git_staircase::core::refs::StaircaseRefs::state_record(
                &resolved.metadata().id,
            ),
        )?
        .record_oid;
        if actual != expected {
            bail!("stale record revision: expected {expected}, have {actual}");
        }
    }
    Ok(resolved)
}

fn step_index(selector: &git_staircase::ResolvedSelector, step_id: &str) -> Result<usize> {
    selector
        .metadata()
        .steps
        .iter()
        .position(|step| step.id == step_id)
        .ok_or_else(|| anyhow!("canonical step id '{step_id}' is not in the selected staircase"))
}

fn archive_canonical(
    git_repo: &git_staircase::GitRepo,
    mut selector: git_staircase::ResolvedSelector,
    expected_record: Option<&str>,
    reason: Option<&str>,
) -> Result<()> {
    if !selector.is_managed() {
        if expected_record.is_some() {
            bail!("an implicit structural key has no record revision lease");
        }
        let metadata = git_staircase::core::adopt(git_repo, selector.metadata())?;
        selector = git_staircase::ResolvedSelector {
            staircase: git_staircase::ResolvedStaircase::Managed(metadata),
            step_index: None,
        };
    }
    git_staircase::core::archive_staircase(
        git_repo,
        &selector,
        &git_staircase::core::ArchiveOptions {
            reason: reason.map(str::to_string),
            dry_run: false,
            snapshot_drafts: false,
            detach_dirty_worktrees: false,
            leave_worktrees: false,
        },
    )?;
    Ok(())
}

fn resolve_branch_set(
    repo: &Repo,
    opts: &ModelOptions,
    branches: &[String],
) -> Result<git_staircase::ResolvedSelector> {
    let mut requested = branches
        .iter()
        .map(|branch| {
            branch
                .strip_prefix("refs/heads/")
                .unwrap_or(branch)
                .to_string()
        })
        .collect::<Vec<_>>();
    requested.sort();
    requested.dedup();
    let staircase = stacksaw_git::build_staircases(repo, opts)?
        .into_iter()
        .find(|staircase| {
            let mut canonical = staircase
                .segments
                .iter()
                .filter_map(|segment| segment.canonical_branch.as_ref())
                .map(|branch| branch.short().to_string())
                .collect::<Vec<_>>();
            canonical.sort();
            canonical == requested
        })
        .ok_or_else(|| anyhow!("branches do not identify one canonical active staircase"))?;
    model::resolve_canonical_selector(repo, &staircase.selector, opts).map_err(Into::into)
}

fn apply_reshape_adapter(
    repo: &Repo,
    git_repo: &git_staircase::GitRepo,
    opts: &ModelOptions,
    target_oid: &str,
    op: &str,
) -> Result<()> {
    let staircase = stacksaw_git::build_staircases(repo, opts)?
        .into_iter()
        .find(|staircase| {
            staircase
                .segments
                .iter()
                .any(|segment| segment.commits.iter().any(|commit| commit.oid == target_oid))
        })
        .ok_or_else(|| anyhow!("commit is not in a canonical active staircase"))?;
    let mut resolved = model::resolve_canonical_selector(repo, &staircase.selector, opts)?;
    if !resolved.is_managed() {
        let metadata = git_staircase::core::adopt(git_repo, resolved.metadata())?;
        resolved = git_staircase::ResolvedSelector {
            staircase: git_staircase::ResolvedStaircase::Managed(metadata),
            step_index: None,
        };
    }
    let lineage_id = resolved.metadata().id.clone();
    let mut sequence = Vec::new();
    let mut boundaries = Vec::new();
    for segment in &staircase.segments {
        sequence.extend(segment.commits.iter().map(|commit| commit.oid.clone()));
        boundaries.push(sequence.len());
    }
    let position = sequence
        .iter()
        .position(|oid| oid == target_oid)
        .ok_or_else(|| anyhow!("selected commit disappeared from canonical decomposition"))?
        + 1;
    let new_boundaries = match op {
        "indent" => {
            let upper = boundaries
                .iter()
                .copied()
                .filter(|boundary| *boundary >= position)
                .min()
                .ok_or_else(|| anyhow!("selected commit has no enclosing step"))?;
            let mut next = boundaries
                .iter()
                .copied()
                .filter(|boundary| !(*boundary == upper && upper < sequence.len()))
                .collect::<Vec<_>>();
            if position > 1 && !next.contains(&(position - 1)) {
                next.push(position - 1);
            }
            next.sort_unstable();
            next.dedup();
            next
        }
        "unindent" => {
            let previous = boundaries
                .iter()
                .copied()
                .filter(|boundary| *boundary < position)
                .max();
            let start = previous.map_or(1, |boundary| boundary + 1);
            let mut next = boundaries.clone();
            if start > 1 {
                next.retain(|boundary| *boundary != start - 1);
            }
            if !next.contains(&position) {
                next.push(position);
            }
            next.sort_unstable();
            next.dedup();
            next
        }
        other => bail!("unknown reshape op {other}"),
    };
    if boundaries == new_boundaries {
        return Ok(());
    }
    let old = boundaries.iter().copied().collect::<std::collections::HashSet<_>>();
    let new = new_boundaries
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    for boundary in old.difference(&new).copied().collect::<Vec<_>>() {
        let cut = &sequence[boundary - 1];
        if let Some(index) = resolved
            .metadata()
            .steps
            .iter()
            .position(|step| &step.cut == cut)
        {
            git_staircase::core::join(
                git_repo,
                &resolved.staircase,
                index,
                index + 1,
                git_staircase::core::JoinOptions {
                    ref_action: git_staircase::core::JoinRefAction::Delete,
                },
            )?;
            resolved = git_staircase::ResolvedSelector {
                staircase: git_staircase::core::resolve_by_id(git_repo, &lineage_id)?,
                step_index: None,
            };
        }
    }
    for boundary in new.difference(&old).copied().collect::<Vec<_>>() {
        let cut = &sequence[boundary - 1];
        let mut previous = git_repo.resolve_commit(&resolved.metadata().target)?;
        let index = resolved
            .metadata()
            .steps
            .iter()
            .enumerate()
            .find_map(|(index, step)| {
                let contains = git_repo.is_ancestor(&previous, cut).unwrap_or(false)
                    && git_repo.is_ancestor(cut, &step.cut).unwrap_or(false);
                previous = step.cut.clone();
                contains.then_some(index)
            })
            .ok_or_else(|| anyhow!("new boundary is outside canonical step cuts"))?;
        git_staircase::core::split(
            git_repo,
            &resolved.staircase,
            index,
            cut,
            None,
            git_staircase::core::SplitOptions { no_ref: false },
        )?;
        resolved = git_staircase::ResolvedSelector {
            staircase: git_staircase::core::resolve_by_id(git_repo, &lineage_id)?,
            step_index: None,
        };
    }
    Ok(())
}


