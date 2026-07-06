//! Host-side, context-aware command execution for the tabbed viewport.
//!
//! Commands launched from the `>` prompt run under a PTY (so full ANSI/VT
//! output renders faithfully in the embedded emulator). Output is streamed off
//! a reader thread into the UI; input, resizes, cancels, and teardown flow the
//! other way. A command runs in the current worktree when it targets the
//! checked-out HEAD (or the worktree row); any other commit is checked out into
//! an ephemeral detached worktree, ref-counted by oid and auto-removed when the
//! last tab using it closes (§9.3).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use stacksaw_ssp::types::WORKTREE_OID;
use stacksaw_ui::viewport::RunContext;
use stacksaw_ui::{App, ExecTarget, PendingRun};

use crate::context::Ctx;

/// A message from a command's reader thread.
enum RunEvent {
    Bytes(u64, Vec<u8>),
}

/// The live resources backing one command terminal.
struct RunHandle {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// The ephemeral-worktree oid this run holds a reference to, if any.
    worktree_oid: Option<String>,
}

/// Owns every running command's PTY plus the pool of ephemeral worktrees.
pub struct RunManager {
    handles: HashMap<u64, RunHandle>,
    /// Ephemeral detached worktrees keyed by commit oid, ref-counted so several
    /// tabs against the same commit share one checkout.
    worktrees: HashMap<String, (PathBuf, usize)>,
    tx: Sender<RunEvent>,
    rx: Receiver<RunEvent>,
    next_id: u64,
    repo_root: PathBuf,
    scratch_root: PathBuf,
    shell: String,
}

impl RunManager {
    pub fn new(ctx: &Ctx) -> Self {
        let (tx, rx) = channel();
        RunManager {
            handles: HashMap::new(),
            worktrees: HashMap::new(),
            tx,
            rx,
            next_id: 1,
            repo_root: ctx.repo_root.clone(),
            scratch_root: ctx.git_dir.join("stacksaw").join("run-worktrees"),
            shell: detect_shell(),
        }
    }

    /// Whether any command process is still tracked (drives a tighter poll).
    pub fn is_busy(&self) -> bool {
        !self.handles.is_empty()
    }

    /// Drive one loop tick: spawn queued commands, stream output, forward input,
    /// apply resizes, and handle cancel/close. Returns `true` if anything
    /// changed that warrants a redraw.
    pub fn tick(&mut self, ctx: &Ctx, app: &mut App) -> bool {
        let mut changed = false;
        for run in app.take_pending_runs() {
            self.launch(ctx, app, run);
            changed = true;
        }
        changed |= self.pump(app);
        self.forward_input(app);
        self.apply_resizes(app);
        changed |= self.lifecycle(app);
        changed
    }

