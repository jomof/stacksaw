//! The per-repo core service state (§3.1, P2 one source of truth).
//!
//! The **only** crate that touches `stacksaw_git` for live repo operations.
//! UI and CLI clients talk to this through the [`crate::core::Core`] handle.

use crate::handle::RepositoryHandle;
use async_trait::async_trait;

use anyhow::{anyhow, bail, Result};
use std::fs;
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
use stacksaw_lint::{collect_findings, default_builtins, FileChange, LintJob, Profile};
use stacksaw_ssp::types::{
    ChangeView, CommitDetail, CommitRecord, EditBegin, EditFinish, Finding, MutatePlan,
    MutateResult, ReviewNote, Rewrite, Snapshot, SCHEMA_VERSION, WORKTREE_OID,
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

    pub fn notes_dir(&self) -> PathBuf {
        self.inner.git_dir.join("stacksaw").join("notes")
    }

    pub async fn note_add(&self, file: &str, line: u32, text: &str) -> Result<ReviewNote> {
        let notes_dir = self.notes_dir();
        fs::create_dir_all(&notes_dir)?;
        let id =
            blake3::hash(format!("{file}:{line}:{text}").as_bytes()).to_hex()[..12].to_string();
        let note = ReviewNote {
            schema_version: SCHEMA_VERSION,
            id: id.clone(),
            source: "note:me".into(),
            file: file.into(),
            line,
            text: text.into(),
            ts: jiff::Timestamp::now().to_string(),
        };
        fs::write(
            notes_dir.join(format!("{id}.json")),
            serde_json::to_vec_pretty(&note)?,
        )?;
        Ok(note)
    }

    pub async fn note_list(&self) -> Result<Vec<ReviewNote>> {
        let notes_dir = self.notes_dir();
        let mut notes = Vec::new();
        if let Ok(entries) = fs::read_dir(&notes_dir) {
            for e in entries.flatten() {
                if let Ok(bytes) = fs::read(e.path()) {
                    if let Ok(n) = serde_json::from_slice::<ReviewNote>(&bytes) {
                        notes.push(n);
                    }
                }
            }
        }
        Ok(notes)
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

    /// Lint a set of commits under the given profile, returning findings.
    pub async fn lint(&self, commits: Vec<String>, profile: Profile) -> Result<Vec<Finding>> {
        let repo_root = self.inner.repo_root.clone();
        let findings = spawn_blocking(move || -> Result<Vec<Finding>> {
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
) -> Result<Vec<LintJob>> {
    let mut jobs = Vec::new();
    for rev in commits {
        let oid = repo.resolve(rev)?;
        let meta = repo.commit_meta(oid)?;
        let short = meta.short();

        let file_specs = repo.tree_diff(meta.parents.first().cloned(), oid)?;
        let paths: Vec<&str> = file_specs.iter().map(|(p, _)| p.as_str()).collect();
        let contents = repo
            .read_blobs(oid, &paths)
            .unwrap_or_else(|_| vec![None; paths.len()]);

        let mut files = Vec::new();
        for ((path, status), content) in file_specs.into_iter().zip(contents) {
            files.push(FileChange {
                path,
                old_oid: None,
                new_oid: None,
                changed_ranges: vec![],
                content,
                added: status == 'A',
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

#[async_trait]
impl RepositoryHandle for Service {
    async fn generation(&self) -> u64 {
        self.generation()
    }
    async fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        self.subscribe()
    }
    async fn snapshot(&self) -> Result<Snapshot> {
        self.snapshot().await
    }
    async fn commit_detail(&self, oid: &str) -> Result<CommitDetail> {
        self.commit_detail(oid).await
    }
    async fn commit_show(&self, rev: &str) -> Result<CommitRecord> {
        self.commit_show(rev).await
    }
    async fn change_view(&self, commit: &str, path: &str) -> Result<ChangeView> {
        self.change_view(commit, path).await
    }
    async fn diff_range(&self, args: &[String]) -> Result<String> {
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.diff_range(&refs).await
    }
    async fn diff_interdiff(&self, a: &str, b: &str) -> Result<String> {
        self.diff_interdiff(a, b).await
    }
    async fn mutate(&self, plan: MutatePlan, if_generation: Option<u64>) -> Result<MutateResult> {
        self.mutate(plan, if_generation).await
    }
    async fn undo(&self, checkpoint: Option<&str>) -> Result<MutateResult> {
        self.undo(checkpoint).await
    }
    async fn checkpoints_list(&self) -> Result<Vec<String>> {
        self.checkpoints_list().await
    }
    async fn worktree_dirty(&self) -> Result<bool> {
        self.worktree_dirty().await
    }
    async fn current_branch(&self) -> Result<Option<String>> {
        self.current_branch().await
    }
    async fn note_add(&self, file: &str, line: u32, text: &str) -> Result<ReviewNote> {
        self.note_add(file, line, text).await
    }
    async fn note_list(&self) -> Result<Vec<ReviewNote>> {
        self.note_list().await
    }
    async fn lint(&self, commits: Vec<String>, profile: Profile) -> Result<Vec<Finding>> {
        self.lint(commits, profile).await
    }
    async fn edit_begin(&self, commit: &str) -> Result<EditBegin> {
        self.edit_begin(commit).await
    }
    async fn edit_finish(&self, token: &str, message: Option<&str>) -> Result<EditFinish> {
        self.edit_finish(token, message).await
    }
    async fn edit_abort(&self, token: &str) -> Result<()> {
        self.edit_abort(token).await
    }
    fn drain_prober(&self) -> bool {
        self.drain_prober()
    }
}
