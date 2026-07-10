//! The interactive TUI event loop (§8.2). Rendering lives in `stacksaw-ui`;
//! this wires crossterm input and terminal setup around it.

use std::env;
use std::ffi::OsString;
use std::fs::{self, Metadata};
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use stacksaw_core::recent::{self, RecentStore};
use stacksaw_ui::app::Mode;
use stacksaw_ui::{
    command, App, ColumnKind, GlyphSet, HoverThrottle, LayoutPrefs, RecentRowView, RecentsView,
    RedrawGate, ViewState, HOVER_MAX_WAIT_MS, HOVER_SETTLE_MS, REDRAW_MIN_INTERVAL_MS,
};

use stacksaw_git::{archive, reshape};

use stacksaw_ssp::types::RebaseStatus;

use crate::context::Ctx;
use crate::rebase_prober::{ProbeKey, RebaseProber};
use crate::runner::{load_command_history, RunManager};

/// Environment variable carrying serialized [`ViewState`] across a dev
/// self-reload (§8.2). Set only on the re-exec'd child, so a fresh manual
/// launch always starts clean.
const STATE_ENV: &str = "STACKSAW_TUI_STATE";

/// Why the event loop exited: quit, relaunch the rebuilt binary in place, or
/// switch this window to another repo (re-exec with that repo as the workdir).
enum Outcome {
    Quit,
    Relaunch,
    SwitchRepo(PathBuf),
}

/// How a completed UI session should end at the process level.
enum Session {
    Quit,
    /// The binary was rebuilt: re-exec ourselves carrying this serialized
    /// [`ViewState`] (see [`ExeWatch`]). This is the only path that re-execs;
    /// repo switches happen in place, keeping the terminal (no flicker).
    Relaunch(String),
}

/// Run a UI window until the user quits. Switching to a recent repo rebuilds the
/// scene in place (the terminal stays in the alternate screen — no re-exec, no
/// blink); only a rebuilt binary re-execs, transparently carrying nav state.
pub fn run(ctx: Ctx, upstream_override: Option<String>) -> anyhow::Result<()> {
    let mut terminal = setup()?;
    let result = run_session(&mut terminal, ctx, upstream_override);
    // Best-effort restore regardless of how the session ended, so an error
    // never leaves the terminal stuck in the alternate screen.
    let _ = restore(&mut terminal);
    match result? {
        Session::Quit => Ok(()),
        Session::Relaunch(state) => relaunch(state),
    }
}

/// Drive one or more repo scenes over a single live terminal. Each `SwitchRepo`
/// rebuilds the context/app for the target repo and loops without tearing the
/// terminal down.
fn run_session(
    terminal: &mut Term,
    mut ctx: Ctx,
    upstream_override: Option<String>,
) -> anyhow::Result<Session> {
    // Nav state handed over by a prior *process* (a self-reload) applies only to
    // the first repo we show; consume it so it can't leak into git subprocesses.
    let mut pending_state = env::var(STATE_ENV).ok();
    env::remove_var(STATE_ENV);
    let mut watch = ExeWatch::new();
    // Set once we've switched at least once, so we can preserve the user's
    // context (they were driving the Stacks column when they picked a repo)
    // rather than dropping them into the default Commits focus.
    let mut switched = false;

    loop {
        let repo = ctx.repo()?;
        let snapshot = stacksaw_git::build_snapshot(&repo, 0, &ctx.model_options())?;
        let mut app = App::new(snapshot);
        app.truecolor = detect_truecolor();
        app.set_glyph_set(GlyphSet::parse(&ctx.config.ui.glyphs));
        app.set_layout_prefs(load_layout());
        if switched {
            app.focused = ColumnKind::Stacks;
        }
        let pending_file = pending_state
            .take()
            .and_then(|raw| apply_state(&mut app, &raw));
        // Record this repo in the MRU and hand the recents ledger to the UI.
        let recents = init_recents(&ctx);
        app.set_recents(recents_view(&recents));

        match event_loop(&ctx, terminal, &mut app, &mut watch, &recents, pending_file)? {
            Outcome::Quit => return Ok(Session::Quit),
            Outcome::Relaunch => {
                return Ok(Session::Relaunch(serde_json::to_string(&app.view_state())?));
            }
            Outcome::SwitchRepo(dir) => {
                // Rebuild the scene for the target repo on the next iteration.
                // A bad target (rare — MRU rows are real dirs) is ignored so the
                // window stays put rather than tearing down.
                match Ctx::open_at(&dir, upstream_override.clone()) {
                    Ok(next) => {
                        ctx = next;
                        switched = true;
                    }
                    Err(e) => tracing::warn!("switch to {} failed: {e:#}", dir.display()),
                }
            }
        }
    }
}