    /// Drain streamed bytes into the UI and reap any exited children.
    fn pump(&mut self, app: &mut App) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                RunEvent::Bytes(id, bytes) => {
                    app.push_pty_output(id, &bytes);
                    changed = true;
                }
            }
        }
        let mut done: Vec<(u64, i32)> = Vec::new();
        for (id, handle) in self.handles.iter_mut() {
            if let Ok(Some(status)) = handle.child.try_wait() {
                done.push((*id, status.exit_code() as i32));
            }
        }
        for (id, code) in done {
            app.finish_run(id, code);
            if let Some(handle) = self.handles.remove(&id) {
                if let Some(oid) = handle.worktree_oid {
                    self.release_worktree(&oid);
                }
            }
            changed = true;
        }
        changed
    }

    fn forward_input(&mut self, app: &mut App) {
        for (id, bytes) in app.take_pty_input() {
            if let Some(handle) = self.handles.get_mut(&id) {
                let _ = handle.writer.write_all(&bytes);
                let _ = handle.writer.flush();
            }
        }
    }

    fn apply_resizes(&mut self, app: &mut App) {
        for (id, rows, cols) in app.sync_run_sizes() {
            if let Some(handle) = self.handles.get(&id) {
                let _ = handle.master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }
    }

    fn lifecycle(&mut self, app: &mut App) -> bool {
        let mut changed = false;
        // Cancel: send Ctrl-C down the PTY, so the shell delivers SIGINT to the
        // foreground process group (matching what a user would type).
        for id in app.take_runs_to_cancel() {
            if let Some(handle) = self.handles.get_mut(&id) {
                let _ = handle.writer.write_all(&[0x03]);
                let _ = handle.writer.flush();
                changed = true;
            }
        }
        // Close: the tab is gone — kill the process and reclaim its worktree.
        for id in app.take_runs_to_close() {
            if let Some(mut handle) = self.handles.remove(&id) {
                let _ = handle.child.kill();
                if let Some(oid) = handle.worktree_oid {
                    self.release_worktree(&oid);
                }
                changed = true;
            }
        }
        changed
    }

    /// Spawn one command, opening its tab in the UI.
    fn launch(&mut self, ctx: &Ctx, app: &mut App, run: PendingRun) {
        let (cwd, worktree_oid) = match self.resolve(ctx, &run.target) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("run context resolution failed: {e:#}");
                return;
            }
        };
        let (cols, rows) = app.viewport_content_size();
        let size = PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = match native_pty_system().openpty(size) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("openpty failed: {e:#}");
                if let Some(oid) = worktree_oid {
                    self.release_worktree(&oid);
                }
                return;
            }
        };
        let mut cmd = CommandBuilder::new(&self.shell);
        cmd.arg("-c");
        cmd.arg(&run.command);
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");
        let child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("spawn failed: {e:#}");
                if let Some(oid) = worktree_oid {
                    self.release_worktree(&oid);
                }
                return;
            }
        };
        // Drop the slave so the master sees EOF when the child exits.
        drop(pair.slave);
        let reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("pty reader failed: {e:#}");
                return;
            }
        };
        let writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("pty writer failed: {e:#}");
                return;
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(RunEvent::Bytes(id, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        self.handles.insert(
            id,
            RunHandle {
                master: pair.master,
                writer,
                child,
                worktree_oid,
            },
        );
        app.open_run(
            id,
            run.command.clone(),
            run.target.label.clone(),
            run.target.oid.clone(),
            run_context(ctx),
            rows,
            cols,
        );
    }

    /// Resolve `(working directory, ephemeral-worktree oid)` for a target: the
    /// current worktree for HEAD / the worktree row, else an ephemeral detached
    /// worktree at the requested commit, entered at the monorepo sub-path.
    fn resolve(&mut self, ctx: &Ctx, target: &ExecTarget) -> anyhow::Result<(PathBuf, Option<String>)> {
        match &target.oid {
            None => Ok((ctx.context_dir.clone(), None)),
            Some(oid) if oid == WORKTREE_OID => Ok((ctx.context_dir.clone(), None)),
            Some(oid) if self.head_matches(ctx, oid) => Ok((ctx.context_dir.clone(), None)),
            Some(oid) => {
                let path = self.acquire_worktree(oid)?;
                Ok((path.join(ctx.rel_subdir()), Some(oid.clone())))
            }
        }
    }

    fn head_matches(&self, ctx: &Ctx, oid: &str) -> bool {
        ctx.repo()
            .ok()
            .and_then(|r| r.head_oid().ok().flatten())
            .map(|o| o.to_string() == oid)
            .unwrap_or(false)
    }

    /// Get (or create) the ephemeral worktree for `oid`, bumping its refcount.
    fn acquire_worktree(&mut self, oid: &str) -> anyhow::Result<PathBuf> {
        if let Some((path, count)) = self.worktrees.get_mut(oid) {
            *count += 1;
            return Ok(path.clone());
        }
        std::fs::create_dir_all(&self.scratch_root)?;
        let short: String = oid.chars().take(12).collect();
        let dest = self.scratch_root.join(short);
        let path = stacksaw_git::refs::add_scratch_worktree(&self.repo_root, oid, &dest)
            .map_err(|e| anyhow::anyhow!("worktree add failed: {e}"))?;
        self.worktrees.insert(oid.to_string(), (path.clone(), 1));
        Ok(path)
    }

    /// Drop a reference to an ephemeral worktree, removing it at zero.
    fn release_worktree(&mut self, oid: &str) {
        if let Some((path, count)) = self.worktrees.get_mut(oid) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                let path = path.clone();
                self.worktrees.remove(oid);
                if let Err(e) = stacksaw_git::refs::remove_worktree(&self.repo_root, &path) {
                    tracing::warn!("worktree remove failed: {e}");
                }
            }
        }
    }
}

