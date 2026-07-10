use crate::error::{GitError, Result};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread;

/// A builder for git commands that centralizes common arguments and error handling.
#[derive(Debug, Clone)]
pub struct GitExecutor {
    workdir: PathBuf,
    args: Vec<String>,
    inert: bool,
    quiet: bool,
    env: Vec<(String, String)>,
}

impl GitExecutor {
    /// Create a new git executor for the given repository path.
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
            args: Vec::new(),
            inert: false,
            quiet: false,
            env: Vec::new(),
        }
    }

    /// Add a single argument to the git command.
    pub fn arg(mut self, arg: impl AsRef<str>) -> Self {
        self.args.push(arg.as_ref().to_string());
        self
    }

    /// Add multiple arguments to the git command.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.args
            .extend(args.into_iter().map(|s| s.as_ref().to_string()));
        self
    }

    /// Apply standard "inert" flags to prevent hooks, rerere, and GPG signing.
    pub fn inert(mut self) -> Self {
        self.inert = true;
        self
    }

    /// Silence stdout and stderr.
    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// Add an environment variable to the git command.
    pub fn env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.env.push((key.into(), val.into()));
        self
    }

    /// Build the underlying `std::process::Command`.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.workdir);
        if self.inert {
            cmd.args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "rerere.enabled=false",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "advice.mergeConflict=false",
            ]);
        }
        if self.quiet {
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.args(&self.args);
        cmd
    }

    /// Execute the command and return its output.
    pub fn output(&self) -> Result<Output> {
        self.command().output().map_err(GitError::Io)
    }

    /// Execute the command and return its exit status.
    pub fn status(&self) -> Result<ExitStatus> {
        self.command().status().map_err(GitError::Io)
    }

    /// Execute the command with provided stdin data and return its output.
    /// Spawns a thread to write to stdin to prevent deadlocks.
    pub fn output_with_stdin(&self, stdin_data: Vec<u8>) -> Result<Output> {
        let mut child = self
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            thread::spawn(move || {
                let _ = stdin.write_all(&stdin_data);
            });
        }

        child.wait_with_output().map_err(GitError::Io)
    }

    /// Execute the command and return the stdout as a string if it succeeded.
    /// Returns `GitError::Command` if the exit status is non-zero.
    pub fn run_captured(&self) -> Result<String> {
        let out = self.output()?;
        self.handle_output(out)
    }

    /// Like `run_captured` but with stdin.
    pub fn run_captured_with_stdin(&self, stdin_data: Vec<u8>) -> Result<String> {
        let out = self.output_with_stdin(stdin_data)?;
        self.handle_output(out)
    }

    fn handle_output(&self, out: Output) -> Result<String> {
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(GitError::Command {
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            })
        }
    }

    /// Execute the command and return whether it succeeded.
    pub fn success(&self) -> Result<bool> {
        Ok(self.status()?.success())
    }
}
