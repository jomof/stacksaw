//! Shared repo/config context for CLI commands.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use stacksaw_core::config::{self, Config};
use stacksaw_git::model::ModelOptions;
use stacksaw_git::Repo;

pub struct Ctx {
    pub repo_root: PathBuf,
    pub git_dir: PathBuf,
    /// The directory the context was opened at (the monorepo sub-project the
    /// user launched in). Used as the working directory for context-run
    /// commands so they land in the right subtree, not the repo root.
    pub context_dir: PathBuf,
    pub config: Config,
    pub upstream_default: String,
}

impl Ctx {
    /// Discover the repo from the current directory and load layered config.
    pub fn open(upstream_override: Option<String>) -> anyhow::Result<Ctx> {
        let cwd = env::current_dir()?;
        Ctx::open_at(&cwd, upstream_override)
    }

    /// Like [`open`](Self::open) but discovers the repo from `dir` rather than
    /// the process's current directory. Used by the TUI to switch the window to
    /// another repo in place (no re-exec).
    pub fn open_at(dir: &Path, upstream_override: Option<String>) -> anyhow::Result<Ctx> {
        let repo = Repo::discover(dir).context("not inside a git repository")?;
        let git_dir = repo.common_dir();
        let repo_root = repo.workdir().unwrap_or_else(|| dir.to_path_buf());
        let context_dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let (config, _prov) = config::load(&repo_root, &git_dir);
        let upstream_default = upstream_override.unwrap_or_else(|| config.upstream.default.clone());
        Ok(Ctx {
            repo_root,
            git_dir,
            context_dir,
            config,
            upstream_default,
        })
    }

    pub fn repo(&self) -> anyhow::Result<Repo> {
        Ok(Repo::open(&self.repo_root)?)
    }

    /// The context directory as a path relative to the repo root (the monorepo
    /// sub-project). Empty when the context is the repo root itself. Used to
    /// place command execution in the same subtree inside an ephemeral
    /// worktree.
    pub fn rel_subdir(&self) -> PathBuf {
        let root =
            fs::canonicalize(&self.repo_root).unwrap_or_else(|_| self.repo_root.clone());
        self.context_dir
            .strip_prefix(&root)
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    pub fn model_options(&self) -> ModelOptions {
        // Resolve the configured default to a concrete ref name if it is a
        // remote-tracking form like `origin/main`.
        let default = if self.upstream_default.starts_with("refs/") {
            self.upstream_default.clone()
        } else {
            format!("refs/remotes/{}", self.upstream_default)
        };
        ModelOptions {
            default_upstream: Some(default),
        }
    }
}
