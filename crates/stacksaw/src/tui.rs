//! The interactive TUI event loop (§8.2). Rendering lives in `stacksaw-ui`;
//! this wires crossterm input and terminal setup around it.

use std::io::Stdout;
use std::time::Duration;

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
use stacksaw_ui::{command, App};

use crate::context::Ctx;

/// Run a UI window until the user quits.
pub fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let repo = ctx.repo()?;
    let snapshot = stacksaw_git::build_snapshot(&repo, 0, &ctx.model_options())?;
    let mut app = App::new(snapshot);
    app.truecolor = detect_truecolor();

    let mut terminal = setup()?;
    let res = event_loop(ctx, &mut terminal, &mut app);
    restore(&mut terminal)?;
    res
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

fn event_loop(ctx: &Ctx, terminal: &mut Term, app: &mut App) -> anyhow::Result<()> {
    let mut last_refresh = std::time::Instant::now();
    loop {
        // Populate the Files column for the selected commit (lazily, only when
        // the selection has moved).
        if let Some(oid) = app.files_needing_load() {
            let files = stacksaw_git::changed_files(&ctx.repo_root, &oid).unwrap_or_default();
            app.set_files(oid, files);
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

        if event::poll(Duration::from_millis(250))? {
            let ev = event::read()?;
            match &ev {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        handle_key(app, *key);
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
            last_refresh = std::time::Instant::now();
        }

        if app.should_quit {
            break;
        }

        // Refresh from the repo periodically so external changes appear (§6).
        if last_refresh.elapsed() > Duration::from_millis(3000) {
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
    Ok(())
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
