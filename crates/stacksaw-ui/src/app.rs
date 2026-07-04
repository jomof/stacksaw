//! The TUI application: state + ratatui rendering of the column scene (§8).

use std::cell::RefCell;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use stacksaw_rainbox::{
    golden_angle_hue, staircase_arc_hue, Background, RainboxColor, StaircaseArc,
};
use stacksaw_ssp::types::{FileEntry, Snapshot, Staircase, WORKTREE_OID};

use crate::command::{self, Action, Command};
use crate::highlight::Highlighter;
use crate::layout::{self, ColumnKind};

/// Which interaction mode the UI is in. Overlays capture input until dismissed
/// (§8.2 command palette / help).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Help,
    Palette,
}

/// Command-palette state: the fuzzy query and the highlighted result row.
#[derive(Default)]
struct PaletteState {
    query: String,
    selected: usize,
}

/// Whether a diff row is an unchanged, added, or deleted line — drives its
/// background tint in the full-file diff view (§8.5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffKind {
    Context,
    Add,
    Del,
}

/// One rendered Diff row: its change kind plus syntax-highlighted text segments
/// (marker already stripped). Cached at load time so highlighting runs once.
struct DiffRow {
    kind: DiffKind,
    spans: Vec<(Color, String)>,
}

/// Status marker identifying the virtual "commit message" row in the Files
/// column (an envelope glyph, distinct from git's A/M/D/R status letters).
const MESSAGE_STATUS: &str = "✉";
/// Display label for the virtual commit-message row.
const MESSAGE_PATH: &str = "commit message";
/// Context rows kept above the first change when a full-file diff opens (§8.5).
const DIFF_CONTEXT_ABOVE: u16 = 3;

/// Clickable regions recorded during the last `draw` so mouse coordinates can
/// be mapped back to selections (§8.2 mouse input). Screen-space, 0-based.
#[derive(Default)]
struct Hit {
    /// Outer rect of each visible column (expanded or spine).
    columns: Vec<(ColumnKind, Rect)>,
    /// `(screen_row, stair index)` for each rendered row in the Stacks column.
    stacks: Vec<(u16, usize)>,
    /// `(screen_row, commit index)` for each commit card in the Commits column.
    commits: Vec<(u16, usize)>,
    /// `(screen_row, file index)` for each row in the Files column.
    files: Vec<(u16, usize)>,
}

/// Application state (view state is client-local per §3.2).
pub struct App {
    pub snapshot: Snapshot,
    pub focused: ColumnKind,
    pub selected_stair: usize,
    pub selected_commit: usize,
    pub selected_file: usize,
    pub zoom: bool,
    pub checks_open: bool,
    pub background: Background,
    /// Whether the terminal renders 24-bit truecolor. When false we emit
    /// 256-color indexed values instead, so hues survive on terminals (e.g.
    /// macOS Terminal.app) that ignore RGB escapes.
    pub truecolor: bool,
    /// Changed files of the currently selected commit (§8.1). Populated lazily
    /// by the host as the selection changes.
    pub files: Vec<FileEntry>,
    /// The commit oid whose files are currently loaded into `files`.
    loaded_oid: Option<String>,
    /// Syntax-highlighted rows of the selected file's diff/content (§8.5),
    /// computed once per load. Empty until a file is selected.
    diff: Vec<DiffRow>,
    /// True when the loaded diff is raw file content (added file / commit
    /// message) rather than a modified-file patch, which affects the empty
    /// placeholder text and the initial scroll position.
    diff_is_raw: bool,
    /// `(commit oid, file path)` currently loaded into `diff`.
    loaded_diff_key: Option<(String, String)>,
    /// Vertical scroll offset into the diff view.
    diff_scroll: u16,
    /// Current interaction mode (normal vs. an overlay).
    mode: Mode,
    /// Command-palette state (query + selection); only meaningful in
    /// [`Mode::Palette`].
    palette: PaletteState,
    /// Set by the `Quit` action; the host event loop observes it and exits.
    pub should_quit: bool,
    /// Hit-test regions from the most recent render.
    hit: RefCell<Hit>,
}

impl App {
    pub fn new(snapshot: Snapshot) -> Self {
        App {
            snapshot,
            focused: ColumnKind::Commits,
            selected_stair: 0,
            selected_commit: 0,
            selected_file: 0,
            zoom: false,
            checks_open: false,
            background: Background::Dark,
            truecolor: true,
            files: Vec::new(),
            loaded_oid: None,
            diff: Vec::new(),
            diff_is_raw: false,
            loaded_diff_key: None,
            diff_scroll: 0,
            mode: Mode::Normal,
            palette: PaletteState::default(),
            should_quit: false,
            hit: RefCell::new(Hit::default()),
        }
    }

    fn selected(&self) -> Option<&Staircase> {
        self.snapshot.staircases.get(self.selected_stair)
    }

    /// Resolve an identity hue to a terminal color, honoring the terminal's
    /// color depth (truecolor RGB, else 256-color indexed).
    fn hue_to_color(&self, hue: f32) -> Color {
        let c = RainboxColor::from_hue(hue).dimmed(1.0, self.background);
        if self.truecolor {
            let (r, g, b) = c.to_rgb();
            Color::Rgb(r, g, b)
        } else {
            Color::Indexed(c.to_ansi256())
        }
    }

