//! The interactive TUI event loop (§8.2). Rendering lives in `stacksaw-ui`;
//! this wires crossterm input and terminal setup around it.

use std::io::Stdout;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseButton, MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use stacksaw_ui::app::Mode;
use stacksaw_ui::{command, App, ViewState};

use crate::context::Ctx;

/// Environment variable carrying serialized [`ViewState`] across a dev
/// self-reload (§8.2). Set only on the re-exec'd child, so a fresh manual
/// launch always starts clean.
const STATE_ENV: &str = "STACKSAW_TUI_STATE";

/// Whether the event loop exited to quit or to relaunch a rebuilt binary.
enum Outcome {
    Quit,
    Relaunch,
}

/// Run a UI window until the user quits (or the binary is rebuilt, in which
/// case we transparently re-exec ourselves — see [`ExeWatch`]).
pub fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let repo = ctx.repo()?;
    let snapshot = stacksaw_git::build_snapshot(&repo, 0, &ctx.model_options())?;
    let mut app = App::new(snapshot);
    app.truecolor = detect_truecolor();

    // Restore navigation state handed over by a prior instance, if any.
    let pending_file = restore_state(&mut app);
    let mut watch = ExeWatch::new();

    let mut terminal = setup()?;
    let outcome = event_loop(ctx, &mut terminal, &mut app, &mut watch, pending_file);
    restore(&mut terminal)?;

    match outcome? {
        Outcome::Quit => Ok(()),
        // Terminal is already restored above, so the child inherits a clean tty.
        Outcome::Relaunch => relaunch(&app),
    }
}

/// Parse [`ViewState`] from [`STATE_ENV`] and apply everything except the file
/// selection (which must wait for the Files column to reload). Returns the
/// pending `selected_file` for the host to apply post-load, if present.
fn restore_state(app: &mut App) -> Option<usize> {
    let raw = std::env::var(STATE_ENV).ok()?;
    // Consume it so it doesn't leak into git subprocesses we spawn.
    std::env::remove_var(STATE_ENV);
    let vs: ViewState = serde_json::from_str(&raw).ok()?;
    let stairs = app.snapshot.staircases.len();
    app.focused = vs.focused;
    app.selected_stair = vs.selected_stair.min(stairs.saturating_sub(1));
    app.selected_commit = vs.selected_commit;
    app.zoom = vs.zoom;
    app.checks_open = vs.checks_open;
    Some(vs.selected_file)
}

/// Replace this process with a fresh invocation of the (rebuilt) binary,
/// forwarding the original arguments and the current navigation state. On Unix
/// this `exec`s in place so the PID is preserved; the call only returns on
/// error (which propagates up to `main`).
fn relaunch(app: &App) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let state = serde_json::to_string(&app.view_state())?;
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut cmd = std::process::Command::new(exe);
    cmd.args(&args).env(STATE_ENV, state);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(cmd.exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(0));
    }
}

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Detect 24-bit truecolor support. `COLORTERM=truecolor|24bit` is the de-facto
/// signal (set by iTerm2, kitty, WezTerm, VS Code, modern tmux, …). When it is
/// absent we fall back to 256-color indexed rendering, which is safe on
/// terminals like macOS Terminal.app that silently drop RGB escapes.
fn detect_truecolor() -> bool {
    match std::env::var("COLORTERM") {
        Ok(v) => v.contains("truecolor") || v.contains("24bit"),
        Err(_) => false,
    }
}

fn setup() -> anyhow::Result<Term> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn restore(terminal: &mut Term) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Watches the on-disk executable and reports when it has been rebuilt.
///
/// We track the *path* (via [`std::env::current_exe`]), not the running inode:
/// `cargo install`/`cargo build` atomically rename a new file over it, so the
/// running process keeps its old (unlinked) image while the path points at
/// fresh bytes. A change is only reported once its signature is *stable* across
/// two consecutive polls, so we never re-exec onto a half-written binary.
struct ExeWatch {
    path: Option<PathBuf>,
    /// Signature of the image we're currently running from.
    running: Option<Sig>,
    /// A differing signature seen once; promoted to a reload when it repeats.
    pending: Option<Sig>,
}

/// A cheap file-identity signature: `(inode, mtime, len)`. The inode is the
/// load-bearing field — `cargo install` atomically renames a new file over the
/// path, which always changes the inode, whereas it *preserves* the source
/// mtime, so an mtime/len-only signature can miss a rebuild.
type Sig = (u64, SystemTime, u64);

impl ExeWatch {
    fn new() -> Self {
        let path = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .ok();
        let running = path.as_deref().and_then(sig);
        ExeWatch {
            path,
            running,
            pending: None,
        }
    }

    /// True once a rebuilt binary has settled at our path.
    fn rebuilt(&mut self) -> bool {
        let Some(cur) = self.path.as_deref().and_then(sig) else {
            return false;
        };
        if Some(cur) == self.running {
            self.pending = None;
            false
        } else if self.pending == Some(cur) {
            true
        } else {
            self.pending = Some(cur);
            false
        }
    }
}