/// Parse [`ViewState`] from `raw` and apply everything except the file
/// selection (which must wait for the Files column to reload). Returns the
/// pending `selected_file` for the host to apply post-load, if present.
fn apply_state(app: &mut App, raw: &str) -> Option<usize> {
    let vs: ViewState = serde_json::from_str(raw).ok()?;
    let stairs = app.snapshot.staircases.len();
    app.focused = vs.focused;
    app.selected_stair = vs.selected_stair.min(stairs.saturating_sub(1));
    app.selected_commit = vs.selected_commit;
    app.zoom = vs.zoom;
    app.checks_open = vs.checks_open;
    // A reload may carry an in-progress resize; let it win over the on-disk copy.
    app.set_layout_prefs(vs.layout);
    Some(vs.selected_file)
}

/// Where the dragged divider layout is persisted (`<data_dir>/layout.json`), a
/// global per-user UI preference alongside the recents MRU.
fn layout_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "stacksaw").map(|d| d.data_dir().join("layout.json"))
}

/// Load the persisted divider layout, or the automatic layout if none is saved.
fn load_layout() -> LayoutPrefs {
    layout_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the divider layout after a drag ends. Best-effort: a failure to
/// write just means the resize won't survive the next launch.
fn save_layout(prefs: &LayoutPrefs) {
    let Some(path) = layout_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(prefs) {
        let _ = fs::write(path, json);
    }
}

/// The stable inputs for the recents ledger, resolved once per session: the
/// current repo and the MRU repos with their detected monorepo roots. Only the
/// per-repo checked-out branch changes while we run, so this is fixed and the
/// [`recents_view`] re-reads just the branches on each refresh.
struct RecentsSource {
    current: PathBuf,
    repos: Vec<(PathBuf, Option<PathBuf>)>,
}

/// Record this repo in the persisted MRU and resolve the stable recents inputs:
/// detect each repo's monorepo root using the configured markers. Branch names
/// are *not* read here — [`recents_view`] does that, cheaply, on every tick.
fn init_recents(ctx: &Ctx) -> RecentsSource {
    let mut store = RecentStore::load();
    store.record(&ctx.repo_root);
    let _ = store.save();

    let markers: Vec<&str> = ctx
        .config
        .monorepo
        .markers
        .iter()
        .map(String::as_str)
        .collect();
    let current = fs::canonicalize(&ctx.repo_root).unwrap_or_else(|_| ctx.repo_root.clone());
    let repos = store
        .repos
        .iter()
        .map(|r| {
            (
                r.path.clone(),
                recent::detect_monorepo_root(&r.path, &markers),
            )
        })
        .collect();
    RecentsSource { current, repos }
}

/// Build the flat, recency-ordered recents ledger for the Stacks column: label
/// each repo relative to its monorepo root and read its currently checked-out
/// branch straight from `.git/HEAD`. Labels are left un-elided — the renderer
/// trims them to the live column width. Cheap enough to call every refresh, so
/// branches stay in sync with checkouts made elsewhere (§6) without watchers.
fn recents_view(src: &RecentsSource) -> RecentsView {
    let rows = recent::flatten_recents(&src.current, &src.repos);
    RecentsView {
        rows: rows
            .into_iter()
            .map(|e| RecentRowView {
                parent: e.parent,
                label: e.label,
                branch: recent::current_branch(&e.path),
                current: e.current,
                path: e.path,
            })
            .collect(),
    }
}

/// Replace this process with a fresh invocation of the (rebuilt) binary,
/// forwarding the original arguments and handing over the serialized navigation
/// `state` via [`STATE_ENV`]. On Unix this `exec`s in place so the PID is
/// preserved; the call only returns on error (which propagates up to `main`).
fn relaunch(state: String) -> anyhow::Result<()> {
    let exe = current_exe_path()?;
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let mut cmd = Command::new(exe);
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

/// The path to re-exec on dev reload. On Linux `current_exe` resolves
/// `/proc/self/exe`, which the kernel reports with a trailing ` (deleted)`
/// once the running binary's file has been replaced (exactly what a rebuild
/// via `cargo install` does). Strip that marker and prefer the real path so
/// the reload picks up the freshly installed binary rather than failing to
/// spawn a nonexistent one.
fn current_exe_path() -> anyhow::Result<PathBuf> {
    let exe = env::current_exe()?;
    if !exe.exists() {
        if let Some(stripped) = exe
            .to_str()
            .and_then(|s| s.strip_suffix(" (deleted)"))
            .map(PathBuf::from)
        {
            if stripped.exists() {
                return Ok(stripped);
            }
        }
    }
    Ok(exe)
}

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Detect 24-bit truecolor support. `COLORTERM=truecolor|24bit` is the de-facto
/// signal (set by iTerm2, kitty, WezTerm, VS Code, modern tmux, …). When it is
/// absent we fall back to 256-color indexed rendering, which is safe on
/// terminals like macOS Terminal.app that silently drop RGB escapes.
fn detect_truecolor() -> bool {
    match env::var("COLORTERM") {
        Ok(v) => v.contains("truecolor") || v.contains("24bit"),
        Err(_) => false,
    }
}

fn setup() -> anyhow::Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    // `EnableMouseCapture` turns on any-event tracking (DEC mode 1003) as well
    // as button/drag, so we receive `MouseEventKind::Moved` for pointer motion
    // — which drives the divider and row hover affordances (a terminal can't
    // change the OS cursor shape, so we light up the target instead).
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
        let path = env::current_exe().and_then(fs::canonicalize).ok();
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
fn sig(path: &Path) -> Option<Sig> {
    let m = fs::metadata(path).ok()?;
    Some((inode_of(&m), m.modified().ok()?, m.len()))
}

#[cfg(unix)]
fn inode_of(m: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.ino()
}

#[cfg(not(unix))]
fn inode_of(_: &Metadata) -> u64 {
    0
}

/// How long to block waiting for input each frame before redrawing.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Idle period after which we rebuild the snapshot to pick up external repo
/// changes (§6). Kept well above the snapshot build cost so the refresh never
/// competes with active input; the timer is debounced by user events below.
const IDLE_REFRESH: Duration = Duration::from_millis(3000);

/// Re-derive each behind staircase's rebase-onto-upstream verdict from the
/// prober's cache (enqueuing a background probe on a miss). Cheap: it only
/// resolves a few oids per behind stack and reads the cache — the actual rebase
/// runs on the worker thread. In-sync stacks are cleared to `Unknown`.
fn apply_rebase_verdicts(ctx: &Ctx, app: &mut App, prober: Option<&mut RebaseProber>) {
    let (Some(prober), Ok(repo)) = (prober, ctx.repo()) else {
        return;
    };
    for s in &mut app.snapshot.staircases {
        // A dangling child (amended parent) needs a *restack* onto the parent's
        // new tip; that takes priority over rebasing the whole stack upstream.
        // Otherwise a behind stack is probed for a rebase onto its upstream.
        let oids = if s.segments.iter().any(|seg| seg.stale) {
            stacksaw_git::restack_probe_oids(s)
        } else if s.behind > 0 {
            stacksaw_git::rebase_probe_oids(&repo, s)
        } else {
            None
        };
        match oids {
            Some((onto, base, tip)) => {
                let v = prober.verdict(ProbeKey { onto, base, tip });
                s.rebase = v.status;
                s.conflict = v.conflict;
            }
            None => {
                s.rebase = RebaseStatus::Unknown;
                s.conflict = None;
            }
        }
    }
}

fn event_loop(
    ctx: &Ctx,
    terminal: &mut Term,
    app: &mut App,
    watch: &mut ExeWatch,
    recents: &RecentsSource,
    mut pending_file: Option<usize>,
) -> anyhow::Result<Outcome> {
    let mut last_refresh = Instant::now();
    // Redraw is the expensive step (tens of ms over a remote tmux/ssh link), so
    // we only re-render when something visible actually changed. Idle pointer
    // motion within the same row must never trigger one.
    let mut needs_redraw = true;
    // And even genuine changes are capped to a frame budget: a rapid mouse sweep
    // that moves the hover highlight across many rows coalesces in time into a
    // handful of frames instead of one flush per row crossed.
    let epoch = Instant::now();
    let mut redraw_gate = RedrawGate::new(REDRAW_MIN_INTERVAL_MS);
    // Hover is debounced separately: a change to the highlighted row/divider is
    // held until motion settles (or a coarse max-wait), so a fast drag paints
    // the final row rather than trailing through every row it crossed.
    let mut hover = HoverThrottle::new(HOVER_SETTLE_MS, HOVER_MAX_WAIT_MS);
    // Context-aware command runner: owns each command terminal's PTY and any
    // ephemeral worktrees. Dropped (killing children, reclaiming worktrees) when
    // this session ends — including on a repo switch.
    let mut runs = RunManager::new(ctx);
    app.set_command_history(load_command_history());
    // LIFO stack of reshape inverses (§4 undo): each indent/unindent pushes the
    // transaction that reverses it, popped by the `Undo` action.
    let mut reshape_undo: Vec<reshape::Undo> = Vec::new();
    // Background rebase probing: verdicts are computed off-thread and cached by
    // oids, so a behind stack's "rebase" chip fills in without stalling the loop
    // and never re-probes until its oids change.
    let mut prober = ctx
        .repo()
        .ok()
        .and_then(|r| r.workdir().map(|w| (w, r.common_dir())))
        .map(|(w, c)| RebaseProber::new(w, c));
    apply_rebase_verdicts(ctx, app, prober.as_mut());
    loop {
        // Fold in any finished background probes; a new verdict changes a chip.
        if prober.as_mut().is_some_and(|p| p.drain()) {
            apply_rebase_verdicts(ctx, app, prober.as_mut());
            needs_redraw = true;
        }
        // Populate the Files column for the selected commit (lazily, only when
        // the selection has moved). A load changes the scene.
        if let Some(oid) = app.files_needing_load() {
            let files = stacksaw_git::changed_files(&ctx.repo_root, &oid).unwrap_or_default();
            app.set_files(oid, files);
            // A relaunch's file selection can only be restored now that the
            // column exists (set_files reset it to the top).
            if let Some(idx) = pending_file.take() {
                app.selected_file = idx.min(app.files.len().saturating_sub(1));
            }
            needs_redraw = true;
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
            needs_redraw = true;
        }

        // Service the command runner: spawn queued commands, stream PTY output
        // into the terminals, forward input/resizes, and reap exits. Any change
        // (new bytes, an exit) triggers a redraw.
        if runs.tick(ctx, app) {
            needs_redraw = true;
        }
        // Leave capture mode automatically if the captured terminal has exited.
        app.refresh_capture();

        // Draw when a change is due — immediate changes right away, hover
        // changes once they settle — subject to the frame budget. A draw paints
        // the current hover state, so it clears the hover debt too.
        let now_ms = || epoch.elapsed().as_millis() as u64;
        if (needs_redraw || hover.due(now_ms())) && redraw_gate.ready(now_ms()) {
            terminal.draw(|f| app.draw(f))?;
            needs_redraw = false;
            hover.drawn(now_ms());
        }

        // Block for the first event, then drain the rest of the queue without
        // blocking. When a redraw is owed but withheld, wake exactly when it's
        // next allowed so the coalesced frame still lands promptly.
        let poll_timeout = if needs_redraw {
            Duration::from_millis(redraw_gate.wait_ms(now_ms()).max(1))
        } else if let Some(wait) = hover.next_due_in(now_ms()) {
            Duration::from_millis(wait.max(redraw_gate.wait_ms(now_ms())).max(1))
        } else {
            POLL_INTERVAL
        };
        // While commands stream, wake often so their output drains promptly even
        // absent user input (crossterm's poll won't wake on the PTY channel).
        let poll_timeout = if runs.is_busy() {
            poll_timeout.min(Duration::from_millis(30))
        } else {
            poll_timeout
        };
        let mut has_event = event::poll(poll_timeout)?;
        while has_event {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(app, key);
                    needs_redraw = true;
                }
                // A terminal resize invalidates the whole rendered frame.
                Event::Resize(_, _) => needs_redraw = true,
                // Mouse only drives the normal scene, not the overlays.
                Event::Mouse(m) if app.mode() == Mode::Normal => match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        app.on_click(m.column, m.row);
                        needs_redraw = true;
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        app.on_drag(m.column, m.row);
                        needs_redraw = true;
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        app.on_mouse_up();
                        save_layout(&app.layout_prefs());
                        needs_redraw = true;
                    }
                    // Bare pointer motion. crossterm reports it as `Moved`, but
                    // some terminals (e.g. Ghostty) encode no-button motion as a
                    // right/middle-button "drag", so treat those as motion too —
                    // otherwise the hover affordance never fires there. A hover
                    // change is debounced (not an immediate redraw) so a fast
                    // drag doesn't trail through every row it crosses.
                    MouseEventKind::Moved
                    | MouseEventKind::Drag(MouseButton::Right)
                    | MouseEventKind::Drag(MouseButton::Middle)
                        if app.on_mouse_move(m.column, m.row) =>
                    {
                        hover.touched(now_ms());
                    }
                    MouseEventKind::ScrollDown => {
                        app.on_scroll(m.column, m.row, true);
                        needs_redraw = true;
                    }
                    MouseEventKind::ScrollUp => {
                        app.on_scroll(m.column, m.row, false);
                        needs_redraw = true;
                    }
                    _ => {}
                },
                _ => {}
            }
            // Debounce the periodic refresh: any interaction defers the next
            // rebuild so a snapshot build never stutters active navigation.
            last_refresh = Instant::now();

            if app.should_quit || app.pending_switch.is_some() {
                break;
            }

            // Drain any events already queued without blocking.
            has_event = event::poll(Duration::ZERO)?;
        }

        // Apply any queued reshape (indent/unindent) or undo, then rebuild the
        // snapshot so the new branch layout shows immediately (§4, P4).
        if apply_reshape(ctx, app, &mut reshape_undo) {
            apply_rebase_verdicts(ctx, app, prober.as_mut());
            needs_redraw = true;
            last_refresh = Instant::now();
        }

        if app.should_quit {
            return Ok(Outcome::Quit);
        }

        // A recent-repo row was activated: switch this window to it.
        if let Some(dir) = app.pending_switch.take() {
            return Ok(Outcome::SwitchRepo(dir));
        }

        // Transparently re-exec when our binary is rebuilt (§8.2 dev reload).
        if watch.rebuilt() {
            return Ok(Outcome::Relaunch);
        }

        // Refresh from the repo periodically so external changes appear (§6).
        if last_refresh.elapsed() > IDLE_REFRESH {
            if let Ok(repo) = ctx.repo() {
                if let Ok(snap) = stacksaw_git::build_snapshot(&repo, 0, &ctx.model_options()) {
                    app.snapshot = snap;
                    app.reconcile_selection();
                    apply_rebase_verdicts(ctx, app, prober.as_mut());
                }
            }
            // Re-read the recents' checked-out branches so the ledger tracks
            // checkouts made in other repos (cheap: one HEAD read each).
            app.set_recents(recents_view(recents));
            last_refresh = Instant::now();
            // External state may have moved; reflect it on the next frame.
            needs_redraw = true;
        }
    }
}

