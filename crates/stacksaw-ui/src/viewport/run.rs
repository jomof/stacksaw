//! The Run viewport contributor: a command terminal. The command runs under a
//! PTY (host-side) and its byte stream is fed to an embedded `vt100` emulator,
//! so full ANSI/VT renders faithfully. Unlike Diff, a `RunView` renders itself.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

/// Scrollback lines retained per command terminal.
const RUN_SCROLLBACK: usize = 5000;

/// Status of a command terminal, used for its tab badge and hint copy.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TabStatus {
    /// A non-command tab (Diff): no status.
    Static,
    /// The command is still executing.
    Running,
    /// The command exited with this code.
    Exited(i32),
}

/// A live decoration a contributor paints on its own tab button, between the
/// label and the close `x`. `role` selects glyph+color from `theme.toml`;
/// `cancel` marks it as an interactive affordance (click to cancel the run).
pub struct TabBadge {
    pub role: &'static str,
    pub cancel: bool,
}

/// The execution context a command ran in, as display-ready strings (the host
/// resolves and abbreviates the paths, so the renderer stays dumb).
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    /// The repo working tree root (e.g. `~/proj`).
    pub repo_root: String,
    /// The git directory (`.git` when nested under the root, else a full path).
    pub git_dir: String,
}

/// A command terminal: the command runs under a PTY (host-side) and its byte
/// stream is fed to this embedded `vt100` emulator, so full ANSI/VT renders
/// faithfully.
pub struct RunView {
    pub id: u64,
    pub command: String,
    pub label: String,
    /// The commit this command was launched against (for re-run), or `None` for
    /// the current HEAD/worktree.
    pub target_oid: Option<String>,
    /// The repo/git context the command executed in (for the tab header).
    pub context: RunContext,
    parser: vt100::Parser,
    rows: u16,
    cols: u16,
    exit: Option<i32>,
}

