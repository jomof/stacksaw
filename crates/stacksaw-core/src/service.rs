//! The per-repo core service state (§3.1, P2 one source of truth).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use stacksaw_git::model::ModelOptions;
use stacksaw_git::{build_snapshot, Repo};
use stacksaw_lint::{collect_findings, default_builtins, FileChange, LintJob, Profile};
use stacksaw_ssp::types::{Finding, Snapshot};
use tokio::sync::broadcast;
use tokio::task::spawn_blocking;

use crate::config::Config;

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

    /// Build a fresh snapshot at the current generation. Runs the (non-`Send`)
    /// gix reads on a blocking thread.
    pub async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let repo_root = self.inner.repo_root.clone();
        let generation = self.generation();
        let default_upstream = Some(self.inner.config.upstream.default.clone());
        let snap = spawn_blocking(move || -> anyhow::Result<Snapshot> {
            let repo = Repo::open(&repo_root)?;
            let opts = ModelOptions { default_upstream };
            Ok(build_snapshot(&repo, generation, &opts)?)
        })
        .await??;
        Ok(snap)
    }

    /// Lint a set of commits under the given profile, returning findings.
    pub async fn lint(
        &self,
        commits: Vec<String>,
        profile: Profile,
    ) -> anyhow::Result<Vec<Finding>> {
        let repo_root = self.inner.repo_root.clone();
        let findings = spawn_blocking(move || -> anyhow::Result<Vec<Finding>> {
            let repo = Repo::open(&repo_root)?;
            let jobs = build_lint_jobs(&repo, &repo_root, &commits, profile)?;
            let linters = default_builtins();
            let outcomes = stacksaw_lint::run(&jobs, &linters);
            let (findings, _errors) = collect_findings(outcomes);
            Ok(findings)
        })
        .await??;
        Ok(findings)
    }
}

/// Build per-commit lint jobs from git, populating changed files + content.
pub fn build_lint_jobs(
    repo: &Repo,
    repo_root: &Path,
    commits: &[String],
    profile: Profile,
) -> anyhow::Result<Vec<LintJob>> {
    use stacksaw_git::refs::git;

    let mut jobs = Vec::new();
    for rev in commits {
        let oid = repo.resolve(rev)?;
        let meta = repo.commit_meta(oid)?;
        let short = meta.short();

        // Changed files vs first parent (or empty tree for root commits).
        let name_status = if meta.parents.is_empty() {
            git(
                repo_root,
                &["show", "--name-status", "--format=", &oid.to_string()],
            )?
        } else {
            git(
                repo_root,
                &[
                    "diff",
                    "--name-status",
                    &format!("{}^", oid),
                    &oid.to_string(),
                ],
            )?
        };
        let mut file_specs = Vec::new();
        for line in name_status.lines() {
            let mut parts = line.split('\t');
            let Some(status) = parts.next() else { continue };
            let Some(path) = parts.next() else { continue };
            let added = status.starts_with('A');
            file_specs.push((path.to_string(), added));
        }

        let paths: Vec<&str> = file_specs.iter().map(|(p, _)| p.as_str()).collect();
        let contents = repo
            .read_blobs(oid, &paths)
            .unwrap_or_else(|_| vec![None; paths.len()]);

        let mut files = Vec::new();
        for ((path, added), content) in file_specs.into_iter().zip(contents) {
            files.push(FileChange {
                path,
                old_oid: None,
                new_oid: None,
                changed_ranges: vec![],
                content,
                added,
            });
        }

        let author_year = jiff::Timestamp::from_second(meta.author_time)
            .ok()
            .map(|t| t.strftime("%Y").to_string().parse::<i32>().unwrap_or(1970))
            .unwrap_or(1970);

        jobs.push(LintJob {
            commit: short,
            author_year,
            message: format!("{}\n\n{}", meta.subject, meta.body),
            files,
            repo_root: repo_root.to_path_buf(),
            worktree: repo_root.to_path_buf(),
            profile,
        });
    }
    Ok(jobs)
}