/// Read a file's `(inode, mtime, len)` signature, or `None` if it can't be
/// stat'd. On non-Unix the inode is reported as `0` and we fall back to
/// mtime/len alone.
fn sig(path: &std::path::Path) -> Option<Sig> {
    let m = std::fs::metadata(path).ok()?;
    Some((inode_of(&m), m.modified().ok()?, m.len()))
}

#[cfg(unix)]
fn inode_of(m: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.ino()
}

#[cfg(not(unix))]
fn inode_of(_: &std::fs::Metadata) -> u64 {
    0
}

/// How long to block waiting for input each frame before redrawing.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Idle period after which we rebuild the snapshot to pick up external repo
/// changes (§6). Kept well above the snapshot build cost so the refresh never
/// competes with active input; the timer is debounced by user events below.
const IDLE_REFRESH: Duration = Duration::from_millis(3000);

fn event_loop(
    ctx: &Ctx,
    terminal: &mut Term,
    app: &mut App,
    watch: &mut ExeWatch,
    mut pending_file: Option<usize>,
) -> anyhow::Result<Outcome> {
    let mut last_refresh = std::time::Instant::now();
    loop {
        // Populate the Files column for the selected commit (lazily, only when
        // the selection has moved).
        if let Some(oid) = app.files_needing_load() {
            let files = stacksaw_git::changed_files(&ctx.repo_root, &oid).unwrap_or_default();
            app.set_files(oid, files);
            // A relaunch's file selection can only be restored now that the
            // column exists (set_files reset it to the top).
            if let Some(idx) = pending_file.take() {
                app.selected_file = idx.min(app.files.len().saturating_sub(1));
            }
        }
        // Populate the Diff column for the selected file (lazily). Added files
        // show their full content instead of an all-`+` patch.
        if let Some((oid, path)) = app.diff_needing_load() {
            // The pinned "commit message" row shows the full message; added
            // files show raw content; everything else shows a unified patch.
            let (text, raw) = if app.selected_file_is_message() {
                (
                    stacksaw_git::commit_message(&ctx.repo_root, &oid).unwrap_or_default(),
                    true,
                )
            } else if app.selected_file_is_added() {
                (
                    stacksaw_git::file_content(&ctx.repo_root, &oid, &path).unwrap_or_default(),
                    true,
                )
            } else {
                (
                    stacksaw_git::file_diff(&ctx.repo_root, &oid, &path).unwrap_or_default(),
                    false,
                )
            };
            app.set_diff(oid, path, &text, raw);
        }

        terminal.draw(|f| app.draw(f))?;

        if event::poll(POLL_INTERVAL)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        handle_key(app, key);
                    }
                }
                // Mouse only drives the normal scene, not the overlays.
                Event::Mouse(m) if app.mode() == Mode::Normal => match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => app.on_click(m.column, m.row),
                    MouseEventKind::ScrollDown => app.on_scroll(m.column, m.row, true),
                    MouseEventKind::ScrollUp => app.on_scroll(m.column, m.row, false),
                    _ => {}
                },
                _ => {}
            }
            // Debounce the periodic refresh: any interaction defers the next
            // rebuild so a snapshot build never stutters active navigation.
            last_refresh = std::time::Instant::now();
        }

        if app.should_quit {
            return Ok(Outcome::Quit);
        }

        // Transparently re-exec when our binary is rebuilt (§8.2 dev reload).
        if watch.rebuilt() {
            return Ok(Outcome::Relaunch);
        }

        // Refresh from the repo periodically so external changes appear (§6).
        if last_refresh.elapsed() > IDLE_REFRESH {
            if let Ok(repo) = ctx.repo() {
                if let Ok(snap) = stacksaw_git::build_snapshot(&repo, 0, &ctx.model_options()) {
                    let (stair, commit) = (app.selected_stair, app.selected_commit);
                    app.snapshot = snap;
                    app.selected_stair = stair.min(app.snapshot.staircases.len().saturating_sub(1));
                    app.selected_commit = commit;
                }
            }
            last_refresh = std::time::Instant::now();
        }
    }
}

/// Route a key press by mode: normal keys resolve through the command registry
/// (§8.2); the help/palette overlays capture input until dismissed.
fn handle_key(app: &mut App, key: KeyEvent) {
    match app.mode() {
        Mode::Normal => {
            if let Some(action) = command::lookup(&key, app.focused) {
                app.apply(action);
            }
        }
        // Help is a read-only overlay: any key closes it.
        Mode::Help => app.close_overlay(),
        Mode::Palette => match key.code {
            KeyCode::Esc => app.close_overlay(),
            KeyCode::Enter => {
                if let Some(action) = app.palette_confirm() {
                    app.apply(action);
                }
            }
            KeyCode::Up => app.palette_move(false),
            KeyCode::Down => app.palette_move(true),
            KeyCode::Backspace => app.palette_backspace(),
            KeyCode::Char(c) => app.palette_input(c),
            _ => {}
        },
    }
}
