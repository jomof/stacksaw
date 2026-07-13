//! The unified client handle: in-process [`Service`] (§3.1).
//!
//! Every UI/CLI face talks to the repo **only** through this type.

use std::path::{Path, PathBuf};
use anyhow::Result;

use stacksaw_ssp::method::ClientKind;
use stacksaw_ssp::types::{
    ChangeView, CommitDetail, CommitRecord, EditBegin, EditFinish, MutatePlan,
    MutateResult, Snapshot,
};
use tokio::sync::broadcast;

use crate::config::Config;
use crate::service::{ChangeEvent, Service};
use crate::watch::{self, WatchGuard};

use std::sync::Arc;

/// Direct in-process handle to the per-repo semantic core.
#[derive(Clone)]
pub struct Core {
    repo_root: PathBuf,
    git_dir: PathBuf,
    config: Config,
    service: Service,
    _watch: Arc<WatchGuard>,
}

impl Core {
    pub async fn attach_or_local(
        repo_root: PathBuf,
        git_dir: PathBuf,
        config: Config,
        _kind: ClientKind,
    ) -> Result<Self> {
        let service = Service::new(repo_root.clone(), git_dir.clone(), config.clone());
        let watch_guard = Arc::new(watch::spawn(service.clone())?);
        Ok(Core {
            repo_root,
            git_dir,
            config,
            service,
            _watch: watch_guard,
        })
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub async fn generation(&self) -> u64 {
        self.service.generation()
    }

    pub async fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        self.service.subscribe()
    }

    pub fn emit(&self, event: ChangeEvent) {
        self.service.emit(event);
    }

    pub fn fast_snapshot(&self) -> Snapshot {
        self.service.fast_snapshot()
    }

    pub async fn snapshot(&self) -> Result<Snapshot> {
        self.service.snapshot().await
    }

    pub async fn commit_detail(&self, oid: &str) -> Result<CommitDetail> {
        self.service.commit_detail(oid).await
    }

    pub async fn commit_show(&self, rev: &str) -> Result<CommitRecord> {
        self.service.commit_show(rev).await
    }

    pub async fn change_view(&self, commit: &str, path: &str) -> Result<ChangeView> {
        self.service.change_view(commit, path).await
    }

    pub async fn diff_range(&self, args: &[&str]) -> Result<String> {
        self.service.diff_range(args).await
    }

    pub async fn diff_interdiff(&self, a: &str, b: &str) -> Result<String> {
        self.service.diff_interdiff(a, b).await
    }

    pub async fn mutate(
        &self,
        plan: MutatePlan,
        if_generation: Option<u64>,
    ) -> Result<MutateResult> {
        self.service.mutate(plan, if_generation).await
    }

    pub async fn undo(&self, checkpoint: Option<&str>) -> Result<MutateResult> {
        self.service.undo(checkpoint).await
    }

    pub async fn checkpoints_list(&self) -> Result<Vec<String>> {
        self.service.checkpoints_list().await
    }

    pub async fn worktree_dirty(&self) -> Result<bool> {
        self.service.worktree_dirty().await
    }

    pub async fn current_branch(&self) -> Result<Option<String>> {
        self.service.current_branch().await
    }

    pub async fn edit_begin(&self, commit: &str) -> Result<EditBegin> {
        self.service.edit_begin(commit).await
    }

    pub async fn edit_finish(&self, token: &str, message: Option<&str>) -> Result<EditFinish> {
        self.service.edit_finish(token, message).await
    }

    pub async fn edit_abort(&self, token: &str) -> Result<()> {
        self.service.edit_abort(token).await
    }

    /// Drain background probe verdicts.
    pub fn drain_prober(&self) -> bool {
        self.service.drain_prober()
    }
}