impl Drop for RunManager {
    fn drop(&mut self) {
        for (_, mut handle) in self.handles.drain() {
            let _ = handle.child.kill();
        }
        for (_, (path, _)) in self.worktrees.drain() {
            let _ = stacksaw_git::refs::remove_worktree(&self.repo_root, &path);
        }
    }
}

/// The shell that launched stacksaw (so a zsh user's command runs under zsh),
/// falling back to `/bin/sh`.
pub fn detect_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Display-ready repo/git context for a run tab's header: the repo root and the
/// git directory, with `$HOME` abbreviated to `~` and the git dir shown relative
/// to the root when nested under it (the common `.git`).
fn run_context(ctx: &Ctx) -> RunContext {
    let git_dir = match ctx.git_dir.strip_prefix(&ctx.repo_root) {
        Ok(rel) => rel.display().to_string(),
        Err(_) => tildify(&ctx.git_dir),
    };
    RunContext {
        repo_root: tildify(&ctx.repo_root),
        git_dir,
    }
}

/// Abbreviate a path's `$HOME` prefix to `~`.
fn tildify(path: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Ok(rel) = path.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

/// Load the user's shell command history (most-recent first, de-duplicated) to
/// power the `>` launcher's autocomplete. Best-effort: an unreadable or unknown
/// history file yields an empty list.
pub fn load_command_history() -> Vec<String> {
    let shell = detect_shell();
    let is_zsh = shell.contains("zsh");
    let path = history_path(is_zsh);
    let Some(path) = path else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read(&path) else {
        return Vec::new();
    };
    // Histories can hold non-UTF-8 bytes (zsh metafied); decode lossily.
    let text = String::from_utf8_lossy(&raw);
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Walk newest-first so the de-dup keeps the most recent occurrence.
    for line in text.lines().rev() {
        let cmd = parse_history_line(line);
        let Some(cmd) = cmd else { continue };
        if cmd.is_empty() {
            continue;
        }
        if seen.insert(cmd.clone()) {
            out.push(cmd);
        }
        if out.len() >= 2000 {
            break;
        }
    }
    out
}

fn history_path(is_zsh: bool) -> Option<PathBuf> {
    if let Ok(hf) = std::env::var("HISTFILE") {
        if !hf.is_empty() {
            return Some(PathBuf::from(hf));
        }
    }
    let home = std::env::var("HOME").ok()?;
    let name = if is_zsh { ".zsh_history" } else { ".bash_history" };
    Some(PathBuf::from(home).join(name))
}

/// Extract the command from a history line, stripping zsh's extended-history
/// metadata (`: <ts>:<dur>;<command>`).
fn parse_history_line(line: &str) -> Option<String> {
    let line = line.trim_end();
    if let Some(rest) = line.strip_prefix(": ") {
        // `<ts>:<dur>;<command>` — take everything after the first ';'.
        if let Some(idx) = rest.find(';') {
            return Some(rest[idx + 1..].to_string());
        }
    }
    Some(line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zsh_extended_history() {
        assert_eq!(
            parse_history_line(": 1700000000:0;cargo test").as_deref(),
            Some("cargo test")
        );
    }

    #[test]
    fn parses_plain_history() {
        assert_eq!(parse_history_line("ls -la").as_deref(), Some("ls -la"));
    }
}