impl RunView {
    pub fn new(
        id: u64,
        command: String,
        label: String,
        target_oid: Option<String>,
        context: RunContext,
        rows: u16,
        cols: u16,
    ) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        RunView {
            id,
            command,
            label,
            target_oid,
            context,
            parser: vt100::Parser::new(rows, cols, RUN_SCROLLBACK),
            rows,
            cols,
            exit: None,
        }
    }

    /// Feed raw PTY bytes to the emulator.
    pub fn push(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// Record that the child process has exited.
    pub fn finish(&mut self, code: i32) {
        self.exit = Some(code);
    }

    pub fn is_running(&self) -> bool {
        self.exit.is_none()
    }

    pub fn status(&self) -> TabStatus {
        match self.exit {
            None => TabStatus::Running,
            Some(code) => TabStatus::Exited(code),
        }
    }

    /// Resize the emulator grid to match the pane (rows, cols).
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if (rows, cols) != (self.rows, self.cols) {
            self.rows = rows;
            self.cols = cols;
            self.parser.screen_mut().set_size(rows, cols);
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    /// The number of rows from the top that carry any output — i.e. the row just
    /// past the last non-blank line. Used to place the finished-command action
    /// buttons right after the output.
    pub fn content_height(&self) -> u16 {
        let screen = self.parser.screen();
        let mut last = 0u16;
        for row in 0..self.rows {
            for col in 0..self.cols {
                if screen.cell(row, col).is_some_and(|c| c.has_contents()) {
                    last = row + 1;
                    break;
                }
            }
        }
        last
    }

    /// Page through the scrollback (down = toward the live screen).
    pub fn on_scroll(&mut self, down: bool) {
        let screen = self.parser.screen();
        let cur = screen.scrollback();
        let next = if down {
            cur.saturating_sub(3)
        } else {
            cur + 3
        };
        self.parser.screen_mut().set_scrollback(next);
    }

    /// The live badge for this tab: a green dot while running, red on failure.
    pub fn badge(&self) -> Option<TabBadge> {
        match self.status() {
            TabStatus::Running => Some(TabBadge {
                role: "tab_status_running",
                cancel: true,
            }),
            TabStatus::Exited(0) => None,
            TabStatus::Exited(_) => Some(TabBadge {
                role: "tab_status_failed",
                cancel: false,
            }),
            TabStatus::Static => None,
        }
    }

    /// Encode a key press into the byte sequence to write to the PTY (capture
    /// mode). Honors application-cursor mode for the arrow keys.
    pub fn key_bytes(&self, key: &KeyEvent) -> Vec<u8> {
        let app_cursor = self.parser.screen().application_cursor();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let arrow = |c: char| {
            let intro = if app_cursor { b"\x1bO" } else { b"\x1b[" };
            let mut v = intro.to_vec();
            v.push(c as u8);
            v
        };
        match key.code {
            KeyCode::Char(c) => {
                if ctrl {
                    // Control character: map a..z/@.._ to 0x00..0x1f.
                    let b = (c.to_ascii_uppercase() as u8).wrapping_sub(b'@') & 0x7f;
                    vec![b]
                } else {
                    c.to_string().into_bytes()
                }
            }
            KeyCode::Enter => vec![b'\r'],
            KeyCode::Backspace => vec![0x7f],
            KeyCode::Tab => vec![b'\t'],
            KeyCode::BackTab => b"\x1b[Z".to_vec(),
            KeyCode::Esc => vec![0x1b],
            KeyCode::Up => arrow('A'),
            KeyCode::Down => arrow('B'),
            KeyCode::Right => arrow('C'),
            KeyCode::Left => arrow('D'),
            KeyCode::Home => b"\x1b[H".to_vec(),
            KeyCode::End => b"\x1b[F".to_vec(),
            KeyCode::PageUp => b"\x1b[5~".to_vec(),
            KeyCode::PageDown => b"\x1b[6~".to_vec(),
            KeyCode::Delete => b"\x1b[3~".to_vec(),
            KeyCode::Insert => b"\x1b[2~".to_vec(),
            _ => Vec::new(),
        }
    }

    /// Render the emulator screen into `area`, cell by cell.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let screen = self.parser.screen();
        let scrolled = screen.scrollback() > 0;
        let (cursor_row, cursor_col) = screen.cursor_position();
        // Drop the cursor once the command has exited — its leftover block would
        // otherwise linger below the final output.
        let show_cursor = self.is_running() && !screen.hide_cursor() && !scrolled;
        let buf = frame.buffer_mut();
        for row in 0..area.height {
            for col in 0..area.width {
                let sx = area.x + col;
                let sy = area.y + row;
                if sx >= buf.area.right() || sy >= buf.area.bottom() {
                    continue;
                }
                let cell = screen.cell(row, col);
                let target = &mut buf[(sx, sy)];
                match cell {
                    Some(c) if c.has_contents() => {
                        target.set_symbol(c.contents());
                        let mut style = cell_style(c);
                        if show_cursor && row == cursor_row && col == cursor_col {
                            style = style.add_modifier(Modifier::REVERSED);
                        }
                        target.set_style(style);
                    }
                    _ => {
                        target.set_symbol(" ");
                        let mut style = Style::default();
                        if show_cursor && row == cursor_row && col == cursor_col {
                            style = style.add_modifier(Modifier::REVERSED);
                        }
                        target.set_style(style);
                    }
                }
            }
        }
    }
}

/// Translate a `vt100` cell's colors and attributes into a ratatui style.
fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = convert_color(cell.fgcolor()) {
        style = style.fg(fg);
    }
    if let Some(bg) = convert_color(cell.bgcolor()) {
        style = style.bg(bg);
    }
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    if cell.dim() {
        style = style.add_modifier(Modifier::DIM);
    }
    style
}

fn convert_color(color: vt100::Color) -> Option<Color> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: u64) -> RunView {
        RunView::new(id, "cmd".into(), "cmd".into(), None, RunContext::default(), 10, 40)
    }

    #[test]
    fn run_badge_tracks_status() {
        let mut r = run(1);
        assert!(matches!(r.status(), TabStatus::Running));
        assert_eq!(r.badge().map(|b| b.role), Some("tab_status_running"));
        assert!(r.badge().map(|b| b.cancel).unwrap_or(false));
        r.finish(0);
        assert!(matches!(r.status(), TabStatus::Exited(0)));
        assert!(r.badge().is_none(), "clean exit shows no badge");
        r.finish(2);
        assert_eq!(r.badge().map(|b| b.role), Some("tab_status_failed"));
    }
}
