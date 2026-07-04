//! The interactive TUI event loop (§8.2). Rendering lives in `stacksaw-ui`;
//! this wires crossterm input and terminal setup around it.

use std::io::Stdout;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use stacksaw_ui::layout::ColumnKind;
use stacksaw_ui::App;

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
            let raw = app.selected_file_is_added();
            let text = if raw {
                stacksaw_git::file_content(&ctx.repo_root, &oid, &path).unwrap_or_default()
            } else {
                stacksaw_git::file_diff(&ctx.repo_root, &oid, &path).unwrap_or_default()
            };
            app.set_diff(oid, path, &text, raw);
        }

        terminal.draw(|f| app.draw(f))?;

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('1') => app.focused = ColumnKind::Stacks,
                        KeyCode::Char('2') => app.focused = ColumnKind::Commits,
                        KeyCode::Char('3') => app.focused = ColumnKind::Files,
                        KeyCode::Char('4') => app.focused = ColumnKind::Diff,
                        KeyCode::Char('5') => {
                            app.checks_open = !app.checks_open;
                            app.focused = ColumnKind::Checks;
                        }
                        KeyCode::Char('z') => app.zoom = !app.zoom,
                        KeyCode::Tab => app.focused = next_column(app.focused, app.checks_open),
                        // j/k (and arrows) move within the focused column.
                        KeyCode::Char('j') | KeyCode::Down => app.move_selection(true),
                        KeyCode::Char('k') | KeyCode::Up => app.move_selection(false),
                        // J/K always move between stacks, regardless of focus.
                        KeyCode::Char('J') => app.move_stair(true),
                        KeyCode::Char('K') => app.move_stair(false),
                        _ => {}
                    }
                }
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => app.on_click(m.column, m.row),
                    MouseEventKind::ScrollDown => app.on_scroll(m.column, m.row, true),
                    MouseEventKind::ScrollUp => app.on_scroll(m.column, m.row, false),
                    _ => {}
                },
                _ => {}
            }
        }

        // Refresh from the repo periodically so external changes appear (§6).
        if last_refresh.elapsed() > Duration::from_millis(500) {
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

fn next_column(cur: ColumnKind, checks_open: bool) -> ColumnKind {
    let order = if checks_open {
        vec![
            ColumnKind::Stacks,
            ColumnKind::Commits,
            ColumnKind::Files,
            ColumnKind::Diff,
            ColumnKind::Checks,
        ]
    } else {
        vec![
            ColumnKind::Stacks,
            ColumnKind::Commits,
            ColumnKind::Files,
            ColumnKind::Diff,
        ]
    };
    let idx = order.iter().position(|c| *c == cur).unwrap_or(0);
    order[(idx + 1) % order.len()]
}