/// Drain a queued reshape (indent/unindent), archive, or undo into real ref
/// moves and refresh the snapshot. Returns true when refs changed (so the caller
/// redraws). Failures (forked stack, HEAD off the tip, no upstream, HEAD on an
/// archived branch) are swallowed: nothing moves.
fn apply_reshape(ctx: &Ctx, app: &mut App, undo_stack: &mut Vec<reshape::Undo>) -> bool {
    use stacksaw_ui::ReshapeOp;

    let mut changed = false;
    if let Some(req) = app.take_pending_reshape() {
        if let Ok(repo) = ctx.repo() {
            let op = match req.op {
                ReshapeOp::Indent => reshape::Op::Indent,
                ReshapeOp::Unindent => reshape::Op::Unindent,
            };
            if let Ok(Some(undo)) = reshape::apply(&repo, &ctx.model_options(), &req.oid, op) {
                undo_stack.push(undo);
                changed = true;
            }
        }
    }
    // Archiving parks a stack's branches out of `refs/heads/`; its inverse joins
    // the same LIFO undo stack, so `u` restores an archive too.
    if let Some(branches) = app.take_pending_archive() {
        if let Ok(repo) = ctx.repo() {
            if let Ok(Some(undo)) = archive::archive(&repo, &ctx.model_options(), &branches) {
                undo_stack.push(undo);
                changed = true;
            }
        }
    }
    if app.take_pending_undo() {
        if let Some(undo) = undo_stack.pop() {
            if let Ok(repo) = ctx.repo() {
                if reshape::undo(&repo, &undo).is_ok() {
                    changed = true;
                }
            }
        }
    }

    if changed {
        if let Ok(repo) = ctx.repo() {
            if let Ok(snap) = stacksaw_git::build_snapshot(&repo, 0, &ctx.model_options()) {
                app.snapshot = snap;
                app.reconcile_selection();
            }
        }
    }
    changed
}

/// Route a key press by mode: normal keys resolve through the command registry
/// (§8.2); the help/palette overlays capture input until dismissed.
fn handle_key(app: &mut App, key: KeyEvent) {
    match app.mode() {
        Mode::Normal => {
            if let Some(action) = command::lookup(&key, app.focus()) {
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
        // The `>` command launcher: type a command, with history + inline ghost.
        Mode::Run => match key.code {
            KeyCode::Esc => app.close_overlay(),
            KeyCode::Enter => app.run_prompt_confirm(),
            KeyCode::Up => app.run_prompt_history(true),
            KeyCode::Down => app.run_prompt_history(false),
            KeyCode::Right | KeyCode::Tab => app.run_prompt_accept_ghost(),
            KeyCode::Backspace => app.run_prompt_backspace(),
            KeyCode::Char(c) => app.run_prompt_push(c),
            _ => {}
        },
        // A focused terminal is capturing input: forward keys to its PTY. The
        // app reserves the release chord (Ctrl-a) and drops back to Normal.
        Mode::Terminal => app.terminal_input(&key),
    }
}