    /// The oid of the currently selected commit, walking segments in order.
    pub fn selected_commit_oid(&self) -> Option<String> {
        let stair = self.selected()?;
        stair
            .segments
            .iter()
            .flat_map(|seg| seg.commits.iter())
            .nth(self.selected_commit)
            .map(|c| c.oid.clone())
    }

    /// The oid whose files need loading, if the selection has moved off the
    /// currently loaded commit. `None` means the Files column is up to date.
    pub fn files_needing_load(&self) -> Option<String> {
        let oid = self.selected_commit_oid()?;
        (self.loaded_oid.as_deref() != Some(oid.as_str())).then_some(oid)
    }

    /// Install the changed files for `oid` (called by the host after a fetch).
    /// A virtual "commit message" row is pinned at the top so the message body
    /// is one selection away and shows in the Diff column (§8.1).
    pub fn set_files(&mut self, oid: String, files: Vec<FileEntry>) {
        self.loaded_oid = Some(oid);
        self.files = Vec::with_capacity(files.len() + 1);
        self.files.push(FileEntry {
            status: MESSAGE_STATUS.to_string(),
            path: MESSAGE_PATH.to_string(),
            ..Default::default()
        });
        self.files.extend(files);
        self.selected_file = 0;
    }

    /// True when the selected Files row is the virtual commit-message entry, so
    /// the Diff column should show the full message rather than a patch.
    pub fn selected_file_is_message(&self) -> bool {
        self.files
            .get(self.selected_file)
            .map(|f| f.status == MESSAGE_STATUS)
            .unwrap_or(false)
    }

    /// Path of the currently selected file, if any.
    pub fn selected_file_path(&self) -> Option<String> {
        self.files.get(self.selected_file).map(|f| f.path.clone())
    }

    /// True when the selected file was added by this commit, so the Diff column
    /// should show its full content rather than an all-`+` patch.
    pub fn selected_file_is_added(&self) -> bool {
        self.files
            .get(self.selected_file)
            .map(|f| f.status.starts_with('A'))
            .unwrap_or(false)
    }

    /// The `(oid, path)` whose diff needs loading, if the selection has moved
    /// off the currently loaded diff. `None` means the Diff column is current.
    pub fn diff_needing_load(&self) -> Option<(String, String)> {
        let oid = self.selected_commit_oid()?;
        let path = self.selected_file_path()?;
        let key = (oid, path);
        (self.loaded_diff_key.as_ref() != Some(&key)).then_some(key)
    }

    /// Install the diff text for `(oid, path)` (called by the host). `raw` marks
    /// the text as plain file content (added file / commit message) rather than
    /// a unified patch. The text is syntax-highlighted (by `path`) and cached as
    /// rendered rows here, so highlighting runs once per load rather than per
    /// frame.
    pub fn set_diff(&mut self, oid: String, path: String, text: &str, raw: bool) {
        let mut hl = Highlighter::for_path(&path, self.truecolor);
        let mut rows = Vec::new();
        for line in text.lines() {
            if !raw && is_diff_meta(line) {
                continue;
            }
            let (kind, body) = if raw {
                (DiffKind::Context, line)
            } else {
                match line.as_bytes().first() {
                    Some(b'+') => (DiffKind::Add, &line[1..]),
                    Some(b'-') => (DiffKind::Del, &line[1..]),
                    Some(b' ') => (DiffKind::Context, &line[1..]),
                    _ => (DiffKind::Context, line),
                }
            };
            rows.push(DiffRow {
                kind,
                spans: hl.line(body),
            });
        }
        self.diff = rows;
        self.diff_is_raw = raw;
        self.loaded_diff_key = Some((oid, path));
        // For a full-file diff, open scrolled to the first change (keeping a few
        // context lines above) rather than at the top, which may be far from any
        // edit. Raw content always opens at the top.
        self.diff_scroll = if raw {
            0
        } else {
            self.first_change_scroll(DIFF_CONTEXT_ABOVE)
        };
    }

    /// Current vertical scroll offset of the Diff viewport (rendered rows).
    pub fn diff_scroll(&self) -> u16 {
        self.diff_scroll
    }

    /// The scroll offset (in rendered rows) that places the first added/deleted
    /// line `context` rows below the top of the viewport. Zero when the file has
    /// no visible change.
    fn first_change_scroll(&self, context: u16) -> u16 {
        let mut body = 0u16;
        for row in &self.diff {
            if row.kind != DiffKind::Context {
                return body.saturating_sub(context);
            }
            body += 1;
        }
        0
    }

    /// Move the selection within the currently focused column (§8.2). Selecting
    /// a different stack or commit resets the dependent selections below it.
    pub fn move_selection(&mut self, down: bool) {
        match self.focused {
            ColumnKind::Stacks => self.move_stair(down),
            ColumnKind::Files => {
                let last = self.files.len().saturating_sub(1);
                self.selected_file = step(self.selected_file, down, last);
            }
            _ => {
                let last = self.commit_count().saturating_sub(1);
                self.selected_commit = step(self.selected_commit, down, last);
                self.selected_file = 0;
            }
        }
    }

