//! Tier-2 external command linters — the primary extension point (§7.4).
//!
//! stacksaw execs the command with a JSON job on stdin and reads a JSON
//! findings array on stdout. A nonzero exit without valid JSON is a linter
//! *error* (surfaced, not fatal). Because this is arbitrary code execution, the
//! caller MUST have cleared the repo trust gate (§7.3) before constructing one.

use std::env::temp_dir;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::Serialize;
use stacksaw_ssp::types::Finding;

use crate::linter::{FileChange, LintError, LintJob, Linter};

/// How the working directory is presented to the command (§7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    /// cwd pinned to a read-only checkout of the commit's tree.
    Tree,
    /// cwd is the live worktree.
    Worktree,
}

pub struct ExternalLinter {
    pub id: String,
    pub version: String,
    pub command: String,
    pub args: Vec<String>,
    pub mode: ExecMode,
    pub timeout: Duration,
    pub cache_dir: PathBuf,
    /// Opaque config passed through to the command as `configBlob`.
    pub config_blob: serde_json::Value,
    pub content_pure: bool,
}

impl Default for ExternalLinter {
    fn default() -> Self {
        ExternalLinter {
            id: "external".into(),
            version: "0".into(),
            command: String::new(),
            args: vec![],
            mode: ExecMode::Tree,
            timeout: Duration::from_secs(30),
            cache_dir: temp_dir(),
            config_blob: serde_json::Value::Null,
            content_pure: false,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExternalJob<'a> {
    commit: &'a str,
    files: &'a [FileChange],
    repo_root: &'a Path,
    worktree: &'a Path,
    config_blob: &'a serde_json::Value,
    cache_dir: &'a Path,
}

impl Linter for ExternalLinter {
    fn id(&self) -> &str {
        &self.id
    }
    fn version(&self) -> &str {
        &self.version
    }
    fn content_pure(&self) -> bool {
        self.content_pure
    }

    fn run(&self, job: &LintJob) -> Result<Vec<Finding>, LintError> {
        let payload = ExternalJob {
            commit: &job.commit,
            files: &job.files,
            repo_root: &job.repo_root,
            worktree: &job.worktree,
            config_blob: &self.config_blob,
            cache_dir: &self.cache_dir,
        };
        let stdin_bytes = serde_json::to_vec(&payload)?;

        let cwd = match self.mode {
            ExecMode::Tree | ExecMode::Worktree => job.worktree.clone(),
        };

        let mut child = Command::new(&self.command)
            .args(&self.args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdin_handle = std::thread::spawn(move || {
            stdin.write_all(&stdin_bytes)
        });

        let mut stdout = child.stdout.take().expect("stdout piped");
        let stdout_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            use std::io::Read;
            stdout.read_to_end(&mut buf).map(|_| buf)
        });

        let mut stderr = child.stderr.take().expect("stderr piped");
        let stderr_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            use std::io::Read;
            stderr.read_to_end(&mut buf).map(|_| buf)
        });

        // Enforce a wall-clock timeout.
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if start.elapsed() > self.timeout {
                let _ = child.kill();
                // Join threads to avoid leak, but we don't care about their results on timeout
                let _ = stdin_handle.join();
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(LintError::Timeout(self.timeout));
            }
            sleep(Duration::from_millis(20));
        };

        // Join threads to get output
        let _ = stdin_handle.join(); // Ignore write error (e.g. broken pipe if process exited early)
        let stdout_bytes = stdout_handle.join().map_err(|_| LintError::Failed(self.id.clone(), "stdout thread panicked".into()))??;
        let stderr_bytes = stderr_handle.join().map_err(|_| LintError::Failed(self.id.clone(), "stderr thread panicked".into()))??;

        let stdout_str = String::from_utf8_lossy(&stdout_bytes);
        match serde_json::from_str::<Vec<Finding>>(stdout_str.trim()) {
            Ok(findings) => Ok(findings),
            Err(e) => {
                // Nonzero exit *without* valid JSON is a surfaced linter error.
                Err(LintError::Failed(
                    self.id.clone(),
                    format!(
                        "exit {}, invalid JSON output ({e}); stderr: {}",
                        status.code().unwrap_or(-1),
                        String::from_utf8_lossy(&stderr_bytes).trim()
                    ),
                ))
            }
        }
    }
}
