//! The unified client handle: in-process [`Service`] or attached daemon (§3.1).
//!
//! Every UI/CLI face talks to the repo **only** through this type. Transport is
//! an implementation detail — semantics are identical.

use std::path::{Path, PathBuf};

use stacksaw_lint::Profile;
use stacksaw_ssp::method::ClientKind;
use stacksaw_ssp::types::{
    ChangeView, CommitDetail, CommitRecord, EditBegin, EditFinish, Finding, MutatePlan,
    MutateResult, ReviewNote, Snapshot,
};
use tokio::sync::broadcast;

use crate::client::SspClient;
use crate::config::Config;
use crate::service::{ChangeEvent, Service};
use crate::watch::{self, WatchGuard};

/// Dual-transport handle to the per-repo semantic core.
pub struct Core {
    repo_root: PathBuf,
    git_dir: PathBuf,
    config: Config,
    inner: CoreInner,
}

enum CoreInner {
    InProcess {
        service: Service,
        _watch: WatchGuard,
    },
    Remote(SspClient),
}

impl Core {
    /// Attach to a running daemon when healthy; otherwise run [`Service`]
    /// in-process with a filesystem watcher (hermetic for CI, warm when shared).
    pub async fn attach_or_local(
        repo_root: PathBuf,
        git_dir: PathBuf,
        config: Config,
        kind: ClientKind,
    ) -> anyhow::Result<Self> {
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
                inner: CoreInner::Remote(client),
            });
        }
        let service = Service::new(repo_root.clone(), git_dir.clone(), config.clone());
        let watch_guard = watch::spawn(service.clone())?;
        Ok(Core {
            repo_root,
            git_dir,
            config,
            inner: CoreInner::InProcess {
                service,
                _watch: watch_guard,
            },
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
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.generation(),
            CoreInner::Remote(c) => c.generation().await.unwrap_or(0),
        }
    }

    pub async fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.subscribe(),
            CoreInner::Remote(c) => c.subscribe_events().await,
        }
    }

    pub async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.snapshot().await,
            CoreInner::Remote(c) => c.snapshot().await,
        }
    }

    pub async fn commit_detail(&self, oid: &str) -> anyhow::Result<CommitDetail> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.commit_detail(oid).await,
            CoreInner::Remote(c) => c.commit_detail(oid).await,
        }
    }

    pub async fn commit_show(&self, rev: &str) -> anyhow::Result<CommitRecord> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.commit_show(rev).await,
            CoreInner::Remote(c) => c.commit_show(rev).await,
        }
    }

    pub async fn change_view(&self, commit: &str, path: &str) -> anyhow::Result<ChangeView> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.change_view(commit, path).await,
            CoreInner::Remote(c) => c.change_view(commit, path).await,
        }
    }

    pub async fn diff_range(&self, args: &[&str]) -> anyhow::Result<String> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.diff_range(args).await,
            CoreInner::Remote(c) => c.diff_range(args).await,
        }
    }

    pub async fn diff_interdiff(&self, a: &str, b: &str) -> anyhow::Result<String> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.diff_interdiff(a, b).await,
            CoreInner::Remote(c) => c.diff_interdiff(a, b).await,
        }
    }

    pub async fn mutate(
        &self,
        plan: MutatePlan,
        if_generation: Option<u64>,
    ) -> anyhow::Result<MutateResult> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.mutate(plan, if_generation).await,
            CoreInner::Remote(c) => c.mutate(plan, if_generation).await,
        }
    }

    pub async fn undo(&self, checkpoint: Option<&str>) -> anyhow::Result<MutateResult> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.undo(checkpoint).await,
            CoreInner::Remote(c) => c.undo(checkpoint).await,
        }
    }

    pub async fn checkpoints_list(&self) -> anyhow::Result<Vec<String>> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.checkpoints_list().await,
            CoreInner::Remote(c) => c.checkpoints_list().await,
        }
    }

    pub async fn worktree_dirty(&self) -> anyhow::Result<bool> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.worktree_dirty().await,
            CoreInner::Remote(c) => c.worktree_dirty().await,
        }
    }

    pub async fn current_branch(&self) -> anyhow::Result<Option<String>> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.current_branch().await,
            CoreInner::Remote(c) => c.current_branch().await,
        }
    }

    pub async fn note_add(&self, file: &str, line: u32, text: &str) -> anyhow::Result<ReviewNote> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.note_add(file, line, text).await,
            CoreInner::Remote(c) => c.note_add(file, line, text).await,
        }
    }

    pub async fn note_list(&self) -> anyhow::Result<Vec<ReviewNote>> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.note_list().await,
            CoreInner::Remote(c) => c.note_list().await,
        }
    }

    pub async fn lint(
        &self,
        commits: Vec<String>,
        profile: Profile,
    ) -> anyhow::Result<Vec<Finding>> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.lint(commits, profile).await,
            CoreInner::Remote(c) => c.lint(commits, profile).await,
        }
    }

    pub async fn edit_begin(&self, commit: &str) -> anyhow::Result<EditBegin> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.edit_begin(commit).await,
            CoreInner::Remote(c) => c.edit_begin(commit).await,
        }
    }

    pub async fn edit_finish(
        &self,
        token: &str,
        message: Option<&str>,
    ) -> anyhow::Result<EditFinish> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.edit_finish(token, message).await,
            CoreInner::Remote(c) => c.edit_finish(token, message).await,
        }
    }

    pub async fn edit_abort(&self, token: &str) -> anyhow::Result<()> {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.edit_abort(token).await,
            CoreInner::Remote(c) => c.edit_abort(token).await,
        }
    }

    /// Drain background probe verdicts (in-process only; no-op on remote).
    pub fn drain_prober(&self) -> bool {
        match &self.inner {
            CoreInner::InProcess { service, .. } => service.drain_prober(),
            CoreInner::Remote(_) => false,
        }
    }
}
