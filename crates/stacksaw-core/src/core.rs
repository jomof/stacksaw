//! The unified client handle: in-process [`Service`] or attached daemon (§3.1).
//!
//! Every UI/CLI face talks to the repo **only** through this type. Transport is
//! an implementation detail — semantics are identical.

use anyhow::Result;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use stacksaw_lint::Profile;
use stacksaw_ssp::method::ClientKind;
use stacksaw_ssp::types::{
    ChangeView, CommitDetail, CommitRecord, EditBegin, EditFinish, Finding, MutatePlan,
    MutateResult, ReviewNote, Snapshot,
};
use tokio::sync::broadcast;

use crate::client::SspClient;
use crate::config::Config;
use crate::handle::RepositoryHandle;
use crate::service::{ChangeEvent, Service};
use crate::watch::{self, WatchGuard};

/// Dual-transport handle to the per-repo semantic core.
pub struct Core {
    repo_root: PathBuf,
    git_dir: PathBuf,
    config: Config,
    inner: Box<dyn RepositoryHandle>,
}

struct LocalHandle {
    service: Service,
    _watch: WatchGuard,
}

#[async_trait]
impl RepositoryHandle for LocalHandle {
    async fn generation(&self) -> u64 {
        self.service.generation()
    }
    async fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        self.service.subscribe()
    }
    async fn snapshot(&self) -> Result<Snapshot> {
        self.service.snapshot().await
    }
    async fn commit_detail(&self, oid: &str) -> Result<CommitDetail> {
        self.service.commit_detail(oid).await
    }
    async fn commit_show(&self, rev: &str) -> Result<CommitRecord> {
        self.service.commit_show(rev).await
    }
    async fn change_view(&self, commit: &str, path: &str) -> Result<ChangeView> {
        self.service.change_view(commit, path).await
    }
    async fn diff_range(&self, args: &[String]) -> Result<String> {
        RepositoryHandle::diff_range(&self.service, args).await
    }
    async fn diff_interdiff(&self, a: &str, b: &str) -> Result<String> {
        self.service.diff_interdiff(a, b).await
    }
    async fn mutate(&self, plan: MutatePlan, if_generation: Option<u64>) -> Result<MutateResult> {
        self.service.mutate(plan, if_generation).await
    }
    async fn undo(&self, checkpoint: Option<&str>) -> Result<MutateResult> {
        self.service.undo(checkpoint).await
    }
    async fn checkpoints_list(&self) -> Result<Vec<String>> {
        self.service.checkpoints_list().await
    }
    async fn worktree_dirty(&self) -> Result<bool> {
        self.service.worktree_dirty().await
    }
    async fn current_branch(&self) -> Result<Option<String>> {
        self.service.current_branch().await
    }
    async fn note_add(&self, file: &str, line: u32, text: &str) -> Result<ReviewNote> {
        self.service.note_add(file, line, text).await
    }
    async fn note_list(&self) -> Result<Vec<ReviewNote>> {
        self.service.note_list().await
    }
    async fn lint(&self, commits: Vec<String>, profile: Profile) -> Result<Vec<Finding>> {
        self.service.lint(commits, profile).await
    }
    async fn edit_begin(&self, commit: &str) -> Result<EditBegin> {
        self.service.edit_begin(commit).await
    }
    async fn edit_finish(&self, token: &str, message: Option<&str>) -> Result<EditFinish> {
        self.service.edit_finish(token, message).await
    }
    async fn edit_abort(&self, token: &str) -> Result<()> {
        self.service.edit_abort(token).await
    }
    fn drain_prober(&self) -> bool {
        self.service.drain_prober()
    }
}

impl Core {
    /// Attach to a running daemon when healthy; otherwise run [`Service`]
    /// in-process with a filesystem watcher (hermetic for CI, warm when shared).
    pub async fn attach_or_local(
        repo_root: PathBuf,
        git_dir: PathBuf,
        config: Config,
        kind: ClientKind,
    ) -> Result<Self> {
        let kind_str = match kind {
            ClientKind::Ui => "ui",
            ClientKind::Cli => "cli",
            ClientKind::Automation => "automation",
        };
        if let Some(client) = SspClient::attach(&git_dir, kind_str).await {
            return Ok(Core {
                repo_root: repo_root.clone(),
                git_dir: git_dir.clone(),
                config: config.clone(),
                inner: Box::new(client),
            });
        }
        let service = Service::new(repo_root.clone(), git_dir.clone(), config.clone());
        let watch_guard = watch::spawn(service.clone())?;
        Ok(Core {
            repo_root,
            git_dir,
            config,
            inner: Box::new(LocalHandle {
                service,
                _watch: watch_guard,
            }),
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
        self.inner.generation().await
    }

    pub async fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        self.inner.subscribe().await
    }

    pub async fn snapshot(&self) -> Result<Snapshot> {
        self.inner.snapshot().await
    }

    pub async fn commit_detail(&self, oid: &str) -> Result<CommitDetail> {
        self.inner.commit_detail(oid).await
    }

    pub async fn commit_show(&self, rev: &str) -> Result<CommitRecord> {
        self.inner.commit_show(rev).await
    }

    pub async fn change_view(&self, commit: &str, path: &str) -> Result<ChangeView> {
        self.inner.change_view(commit, path).await
    }

    pub async fn diff_range(&self, args: &[&str]) -> Result<String> {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        self.inner.diff_range(&args).await
    }

    pub async fn diff_interdiff(&self, a: &str, b: &str) -> Result<String> {
        self.inner.diff_interdiff(a, b).await
    }

    pub async fn mutate(
        &self,
        plan: MutatePlan,
        if_generation: Option<u64>,
    ) -> Result<MutateResult> {
        self.inner.mutate(plan, if_generation).await
    }

    pub async fn undo(&self, checkpoint: Option<&str>) -> Result<MutateResult> {
        self.inner.undo(checkpoint).await
    }

    pub async fn checkpoints_list(&self) -> Result<Vec<String>> {
        self.inner.checkpoints_list().await
    }

    pub async fn worktree_dirty(&self) -> Result<bool> {
        self.inner.worktree_dirty().await
    }

    pub async fn current_branch(&self) -> Result<Option<String>> {
        self.inner.current_branch().await
    }

    pub async fn note_add(&self, file: &str, line: u32, text: &str) -> Result<ReviewNote> {
        self.inner.note_add(file, line, text).await
    }

    pub async fn note_list(&self) -> Result<Vec<ReviewNote>> {
        self.inner.note_list().await
    }

    pub async fn lint(&self, commits: Vec<String>, profile: Profile) -> Result<Vec<Finding>> {
        self.inner.lint(commits, profile).await
    }

    pub async fn edit_begin(&self, commit: &str) -> Result<EditBegin> {
        self.inner.edit_begin(commit).await
    }

    pub async fn edit_finish(&self, token: &str, message: Option<&str>) -> Result<EditFinish> {
        self.inner.edit_finish(token, message).await
    }

    pub async fn edit_abort(&self, token: &str) -> Result<()> {
        self.inner.edit_abort(token).await
    }

    /// Drain background probe verdicts (in-process only; no-op on remote).
    pub fn drain_prober(&self) -> bool {
        self.inner.drain_prober()
    }
}
