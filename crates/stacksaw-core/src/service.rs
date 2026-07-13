//! The per-repo core service state (§3.1, P2 one source of truth).
//!
//! The **only** crate that touches `stacksaw_git` for live repo operations.
//! UI and CLI clients talk to this through the [`crate::core::Core`] handle.



use anyhow::{anyhow, bail, Result};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use stacksaw_git::archive;
use stacksaw_git::edit;
use stacksaw_git::model::ModelOptions;
use stacksaw_git::refs::{self, git};
use stacksaw_git::reshape::{self, Op};
use stacksaw_git::{
    build_snapshot, changed_files, commit_message, file_content, file_diff, snapshot,
    DiffProcessor, Repo,
};
use stacksaw_ssp::types::{
    ChangeView, CommitDetail, CommitRecord, EditBegin, EditFinish, MutatePlan,
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
        let default_upstream = if upstream.is_empty() {
            None
        } else if upstream.starts_with("refs/") {
            Some(upstream.clone())
        } else {
            Some(format!("refs/remotes/{upstream}"))
        };
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
        let head = Repo::open(&self.inner.repo_root)
            .ok()
            .and_then(|r| r.head_oid().ok().flatten())
            .map(|o| stacksaw_ssp::git_ref::GitRef::new(o.to_string()));
        Snapshot {
            schema_version: SCHEMA_VERSION,
            generation: self.generation(),
            head,
            detached: false,
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
        spawn_blocking(move || -> Result<()> {
            let repo = Repo::open(&repo_root)?;
            match plan {
                MutatePlan::Reshape { target_oid, op } => {
                    let op = match op.as_str() {
                        "indent" => Op::Indent,
                        "unindent" => Op::Unindent,
                        other => bail!("unknown reshape op {other}"),
                    };
                    let _ = reshape::apply(&repo, &opts, &target_oid, op)?;
                }
                MutatePlan::Archive { branches } => {
                    let _ = archive::archive(&repo, &opts, &branches)?;
                }
            }
            Ok(())
        })
        .await??;
        let g = self.advance();
        self.emit(ChangeEvent::RefsChanged);
        let checkpoint = self
            .checkpoints_list()
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
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