    /// Move the stack selection, resetting the commit/file selections beneath.
    pub fn move_stair(&mut self, down: bool) {
        let last = self.snapshot.staircases.len().saturating_sub(1);
        self.selected_stair = step(self.selected_stair, down, last);
        self.selected_commit = 0;
        self.selected_file = 0;
    }

    /// Number of commits in the selected staircase (for clamping selection).
    fn commit_count(&self) -> usize {
        self.selected()
            .map(|s| s.segments.iter().map(|seg| seg.commits.len()).sum())
            .unwrap_or(0)
    }

    /// Handle a left click at screen coordinates: focus the clicked column and,
    /// in Stacks/Commits, select the clicked row (§8.2).
    pub fn on_click(&mut self, x: u16, y: u16) {
        enum Target {
            Focus(ColumnKind),
            Stair(usize),
            Commit(usize),
            File(usize),
        }
        let mut actions: Vec<Target> = Vec::new();
        {
            let hit = self.hit.borrow();
            let Some((kind, _)) = hit.columns.iter().find(|(_, r)| contains(*r, x, y)) else {
                return;
            };
            actions.push(Target::Focus(*kind));
            match kind {
                ColumnKind::Stacks => {
                    if let Some((_, idx)) = hit.stacks.iter().find(|(ry, _)| *ry == y) {
                        actions.push(Target::Stair(*idx));
                    }
                }
                ColumnKind::Commits => {
                    if let Some((_, idx)) = hit.commits.iter().find(|(ry, _)| *ry == y) {
                        actions.push(Target::Commit(*idx));
                    }
                }
                ColumnKind::Files => {
                    if let Some((_, idx)) = hit.files.iter().find(|(ry, _)| *ry == y) {
                        actions.push(Target::File(*idx));
                    }
                }
                _ => {}
            }
        }
        for a in actions {
            match a {
                Target::Focus(k) => self.focused = k,
                Target::Stair(i) => {
                    self.selected_stair = i;
                    self.selected_commit = 0;
                    self.selected_file = 0;
                }
                Target::Commit(i) => {
                    self.selected_commit = i;
                    self.selected_file = 0;
                }
                Target::File(i) => self.selected_file = i,
            }
        }
    }

    /// Handle a scroll-wheel tick over screen coordinates: move the selection in
    /// whichever column the pointer is over (§8.2).
    pub fn on_scroll(&mut self, x: u16, y: u16, down: bool) {
        let over = {
            let hit = self.hit.borrow();
            hit.columns
                .iter()
                .find(|(_, r)| contains(*r, x, y))
                .map(|(k, _)| *k)
        };
        let over = over.unwrap_or(self.focused);
        match over {
            ColumnKind::Stacks => self.move_stair(down),
            ColumnKind::Files => {
                let last = self.files.len().saturating_sub(1);
                self.selected_file = step(self.selected_file, down, last);
            }
            ColumnKind::Diff => {
                // Scroll the diff viewport rather than moving a selection.
                let last = self.diff.len().saturating_sub(1) as u16;
                self.diff_scroll = if down {
                    (self.diff_scroll + 3).min(last)
                } else {
                    self.diff_scroll.saturating_sub(3)
                };
            }
            _ => {
                let last = self.commit_count().saturating_sub(1);
                self.selected_commit = step(self.selected_commit, down, last);
                self.selected_file = 0;
            }
        }
    }

    /// The current interaction mode (normal vs. an overlay).
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Apply a registry [`Action`] (§8.2). This is the one place actions take
    /// effect, so the keymap, palette, and any future scripting all converge
    /// here.
    pub fn apply(&mut self, action: Action) {
        match action {
            Action::MoveDown => self.move_selection(true),
            Action::MoveUp => self.move_selection(false),
            Action::StairDown => self.move_stair(true),
            Action::StairUp => self.move_stair(false),
            Action::NextColumn => self.cycle_column(),
            Action::Focus(k) => self.focused = k,
            Action::ToggleChecks => {
                self.checks_open = !self.checks_open;
                self.focused = ColumnKind::Checks;
            }
            Action::ToggleZoom => self.zoom = !self.zoom,
            Action::OpenPalette => {
                self.palette = PaletteState::default();
                self.mode = Mode::Palette;
            }
            Action::OpenHelp => self.mode = Mode::Help,
            Action::Quit => self.should_quit = true,
        }
    }

    /// Advance focus to the next visible column (§8.2 `Tab`).
    fn cycle_column(&mut self) {
        let order: &[ColumnKind] = if self.checks_open {
            &[
                ColumnKind::Stacks,
                ColumnKind::Commits,
                ColumnKind::Files,
                ColumnKind::Diff,
                ColumnKind::Checks,
            ]
        } else {
            &[
                ColumnKind::Stacks,
                ColumnKind::Commits,
                ColumnKind::Files,
                ColumnKind::Diff,
            ]
        };
        let idx = order.iter().position(|c| *c == self.focused).unwrap_or(0);
        self.focused = order[(idx + 1) % order.len()];
    }

    // --- Overlay input (help / command palette) --------------------------

