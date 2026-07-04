//! Shared repo/config context for CLI commands.

use std::path::PathBuf;

use anyhow::Context as _;
use stacksaw_core::config::{self, Config};
use stacksaw_git::model::ModelOptions;
use stacksaw_git::Repo;

pub struct Ctx {
    pub repo_root: PathBuf,
    pub git_dir: PathBuf,
    pub config: Config,
    pub upstream_default: String,
}

impl Ctx {
    /// Discover the repo from the current directory and load layered config.
    pub fn open(upstream_override: Option<String>) -> anyhow::Result<Ctx> {
        let cwd = std::env::current_dir()?;
        let repo = Repo::discover(&cwd).context("not inside a git repository")?;
        let git_dir = repo.common_dir();
        let repo_root = repo.workdir().unwrap_or_else(|| cwd.clone());
        let (config, _prov) = config::load(&repo_root, &git_dir);
        let upstream_default = upstream_override.unwrap_or_else(|| config.upstream.default.clone());
        Ok(Ctx {
            repo_root,
            git_dir,
            config,
            upstream_default,
        })
    }

    pub fn repo(&self) -> anyhow::Result<Repo> {
        Ok(Repo::open(&self.repo_root)?)
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