    /// Dismiss any open overlay, returning to normal mode.
    pub fn close_overlay(&mut self) {
        self.mode = Mode::Normal;
    }

    /// Append a character to the palette query.
    pub fn palette_input(&mut self, c: char) {
        self.palette.query.push(c);
        self.palette.selected = 0;
    }

    /// Delete the last character of the palette query.
    pub fn palette_backspace(&mut self) {
        self.palette.query.pop();
        self.palette.selected = 0;
    }

    /// Move the palette selection up/down, clamped to the result count.
    pub fn palette_move(&mut self, down: bool) {
        let last = self.palette_results().len().saturating_sub(1);
        self.palette.selected = step(self.palette.selected, down, last);
    }

    /// Confirm the highlighted palette entry: close the palette and return its
    /// action for the host to [`apply`](Self::apply).
    pub fn palette_confirm(&mut self) -> Option<Action> {
        let action = self
            .palette_results()
            .get(self.palette.selected)
            .map(|c| c.action);
        self.close_overlay();
        action
    }

    /// The palette's current fuzzy-filtered commands, best match first. An
    /// empty query lists every command in registry order.
    fn palette_results(&self) -> Vec<&'static Command> {
        let all: Vec<&'static Command> = command::registry().iter().collect();
        let query = self.palette.query.trim();
        if query.is_empty() {
            return all;
        }
        use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
        use nucleo_matcher::{Config, Matcher};
        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let titles: Vec<&'static str> = all.iter().map(|c| c.title).collect();
        pattern
            .match_list(titles, &mut matcher)
            .into_iter()
            .filter_map(|(title, _)| all.iter().find(|c| c.title == title).copied())
            .collect()
    }

    /// Draw the full scene for the current terminal size.
    pub fn draw(&self, frame: &mut Frame) {
        {
            let mut hit = self.hit.borrow_mut();
            hit.columns.clear();
            hit.stacks.clear();
            hit.commits.clear();
            hit.files.clear();
        }
        let full = frame.area();
        // Reserve the bottom row for the always-on hint bar (§8.2).
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(full);
        let area = rows[0];
        // Narrow terminals stay in single-column deck mode (§8.1).
        if area.width < layout::DECK_MODE_COLS {
            self.draw_deck(frame, area, self.focused);
        } else {
            // Wide layout: the master columns (Stacks | Commits | Files
            // [| Checks]) sit in a top band, with the Diff pane full-width below
            // them so source code has room to breathe.
            let (top, bottom) = self.split_scene(area);
            if top.height > 0 {
                self.draw_top_columns(frame, top);
            }
            if bottom.height > 0 {
                self.draw_column(frame, bottom, ColumnKind::Diff);
            }
        }
        self.draw_hint_bar(frame, rows[1]);

        // Overlays sit on top of the scene and capture input (§8.2).
        match self.mode {
            Mode::Help => self.draw_help(frame, full),
            Mode::Palette => self.draw_palette(frame, full),
            Mode::Normal => {}
        }
    }

    /// The always-on hint bar: a projection of the command registry showing the
    /// most relevant keys for the focused column (§8.2).
    fn draw_hint_bar(&self, frame: &mut Frame, area: Rect) {
        let mut spans: Vec<RSpan> = Vec::new();
        for (i, cmd) in command::hint_commands(self.focused).iter().enumerate() {
            if i > 0 {
                spans.push(RSpan::styled(" · ", Style::default().fg(Color::DarkGray)));
            }
            spans.push(RSpan::styled(
                cmd.primary_key_label(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
            spans.push(RSpan::raw(" "));
            spans.push(RSpan::styled(
                cmd.title,
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// The `?` help overlay: every command grouped by category (§8.2).
    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        for category in command::Category::ORDER {
            let cmds: Vec<&Command> = command::registry()
                .iter()
                .filter(|c| c.category == category)
                .collect();
            if cmds.is_empty() {
                continue;
            }
            lines.push(Line::from(RSpan::styled(
                category.title(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            for cmd in cmds {
                let keys = cmd
                    .keys
                    .iter()
                    .map(|k| k.label())
                    .collect::<Vec<_>>()
                    .join(" / ");
                lines.push(Line::from(vec![
                    RSpan::styled(
                        format!("  {keys:<10}"),
                        Style::default().fg(Color::Yellow),
                    ),
                    RSpan::raw(" "),
                    RSpan::raw(cmd.title),
                ]));
            }
            lines.push(Line::from(""));
        }
        lines.push(Line::from(RSpan::styled(
            "any key to close",
            Style::default().add_modifier(Modifier::DIM),
        )));

        let popup = centered_rect(48, (lines.len() as u16 + 2).min(area.height), area);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Help — keys")
            .border_style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
        frame.render_widget(Paragraph::new(lines).block(block), popup);
    }

    /// The `:` command palette: a fuzzy-filtered list of every command, each
    /// showing its key so the palette teaches shortcuts (§8.2).
    fn draw_palette(&self, frame: &mut Frame, area: Rect) {
        let results = self.palette_results();
        let popup = centered_rect(52, 16.min(area.height), area);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Command palette")
            .border_style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        // Query line.
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                RSpan::styled("› ", Style::default().fg(Color::Cyan)),
                RSpan::raw(self.palette.query.clone()),
                RSpan::styled("▏", Style::default().add_modifier(Modifier::DIM)),
            ])),
            rows[0],
        );
        // Results, each with its primary key right-aligned.
        let width = rows[1].width as usize;
        let items: Vec<ListItem> = results
            .iter()
            .map(|cmd| {
                let key = cmd.primary_key_label();
                let gap = width
                    .saturating_sub(cmd.title.chars().count() + key.chars().count() + 2)
                    .max(1);
                ListItem::new(Line::from(vec![
                    RSpan::raw(cmd.title),
                    RSpan::raw(" ".repeat(gap)),
                    RSpan::styled(key, Style::default().fg(Color::Cyan)),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        if !results.is_empty() {
            state.select(Some(self.palette.selected.min(results.len() - 1)));
        }
        let list = List::new(items)
            .highlight_style(highlight_style())
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, rows[1], &mut state);
    }

    /// Split the frame into the top column band and the bottom Diff pane. Zoom
    /// gives the focused region the whole frame (Diff when Diff is focused, the
    /// column band otherwise).
    fn split_scene(&self, area: Rect) -> (Rect, Rect) {
        let empty = Rect { height: 0, ..area };
        if self.zoom {
            return if self.focused == ColumnKind::Diff {
                (empty, area)
            } else {
                (area, empty)
            };
        }
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        (rows[0], rows[1])
    }

    /// Lay the master columns across the top band.
    fn draw_top_columns(&self, frame: &mut Frame, area: Rect) {
        let mut columns = vec![ColumnKind::Stacks, ColumnKind::Commits, ColumnKind::Files];
        if self.checks_open {
            columns.push(ColumnKind::Checks);
        }
        // Zoom only maximizes a top column when one is actually focused here.
        let zoom = self.zoom && self.focused != ColumnKind::Diff;
        let slots = layout::plan_over(
            area.width,
            self.focused,
            zoom,
            &columns,
            Some(self.stacks_content_width()),
        );
        let constraints: Vec<Constraint> = slots
            .iter()
            .map(|s| Constraint::Length(s.width.unwrap_or(layout::SPINE_WIDTH)))
            .collect();
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);
        for (slot, rect) in slots.iter().zip(chunks.iter()) {
            match slot.width {
                Some(_) => self.draw_column(frame, *rect, slot.kind),
                None => self.draw_spine(frame, *rect, slot.kind),
            }
        }
    }

    fn draw_deck(&self, frame: &mut Frame, area: Rect, focused: ColumnKind) {
        let crumb = self.breadcrumb(focused);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);
        frame.render_widget(
            Paragraph::new(crumb).style(Style::default().add_modifier(Modifier::BOLD)),
            rows[0],
        );
        self.draw_column(frame, rows[1], focused);
    }

    fn breadcrumb(&self, focused: ColumnKind) -> String {
        let stair = self.selected().map(|s| s.name.as_str()).unwrap_or("—");
        format!("Stacks ▸ {stair} ▸ {}", focused.title())
    }

    fn draw_spine(&self, frame: &mut Frame, area: Rect, kind: ColumnKind) {
        self.hit.borrow_mut().columns.push((kind, area));
        // Rotated title + identity strip (§8.1). Rendered vertically.
        let title: String = kind.title().chars().take(area.height as usize).collect();
        let vertical: Vec<Line> = title.chars().map(|c| Line::from(c.to_string())).collect();
        frame.render_widget(
            Paragraph::new(vertical).block(Block::default().borders(Borders::RIGHT)),
            area,
        );
    }

    fn draw_column(&self, frame: &mut Frame, area: Rect, kind: ColumnKind) {
        self.hit.borrow_mut().columns.push((kind, area));
        let focused = kind == self.focused;
        let border_style = if focused {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(kind.title())
            .border_style(border_style);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        match kind {
            ColumnKind::Stacks => self.draw_stacks(frame, inner),
            ColumnKind::Commits => self.draw_commits(frame, inner),
            ColumnKind::Files => self.draw_files(frame, inner),
            ColumnKind::Diff => self.draw_diff(frame, inner),
            ColumnKind::Checks => self.draw_checks(frame, inner),
        }
    }

    /// Outer width the Stacks column needs to show its widest row without
    /// truncation: highlight marker + name + the `↑a ↓b` counters + borders.
    fn stacks_content_width(&self) -> u16 {
        const MARKER: usize = 2; // "▶ "
        const BORDERS: usize = 2; // left + right column borders
        let content = self
            .snapshot
            .staircases
            .iter()
            .map(|s| {
                let dirty = if s.dirty { 2 } else { 0 }; // " ✎"
                let counters = format!("  ↑{} ↓{}", s.ahead, s.behind).chars().count();
                s.name.chars().count() + counters + dirty
            })
            .max()
            .unwrap_or(0);
        // Ensure the "Stacks" title still fits in the border.
        let title = "Stacks".len();
        (MARKER + content.max(title) + BORDERS) as u16
    }

    fn draw_stacks(&self, frame: &mut Frame, area: Rect) {
        {
            let mut hit = self.hit.borrow_mut();
            for i in 0..self.snapshot.staircases.len() {
                let ry = area.y + i as u16;
                if ry >= area.y + area.height {
                    break;
                }
                hit.stacks.push((ry, i));
            }
        }
        let items: Vec<ListItem> = self
            .snapshot
            .staircases
            .iter()
            .map(|s| {
                // Each staircase keeps its own identity hue (§8.3).
                let color = self.hue_to_color(golden_angle_hue(&s.name));
                let dirty = if s.dirty { " ✎" } else { "" };
                let line = Line::from(vec![
                    RSpan::styled(s.name.clone(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    RSpan::styled(
                        format!("  ↑{} ↓{}{}", s.ahead, s.behind, dirty),
                        Style::default().add_modifier(Modifier::DIM),
                    ),
                ]);
                ListItem::new(line)
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.selected_stair));
        let list = List::new(items)
            .highlight_style(highlight_style())
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_commits(&self, frame: &mut Frame, area: Rect) {
        let Some(stair) = self.selected() else {
            frame.render_widget(Paragraph::new("no staircase"), area);
            return;
        };
        // Header row + list below.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        let header = format!("upstream {} ↑{} ↓{}", stair.upstream, stair.ahead, stair.behind);
        frame.render_widget(
            Paragraph::new(header).style(Style::default().add_modifier(Modifier::DIM)),
            rows[0],
        );
        let list_area = rows[1];

        let arc = StaircaseArc::default();
        // Rainbow the commits across the whole stack: each commit gets its own
        // step along the staircase arc (§8.3), not one hue per segment.
        let total = stair.segments.iter().map(|s| s.commits.len()).sum::<usize>();
        let mut items: Vec<ListItem> = Vec::new();
        // Line index (within the list) of each commit, for hit-testing + state.
        let mut commit_line: Vec<usize> = Vec::new();
        let mut commit_idx = 0usize;

        for (si, seg) in stair.segments.iter().enumerate() {
            let indent = "  ".repeat(seg.parent.map_or(0, |_| si.min(6)));
            let riser_hue = staircase_arc_hue(arc, commit_idx.min(total.saturating_sub(1)), total);
            let riser_color = self.hue_to_color(riser_hue);
            items.push(ListItem::new(Line::from(vec![
                RSpan::raw(indent.clone()),
                RSpan::styled(
                    format!("╭┴ {} ─", seg.branch),
                    Style::default().fg(riser_color).add_modifier(Modifier::DIM),
                ),
            ])));
            for c in &seg.commits {
                const MARKER: usize = 2; // "▶ " highlight symbol
                let content_w = (list_area.width as usize).saturating_sub(MARKER);
                // The virtual worktree commit renders distinctly (§8.3): a pencil
                // glyph + label in an editorial yellow, churn still right-aligned.
                if c.oid == WORKTREE_OID {
                    let label = "✎ Uncommitted changes";
                    let churn_w = stat_width(c.added, c.deleted);
                    let pad = content_w
                        .saturating_sub(label.chars().count() + churn_w)
                        .max(1);
                    commit_line.push(items.len());
                    let mut spans = vec![
                        RSpan::styled(
                            label.to_string(),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::ITALIC),
                        ),
                        spaces(pad),
                    ];
                    spans.extend(stat_spans(c.added, c.deleted));
                    items.push(ListItem::new(Line::from(spans)));
                    commit_idx += 1;
                    continue;
                }
                let hue = staircase_arc_hue(arc, commit_idx, total);
                let color = self.hue_to_color(hue);
                let chips = commit_chips(c);
                let churn_w = stat_width(c.added, c.deleted);
                // The `-N +M` churn is right-justified against the column edge;
                // the subject fills the space in between, truncated (from the
                // back) only when it would otherwise collide with the churn.
                // Reserve the highlight marker, indent, hash, chips, and churn.
                let indent_w = indent.chars().count();
                let short_w = c.short.chars().count();
                let chips_w = chips.chars().count();
                let fixed = indent_w + short_w + 1 + chips_w + churn_w;
                let budget = content_w.saturating_sub(fixed + 1).max(8);
                let subject = truncate(&c.subject, budget);
                let used_left = indent_w + short_w + 1 + subject.chars().count() + chips_w;
                let pad = content_w.saturating_sub(used_left + churn_w).max(1);
                commit_line.push(items.len());
                let mut spans = vec![
                    RSpan::styled(format!("{indent}"), Style::default()),
                    RSpan::styled(c.short.clone(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    RSpan::styled(format!(" {subject}"), Style::default().fg(color)),
                    RSpan::styled(chips, Style::default().fg(color)),
                    spaces(pad),
                ];
                spans.extend(stat_spans(c.added, c.deleted));
                items.push(ListItem::new(Line::from(spans)));
                commit_idx += 1;
            }
        }

        // Hit rows: list starts at list_area.y; map each commit's line index.
        {
            let mut hit = self.hit.borrow_mut();
            for (ci, &line) in commit_line.iter().enumerate() {
                let ry = list_area.y + line as u16;
                if ry >= list_area.y + list_area.height {
                    break;
                }
                hit.commits.push((ry, ci));
            }
        }

        let selected_line = commit_line.get(self.selected_commit).copied();
        let mut state = ListState::default();
        state.select(selected_line);
        let list = List::new(items)
            .highlight_style(highlight_style())
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, list_area, &mut state);
    }

    fn draw_files(&self, frame: &mut Frame, area: Rect) {
        if self.files.is_empty() {
            let msg = if self.selected_commit_oid().is_some() {
                "(no changed files)"
            } else {
                "(select a commit)"
            };
            frame.render_widget(
                Paragraph::new(msg).style(Style::default().add_modifier(Modifier::DIM)),
                area,
            );
            return;
        }
        {
            let mut hit = self.hit.borrow_mut();
            for i in 0..self.files.len() {
                let ry = area.y + i as u16;
                if ry >= area.y + area.height {
                    break;
                }
                hit.files.push((ry, i));
            }
        }
        let items: Vec<ListItem> = self
            .files
            .iter()
            .map(|f| {
                // The pinned commit-message row renders as a labelled envelope,
                // not a path (no directory split, no rainbow-by-folder).
                if f.status == MESSAGE_STATUS {
                    return ListItem::new(Line::from(vec![
                        RSpan::styled(
                            format!("{MESSAGE_STATUS} "),
                            Style::default().fg(Color::Gray),
                        ),
                        RSpan::styled(
                            f.path.clone(),
                            Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                }
                let status = f.status.chars().next().unwrap_or('?');
                let (dir, name) = split_path(&f.path);
                // Filename first (never hidden), colored by its directory so
                // files in the same folder share a hue (§8.3).
                let name_color = self.hue_to_color(golden_angle_hue(dir));
                let churn_w = stat_width(f.added, f.deleted);
                const MARKER: usize = 2; // "▶ " highlight symbol
                let content_w = (area.width as usize).saturating_sub(MARKER);
                let status_w = 2; // "M "
                let name_w = name.chars().count();
                // Right-justify the churn; give whatever space is left to the
                // directory, shortening it from the *front* so the leaf folder
                // stays visible. The filename is never truncated.
                let reserved = status_w + name_w + churn_w + 1; // +1 min gap
                let mut spans = vec![
                    RSpan::styled(format!("{status} "), Style::default().fg(status_color(status))),
                    RSpan::styled(name.to_string(), Style::default().fg(name_color).add_modifier(Modifier::BOLD)),
                ];
                let mut used_left = status_w + name_w;
                if !dir.is_empty() {
                    // Directory block is "  {dir}"; budget its dir portion.
                    let dir_max = content_w.saturating_sub(reserved + 2); // 2 = "  "
                    if dir_max > 0 {
                        let shown = truncate_front(dir, dir_max);
                        used_left += 2 + shown.chars().count();
                        spans.push(RSpan::styled(
                            format!("  {shown}"),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }
                let pad = content_w.saturating_sub(used_left + churn_w).max(1);
                spans.push(spaces(pad));
                spans.extend(stat_spans(f.added, f.deleted));
                ListItem::new(Line::from(spans))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.selected_file));
        let list = List::new(items)
            .highlight_style(highlight_style())
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_diff(&self, frame: &mut Frame, area: Rect) {
        if self.diff.is_empty() {
            let msg = match (self.selected_commit_oid(), self.selected_file_path()) {
                (Some(_), Some(_)) if self.diff_is_raw => "(empty file)",
                (Some(_), Some(_)) => "(no diff for this file)",
                _ => "(select a file)",
            };
            frame.render_widget(
                Paragraph::new(msg).style(Style::default().add_modifier(Modifier::DIM)),
                area,
            );
            return;
        }
        // Every row is syntax-highlighted (cached at load). Modified-file rows
        // additionally get a full-width tinted background — green for additions,
        // red for deletions — so a change reads as a highlighted line.
        let width = area.width as usize;
        let (add_bg, del_bg) = self.diff_bg_colors();
        let lines: Vec<Line> = self
            .diff
            .iter()
            .map(|row| {
                let bg = match row.kind {
                    DiffKind::Add => Some(add_bg),
                    DiffKind::Del => Some(del_bg),
                    DiffKind::Context => None,
                };
                let mut spans: Vec<RSpan> = Vec::with_capacity(row.spans.len() + 1);
                let mut used = 0usize;
                for (color, text) in &row.spans {
                    used += text.chars().count();
                    let mut style = Style::default().fg(*color);
                    if let Some(c) = bg {
                        style = style.bg(c);
                    }
                    spans.push(RSpan::styled(text.clone(), style));
                }
                // Pad to the full width so the background covers the whole row.
                if let Some(c) = bg {
                    if used < width {
                        spans.push(RSpan::styled(
                            " ".repeat(width - used),
                            Style::default().bg(c),
                        ));
                    }
                }
                Line::from(spans)
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).scroll((self.diff_scroll, 0)),
            area,
        );
    }

    /// Background tints for added/deleted diff rows, honoring color depth.
    fn diff_bg_colors(&self) -> (Color, Color) {
        if self.truecolor {
            // Desaturated green / red that sit quietly under default-fg text.
            (Color::Rgb(22, 58, 33), Color::Rgb(74, 24, 28))
        } else {
            (Color::Indexed(22), Color::Indexed(52))
        }
    }

    fn draw_checks(&self, frame: &mut Frame, area: Rect) {
        let total: u32 = self
            .selected()
            .map(|s| {
                s.segments
                    .iter()
                    .flat_map(|seg| seg.commits.iter())
                    .map(|c| c.finding_counts.total())
                    .sum()
            })
            .unwrap_or(0);
        frame.render_widget(Paragraph::new(format!("⚠ {total} findings")), area);
    }
}

/// The shared selection highlight: a solid bar that repaints cleanly and does
/// not fight the per-row rainbow foreground (§8.3 — selection is a background,
/// so hue still carries identity).
fn highlight_style() -> Style {
    // Indexed(238) is a dark gray in the xterm-256 palette; unlike Rgb it also
    // renders on terminals without truecolor, so the selection bar is visible
    // everywhere.
    Style::default()
        .bg(Color::Indexed(238))
        .add_modifier(Modifier::BOLD)
}

/// Split a path into `(dir, filename)`. `dir` keeps a trailing component only
/// (no leading slash); it is empty for a top-level file.
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// A `-N +M` churn annotation: deleted count in red, added count in green.
/// Zero counts are suppressed (no `-0`/`+0`); an all-zero change yields no
/// spans at all. Rendered as owned spans so it can be dropped into any line.
fn stat_spans(added: u32, deleted: u32) -> Vec<RSpan<'static>> {
    let mut spans = Vec::new();
    if deleted > 0 {
        spans.push(RSpan::styled(
            format!("-{deleted}"),
            Style::default().fg(Color::Red),
        ));
    }
    if added > 0 {
        if !spans.is_empty() {
            spans.push(RSpan::raw(" "));
        }
        spans.push(RSpan::styled(
            format!("+{added}"),
            Style::default().fg(Color::Green),
        ));
    }
    spans
}

/// Rendered width (in cells) of the [`stat_spans`] annotation.
fn stat_width(added: u32, deleted: u32) -> usize {
    stat_spans(added, deleted)
        .iter()
        .map(|s| s.content.chars().count())
        .sum()
}

/// A run of `n` blank cells, used to right-justify trailing content.
fn spaces(n: usize) -> RSpan<'static> {
    RSpan::raw(" ".repeat(n))
}

/// A `w`×`h` rectangle centered within `area` (clamped to fit). Used for the
/// help/palette overlays.
fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Shorten `s` to at most `max` cells by dropping characters from the *front*
/// (keeping the tail) and prefixing an ellipsis — used for file directories so
/// the most specific path segment stays visible (§8.1).
fn truncate_front(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let keep = max - 1; // room for the leading ellipsis
    let tail: String = s.chars().skip(n - keep).collect();
    format!("…{tail}")
}

/// Color a name-status letter (green add / yellow modify / red delete / …).
fn status_color(status: char) -> Color {
    match status {
        'A' => Color::Green,
        'M' => Color::Yellow,
        'D' => Color::Red,
        'R' | 'C' => Color::Cyan,
        _ => Color::Gray,
    }
}

/// True for unified-diff rows that are hidden in the full-file view (git
/// headers, hunk markers, mode/rename lines). Kept in sync between the renderer
/// and the initial-scroll computation.
fn is_diff_meta(line: &str) -> bool {
    line.starts_with("diff ")
        || line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("@@")
        || line.starts_with("new file")
        || line.starts_with("deleted file")
        || line.starts_with("old mode")
        || line.starts_with("new mode")
        || line.starts_with("similarity ")
        || line.starts_with("rename ")
        || line.starts_with("copy ")
        || line.starts_with("\\ No newline")
}

/// True when screen point `(x, y)` lies inside `rect`.
fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// Step an index toward `last` (down) or `0` (up), saturating at the bounds.
fn step(cur: usize, down: bool, last: usize) -> usize {
    if down {
        (cur + 1).min(last)
    } else {
        cur.saturating_sub(1)
    }
}

fn commit_chips(c: &stacksaw_ssp::types::CommitSummary) -> String {
    let mut s = String::new();
    let fc = &c.finding_counts;
    if fc.total() == 0 {
        s.push_str(" ✓");
    } else {
        if fc.error > 0 {
            s.push_str(&format!(" ✗{}", fc.error));
        }
        if fc.warning > 0 {
            s.push_str(&format!(" ⚠{}", fc.warning));
        }
    }
    if !c.twins.is_empty() {
        s.push_str(" ⧉");
    }
    s
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Render the app to plain text lines using ratatui's `TestBackend` (§14).
pub fn render_to_lines(app: &App, width: u16, height: u16) -> Vec<String> {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut lines = Vec::new();
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            line.push_str(buffer[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::truncate_front;

    #[test]
    fn truncate_front_keeps_the_tail() {
        assert_eq!(truncate_front("short", 10), "short");
        assert_eq!(truncate_front("src/proto/wire", 8), "…to/wire");
        assert_eq!(truncate_front("src/proto/wire", 8).chars().count(), 8);
        assert_eq!(truncate_front("anything", 0), "");
    }
}
