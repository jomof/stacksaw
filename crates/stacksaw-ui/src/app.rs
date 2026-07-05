//! The TUI application: state + ratatui rendering of the column scene (§8).

use std::cell::RefCell;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use stacksaw_rainbox::Background;
use stacksaw_ssp::types::{CommitSummary, FileEntry, Snapshot, Staircase, WORKTREE_OID};

use crate::command::{self, Action, Command};
use crate::highlight::Highlighter;
use crate::layout::{self, ColumnKind};
use crate::theme::{ChipKind, Ctx, RainbowInput, Theme};

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
    /// The resolved UI theme (§8.3): colors, glyphs, and modifiers, loaded once
    /// from the embedded `theme.toml`.
    theme: Theme,
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
            theme: Theme::load(),
        }
    }

    fn selected(&self) -> Option<&Staircase> {
        self.snapshot.staircases.get(self.selected_stair)
    }

    /// The current render context (color depth + perceptual background), which
    /// the theme needs to lower styles to concrete terminal colors.
    fn ctx(&self) -> Ctx {
        Ctx {
            truecolor: self.truecolor,
            background: self.background,
        }
    }

    /// The `↑a ↓b` ahead/behind counters, glyphs sourced from the theme so the
    /// same marks appear here and in the legend. Shared by the width estimate
    /// and the rendered row.
    fn counters_text(&self, ahead: u32, behind: u32) -> String {
        format!(
            "  {}{ahead} {}{behind}",
            self.theme.glyph("ahead"),
            self.theme.glyph("behind"),
        )
    }

    /// The `-N +M` churn annotation as themed spans (deletions then additions;
    /// zero halves suppressed), ready to drop into any line.
    fn churn_spans(&self, added: u32, deleted: u32) -> Vec<RSpan<'static>> {
        let ctx = self.ctx();
        let mut spans = Vec::new();
        if deleted > 0 {
            spans.push(RSpan::styled(
                format!("-{deleted}"),
                self.theme.style("churn_deleted", ctx, RainbowInput::None),
            ));
        }
        if added > 0 {
            if !spans.is_empty() {
                spans.push(RSpan::raw(" "));
            }
            spans.push(RSpan::styled(
                format!("+{added}"),
                self.theme.style("churn_added", ctx, RainbowInput::None),
            ));
        }
        spans
    }

    /// The status chips for a commit as themed spans, plus their rendered width.
    /// Each chip is a semantic mark (§8.3): clean, or error/warning counts, and
    /// a twin flag; colored by role, not by the commit's rainbow hue.
    fn chip_spans(&self, c: &CommitSummary) -> (Vec<RSpan<'static>>, usize) {
        let ctx = self.ctx();
        let mut spans = Vec::new();
        let mut width = 0usize;
        for (kind, count) in chip_specs(c) {
            let (glyph, style) = self.theme.chip(kind, ctx);
            let text = match count {
                Some(n) => format!(" {glyph}{n}"),
                None => format!(" {glyph}"),
            };
            width += text.chars().count();
            spans.push(RSpan::styled(text, style));
        }
        (spans, width)
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
        let mut hl = Highlighter::for_path(&path, self.truecolor, self.theme.syntax_theme());
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
        for (body, row) in self.diff.iter().enumerate() {
            if row.kind != DiffKind::Context {
                return (body as u16).saturating_sub(context);
            }
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
                self.draw_column(frame, bottom, ColumnKind::Diff, true);
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
        let ctx = self.ctx();
        let mut spans: Vec<RSpan> = Vec::new();
        for (i, cmd) in command::hint_commands(self.focused).iter().enumerate() {
            if i > 0 {
                spans.push(RSpan::styled(
                    format!(" {} ", self.theme.glyph("hint_separator")),
                    self.theme.style("hint_separator", ctx, RainbowInput::None),
                ));
            }
            spans.push(RSpan::styled(
                cmd.primary_key_label(),
                self.theme.style("hint_key", ctx, RainbowInput::None),
            ));
            spans.push(RSpan::raw(" "));
            spans.push(RSpan::styled(
                cmd.title,
                self.theme.style("hint_label", ctx, RainbowInput::None),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// The `?` help overlay: every command grouped by category (§8.2).
    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
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
                self.theme.style("help_heading", ctx, RainbowInput::None),
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
                        self.theme.style("help_key", ctx, RainbowInput::None),
                    ),
                    RSpan::raw(" "),
                    RSpan::raw(cmd.title),
                ]));
            }
            lines.push(Line::from(""));
        }
        lines.push(Line::from(RSpan::styled(
            "any key to close",
            self.theme.style("help_footer", ctx, RainbowInput::None),
        )));

        let popup = centered_rect(48, (lines.len() as u16 + 2).min(area.height), area);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Help — keys")
            .border_style(self.theme.style("overlay_frame", ctx, RainbowInput::None));
        frame.render_widget(Paragraph::new(lines).block(block), popup);
    }

    /// The `:` command palette: a fuzzy-filtered list of every command, each
    /// showing its key so the palette teaches shortcuts (§8.2).
    fn draw_palette(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
        let results = self.palette_results();
        let popup = centered_rect(52, 16.min(area.height), area);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Command palette")
            .border_style(self.theme.style("overlay_frame", ctx, RainbowInput::None));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        // Query line.
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                RSpan::styled(
                    self.theme.glyph("palette_prompt").to_string(),
                    self.theme.style("palette_prompt", ctx, RainbowInput::None),
                ),
                RSpan::raw(self.palette.query.clone()),
                RSpan::styled(
                    self.theme.glyph("palette_cursor").to_string(),
                    self.theme.style("palette_cursor", ctx, RainbowInput::None),
                ),
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
                    RSpan::styled(key, self.theme.style("palette_key", ctx, RainbowInput::None)),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        if !results.is_empty() {
            state.select(Some(self.palette.selected.min(results.len() - 1)));
        }
        let list = List::new(items)
            .highlight_style(self.theme.selection_style(self.ctx()))
            .highlight_symbol(self.theme.selection_symbol());
        frame.render_stateful_widget(list, rows[1], &mut state);
    }

    /// Split the frame into the top column band and the bottom Diff pane.
    ///
    /// Zooming the Diff column gives it the whole frame. Zooming a *top* column
    /// keeps the normal split so the Diff pane stays visible — the zoom just
    /// collapses that column's siblings to spines inside the top band (handled
    /// in `draw_top_columns`), giving the focused column the band's full width.
    fn split_scene(&self, area: Rect) -> (Rect, Rect) {
        let empty = Rect { height: 0, ..area };
        if self.zoom && self.focused == ColumnKind::Diff {
            return (empty, area);
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
        // A spine is a 3-cell box (border+letter+border) only when it stands
        // alone. When it shares its left divider with a neighbor (every slot but
        // the first) it needs just 2 cells: letter + right divider. Reclaim the
        // saved cells and hand them to the expanded column so the band still
        // fills `area` exactly with no trailing blank.
        let mut widths: Vec<u16> = slots
            .iter()
            .enumerate()
            .map(|(i, s)| match s.width {
                Some(w) => w,
                None if i == 0 => layout::SPINE_WIDTH,
                None => layout::SPINE_WIDTH - 1,
            })
            .collect();
        let used: u16 = widths.iter().sum();
        let reclaimed = area.width.saturating_sub(used);
        if reclaimed > 0 {
            // Give the surplus to the widest expanded slot (ties → last).
            if let Some(idx) = slots
                .iter()
                .enumerate()
                .filter(|(_, s)| s.width.is_some())
                .max_by_key(|(i, s)| (s.width.unwrap(), *i))
                .map(|(i, _)| i)
            {
                widths[idx] += reclaimed;
            }
        }
        let constraints: Vec<Constraint> =
            widths.iter().map(|w| Constraint::Length(*w)).collect();
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);
        for (i, (slot, rect)) in slots.iter().zip(chunks.iter()).enumerate() {
            // Only the first column/spine draws a left border; every other one
            // reuses its left neighbor's right border as the shared divider, so
            // adjacent columns show a single line rather than a doubled one.
            let first = i == 0;
            match slot.width {
                Some(_) => self.draw_column(frame, *rect, slot.kind, first),
                None => self.draw_spine(frame, *rect, slot.kind, first),
            }
        }
        // Blocks don't merge borders, so each internal divider meets the band's
        // top/bottom edges as `┐─`/`┘─` rather than a connected tee. Stitch
        // those junctions into `┬`/`┴` for clean elbows (the user-visible fix).
        stitch_dividers(frame, area, &chunks);
    }

    fn draw_deck(&self, frame: &mut Frame, area: Rect, focused: ColumnKind) {
        let crumb = self.breadcrumb(focused);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);
        frame.render_widget(
            Paragraph::new(crumb)
                .style(self.theme.style("breadcrumb", self.ctx(), RainbowInput::None)),
            rows[0],
        );
        self.draw_column(frame, rows[1], focused, true);
    }

    fn breadcrumb(&self, focused: ColumnKind) -> String {
        let stair = self.selected().map(|s| s.name.as_str()).unwrap_or("—");
        let sep = self.theme.glyph("breadcrumb");
        format!("Stacks {sep} {stair} {sep} {}", focused.title())
    }

    /// Draw a collapsed column as a vertical "spine". Every slot carries top and
    /// bottom borders so the band reads as one continuous frame; `left_border`
    /// additionally closes the band's left edge for the first slot. The shared
    /// divider with the neighbor to the right is this spine's right border, with
    /// its junctions stitched into tees by [`stitch_dividers`].
    fn draw_spine(&self, frame: &mut Frame, area: Rect, kind: ColumnKind, left_border: bool) {
        self.hit.borrow_mut().columns.push((kind, area));
        let mut borders = Borders::TOP | Borders::BOTTOM | Borders::RIGHT;
        if left_border {
            borders |= Borders::LEFT;
        }
        // Rotated title + identity strip (§8.1), inside the top/bottom border.
        // The letters carry the focus highlight (the border stays gray).
        let focused = kind == self.focused;
        let style = self.theme.column_title_style(focused, self.ctx());
        let inner_h = area.height.saturating_sub(2) as usize;
        let title: String = kind.title().chars().take(inner_h).collect();
        let vertical: Vec<Line> = title
            .chars()
            .map(|c| Line::from(RSpan::styled(c.to_string(), style)))
            .collect();
        let block = Block::default()
            .borders(borders)
            .border_style(self.theme.style("column_border", self.ctx(), RainbowInput::None));
        let inner = block.inner(area);
        frame.render_widget(Paragraph::new(vertical).block(block), area);
        set_window_intensity(frame, inner, self.theme.content_overlay(focused));
    }

    /// Draw an expanded column. `left_border` is `false` for columns that abut a
    /// neighbor on their left (in the top band) so the shared divider is a
    /// single line; standalone columns (Diff pane, deck mode) pass `true`.
    fn draw_column(&self, frame: &mut Frame, area: Rect, kind: ColumnKind, left_border: bool) {
        self.hit.borrow_mut().columns.push((kind, area));
        let focused = kind == self.focused;
        // The border stays a calm gray; focus is signalled by highlighting the
        // column's title word instead of the whole box.
        let borders = if left_border {
            Borders::ALL
        } else {
            Borders::TOP | Borders::BOTTOM | Borders::RIGHT
        };
        let block = Block::default()
            .borders(borders)
            .title(kind.title())
            .border_style(self.theme.style("column_border", self.ctx(), RainbowInput::None))
            .title_style(self.theme.column_title_style(focused, self.ctx()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        // A glyph key on the bottom inner row (when present) explains the
        // symbols shown in this column; the body renders above it.
        let body = self.draw_legend(frame, inner, kind);

        match kind {
            ColumnKind::Stacks => self.draw_stacks(frame, body),
            ColumnKind::Commits => self.draw_commits(frame, body),
            ColumnKind::Files => self.draw_files(frame, body),
            ColumnKind::Diff => self.draw_diff(frame, body),
            ColumnKind::Checks => self.draw_checks(frame, body),
        }
        // Window-level intensity: the focused column reads at normal weight,
        // unfocused columns recede (dimmed). Applied over the whole inner area
        // so it covers content + legend regardless of how each is styled.
        set_window_intensity(frame, inner, self.theme.content_overlay(focused));
    }

    /// Outer width the Stacks column needs to show its widest row without
    /// truncation: highlight marker + name + the `↑a ↓b` counters + borders,
    /// and wide enough that the glyph legend on the bottom row fits too.
    fn stacks_content_width(&self) -> u16 {
        const MARKER: usize = 2; // "▶ "
        const BORDERS: usize = 2; // left + right column borders
        let content = self
            .snapshot
            .staircases
            .iter()
            .map(|s| {
                // " " + dirty glyph when the stack has uncommitted changes.
                let dirty = if s.dirty {
                    1 + self.theme.glyph("dirty").chars().count()
                } else {
                    0
                };
                let counters = self.counters_text(s.ahead, s.behind).chars().count();
                s.name.chars().count() + counters + dirty
            })
            .max()
            .unwrap_or(0);
        // Ensure the "Stacks" title still fits in the border.
        let title = "Stacks".len();
        // The legend row has no marker; it just needs the inner width.
        let legend = legend_width(&self.column_legend(ColumnKind::Stacks));
        let inner = (MARKER + content.max(title)).max(legend);
        (inner + BORDERS) as u16
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
                let ctx = self.ctx();
                // Each staircase keeps its own identity hue (§8.3), sourced from
                // its name via the `stack` identity.
                let mut spans = vec![
                    RSpan::styled(
                        s.name.clone(),
                        self.theme.style("stack_name", ctx, RainbowInput::Key(&s.name)),
                    ),
                    RSpan::styled(
                        self.counters_text(s.ahead, s.behind),
                        self.theme.style("stack_counters", ctx, RainbowInput::None),
                    ),
                ];
                // The dirty marker reuses the editorial `dirty` role (glyph +
                // color) so it reads the same on a stack and in the legend.
                if s.dirty {
                    spans.push(RSpan::styled(
                        format!(" {}", self.theme.glyph("dirty")),
                        self.theme.style("dirty", ctx, RainbowInput::None),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.selected_stair));
        let list = List::new(items)
            .highlight_style(self.theme.selection_style(self.ctx()))
            .highlight_symbol(self.theme.selection_symbol());
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
        let ctx = self.ctx();
        let header = format!(
            "upstream {} {}{} {}{}",
            stair.upstream,
            self.theme.glyph("ahead"),
            stair.ahead,
            self.theme.glyph("behind"),
            stair.behind,
        );
        frame.render_widget(
            Paragraph::new(header).style(self.theme.style("commit_header", ctx, RainbowInput::None)),
            rows[0],
        );
        let list_area = rows[1];

        // Rainbow the commits across the whole stack: each commit gets its own
        // step along the staircase arc (§8.3), not one hue per segment.
        let total = stair.segments.iter().map(|s| s.commits.len()).sum::<usize>();
        let mut items: Vec<ListItem> = Vec::new();
        // Line index (within the list) of each commit, for hit-testing + state.
        let mut commit_line: Vec<usize> = Vec::new();
        let mut commit_idx = 0usize;

        for (si, seg) in stair.segments.iter().enumerate() {
            let indent = "  ".repeat(seg.parent.map_or(0, |_| si.min(6)));
            let riser_pos = RainbowInput::Position {
                index: commit_idx.min(total.saturating_sub(1)),
                total,
            };
            items.push(ListItem::new(Line::from(vec![
                RSpan::raw(indent.clone()),
                RSpan::styled(
                    format!(
                        "{} {} {}",
                        self.theme.lead("segment_riser"),
                        seg.branch,
                        self.theme.trail("segment_riser"),
                    ),
                    self.theme.style("segment_riser", ctx, riser_pos),
                ),
            ])));
            for c in &seg.commits {
                const MARKER: usize = 2; // "▶ " highlight symbol
                let content_w = (list_area.width as usize).saturating_sub(MARKER);
                // The virtual worktree commit renders distinctly (§8.3): a pencil
                // glyph + label (the `commit_worktree` role), churn right-aligned.
                if c.oid == WORKTREE_OID {
                    let label = format!("{} Uncommitted changes", self.theme.glyph("commit_worktree"));
                    let churn_w = stat_width(c.added, c.deleted);
                    let pad = content_w
                        .saturating_sub(label.chars().count() + churn_w)
                        .max(1);
                    commit_line.push(items.len());
                    let mut spans = vec![
                        RSpan::styled(
                            label,
                            self.theme.style("commit_worktree", ctx, RainbowInput::None),
                        ),
                        spaces(pad),
                    ];
                    spans.extend(self.churn_spans(c.added, c.deleted));
                    items.push(ListItem::new(Line::from(spans)));
                    commit_idx += 1;
                    continue;
                }
                let pos = RainbowInput::Position { index: commit_idx, total };
                let (chip_spans, chips_w) = self.chip_spans(c);
                let churn_w = stat_width(c.added, c.deleted);
                // The `-N +M` churn is right-justified against the column edge;
                // the subject fills the space in between, truncated (from the
                // back) only when it would otherwise collide with the churn.
                // Reserve the highlight marker, indent, hash, chips, and churn.
                let indent_w = indent.chars().count();
                let short_w = c.short.chars().count();
                let fixed = indent_w + short_w + 1 + chips_w + churn_w;
                let budget = content_w.saturating_sub(fixed + 1).max(8);
                let subject = truncate(&c.subject, budget);
                let used_left = indent_w + short_w + 1 + subject.chars().count() + chips_w;
                let pad = content_w.saturating_sub(used_left + churn_w).max(1);
                commit_line.push(items.len());
                let mut spans = vec![
                    RSpan::raw(indent.to_string()),
                    // Identity hue is carried by the hash and chips; the subject
                    // stays the default foreground.
                    RSpan::styled(c.short.clone(), self.theme.style("commit_hash", ctx, pos)),
                    RSpan::styled(
                        format!(" {subject}"),
                        self.theme.style("commit_subject", ctx, RainbowInput::None),
                    ),
                ];
                spans.extend(chip_spans);
                spans.push(spaces(pad));
                spans.extend(self.churn_spans(c.added, c.deleted));
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
            .highlight_style(self.theme.selection_style(ctx))
            .highlight_symbol(self.theme.selection_symbol());
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
                Paragraph::new(msg)
                    .style(self.theme.style("diff_placeholder", self.ctx(), RainbowInput::None)),
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
                let ctx = self.ctx();
                // The pinned commit-message row renders as a labelled envelope,
                // not a path (no directory split, no rainbow-by-folder).
                if f.status == MESSAGE_STATUS {
                    return ListItem::new(Line::from(vec![
                        RSpan::styled(
                            format!("{} ", self.theme.glyph("file_message_glyph")),
                            self.theme.style("file_message_glyph", ctx, RainbowInput::None),
                        ),
                        RSpan::styled(
                            f.path.clone(),
                            self.theme.style("file_message_path", ctx, RainbowInput::None),
                        ),
                    ]));
                }
                let status = f.status.chars().next().unwrap_or('?');
                let (dir, name) = split_path(&f.path);
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
                    RSpan::styled(
                        format!("{status} "),
                        self.theme.file_status_style(status, ctx),
                    ),
                    // Filename first (never hidden), hued by its directory so
                    // files in the same folder share a hue (§8.3).
                    RSpan::styled(
                        name.to_string(),
                        self.theme.style("file_name", ctx, RainbowInput::Key(dir)),
                    ),
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
                            self.theme.style("file_dir", ctx, RainbowInput::None),
                        ));
                    }
                }
                let pad = content_w.saturating_sub(used_left + churn_w).max(1);
                spans.push(spaces(pad));
                spans.extend(self.churn_spans(f.added, f.deleted));
                ListItem::new(Line::from(spans))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.selected_file));
        let list = List::new(items)
            .highlight_style(self.theme.selection_style(self.ctx()))
            .highlight_symbol(self.theme.selection_symbol());
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
                Paragraph::new(msg)
                    .style(self.theme.style("diff_placeholder", self.ctx(), RainbowInput::None)),
                area,
            );
            return;
        }
        // Every row is syntax-highlighted (cached at load). Modified-file rows
        // additionally get a full-width tinted background — green for additions,
        // red for deletions — so a change reads as a highlighted line.
        let width = area.width as usize;
        let ctx = self.ctx();
        let add_bg = self.theme.diff_bg(true, ctx);
        let del_bg = self.theme.diff_bg(false, ctx);
        let lines: Vec<Line> = self
            .diff
            .iter()
            .map(|row| {
                let bg = match row.kind {
                    DiffKind::Add => add_bg,
                    DiffKind::Del => del_bg,
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

    /// A legend of the glyphs *currently* displayed in `kind`'s column, so the
    /// symbols are self-explanatory. Only glyphs actually present in the current
    /// view are listed; returns an empty vec when there is nothing to explain.
    fn column_legend(&self, kind: ColumnKind) -> Vec<RSpan<'static>> {
        let ctx = self.ctx();
        let secondary = self.theme.style("secondary", ctx, RainbowInput::None);
        let mut entries: Vec<Vec<RSpan<'static>>> = Vec::new();
        // A chip key: its glyph in the chip color, its word in the label style.
        let chip = |kind: ChipKind, label: &str| {
            let (glyph, style) = self.theme.chip(kind, ctx);
            self.legend_entry(&glyph, style, label)
        };
        match kind {
            ColumnKind::Stacks => {
                entries.push(self.legend_entry(self.theme.glyph("ahead"), secondary, "ahead"));
                entries.push(self.legend_entry(self.theme.glyph("behind"), secondary, "behind"));
                if self.snapshot.staircases.iter().any(|s| s.dirty) {
                    entries.push(self.legend_entry(
                        self.theme.glyph("dirty"),
                        self.theme.style("dirty", ctx, RainbowInput::None),
                        "uncommitted",
                    ));
                }
            }
            ColumnKind::Commits => {
                let Some(stair) = self.selected() else {
                    return Vec::new();
                };
                let commits: Vec<_> = stair
                    .segments
                    .iter()
                    .flat_map(|s| s.commits.iter())
                    .collect();
                if !stair.segments.is_empty() {
                    entries.push(self.legend_entry(self.theme.lead("segment_riser"), secondary, "branch"));
                }
                if commits
                    .iter()
                    .any(|c| c.oid != WORKTREE_OID && c.finding_counts.total() == 0)
                {
                    entries.push(chip(ChipKind::Clean, "clean"));
                }
                if commits.iter().any(|c| c.finding_counts.error > 0) {
                    entries.push(chip(ChipKind::Error, "errors"));
                }
                if commits.iter().any(|c| c.finding_counts.warning > 0) {
                    entries.push(chip(ChipKind::Warning, "warnings"));
                }
                if commits.iter().any(|c| !c.twins.is_empty()) {
                    entries.push(chip(ChipKind::Twin, "twin"));
                }
                if commits.iter().any(|c| c.oid == WORKTREE_OID) {
                    entries.push(self.legend_entry(
                        self.theme.glyph("dirty"),
                        self.theme.style("dirty", ctx, RainbowInput::None),
                        "uncommitted",
                    ));
                }
                if commits.iter().any(|c| c.added > 0 || c.deleted > 0) {
                    entries.push(self.churn_legend_entry());
                }
            }
            ColumnKind::Files => {
                if self.files.is_empty() {
                    return Vec::new();
                }
                if self.files.iter().any(|f| f.status == MESSAGE_STATUS) {
                    entries.push(self.legend_entry(
                        self.theme.glyph("file_message_glyph"),
                        secondary,
                        "message",
                    ));
                }
                for (ch, label) in [
                    ('A', "added"),
                    ('M', "modified"),
                    ('D', "deleted"),
                    ('R', "renamed"),
                    ('C', "copied"),
                ] {
                    if self.files.iter().any(|f| {
                        f.status != MESSAGE_STATUS && f.status.starts_with(ch)
                    }) {
                        entries.push(self.legend_entry(
                            &ch.to_string(),
                            self.theme.file_status_style(ch, ctx),
                            label,
                        ));
                    }
                }
                if self.files.iter().any(|f| f.added > 0 || f.deleted > 0) {
                    entries.push(self.churn_legend_entry());
                }
            }
            // The Diff and Checks columns carry no glyph vocabulary worth a key.
            ColumnKind::Diff | ColumnKind::Checks => return Vec::new(),
        }
        join_legend(entries)
    }

    /// One legend item: a styled `glyph` followed by the label in the theme's
    /// legend-label style.
    fn legend_entry(&self, glyph: &str, glyph_style: Style, label: &str) -> Vec<RSpan<'static>> {
        vec![
            RSpan::styled(glyph.to_string(), glyph_style),
            RSpan::styled(
                format!(" {label}"),
                self.theme.style("legend_label", self.ctx(), RainbowInput::None),
            ),
        ]
    }

    /// The churn key: themed `-` / `+` marks with a shared "lines" label.
    fn churn_legend_entry(&self) -> Vec<RSpan<'static>> {
        let ctx = self.ctx();
        let label = self.theme.style("legend_label", ctx, RainbowInput::None);
        vec![
            RSpan::styled("-", self.theme.style("churn_deleted", ctx, RainbowInput::None)),
            RSpan::styled("/", label),
            RSpan::styled("+", self.theme.style("churn_added", ctx, RainbowInput::None)),
            RSpan::styled(" lines", label),
        ]
    }

    /// Reserve the bottom inner row for [`column_legend`] when it has content and
    /// there is room; returns the (possibly shortened) area left for the body.
    fn draw_legend(&self, frame: &mut Frame, inner: Rect, kind: ColumnKind) -> Rect {
        let legend = self.column_legend(kind);
        if legend.is_empty() || inner.height < 3 {
            return inner;
        }
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        frame.render_widget(Paragraph::new(Line::from(legend)), rows[1]);
        rows[0]
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
        frame.render_widget(
            Paragraph::new(format!("{} {total} findings", self.theme.glyph("checks_summary")))
                .style(self.theme.style("checks_summary", self.ctx(), RainbowInput::None)),
            area,
        );
    }
}

/// Split a path into `(dir, filename)`. `dir` keeps a trailing component only
/// (no leading slash); it is empty for a top-level file.
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Rendered width (in cells) of the `-N +M` churn annotation, matching the
/// spans built by [`App::churn_spans`]: `-{deleted}` and `+{added}`, each
/// suppressed when zero, joined by a single space when both appear.
fn stat_width(added: u32, deleted: u32) -> usize {
    let mut w = 0;
    if deleted > 0 {
        w += 1 + deleted.to_string().len();
    }
    if added > 0 {
        if w > 0 {
            w += 1; // the joining space
        }
        w += 1 + added.to_string().len();
    }
    w
}

/// A run of `n` blank cells, used to right-justify trailing content.
fn spaces(n: usize) -> RSpan<'static> {
    RSpan::raw(" ".repeat(n))
}

/// Apply the window-level intensity `overlay` to every cell in `area`: an
/// unfocused column receives the theme's dim overlay so the eye lands on the
/// active window, while a focused column receives an empty (no-op) patch. Cell
/// fg/bg and any intentional intra-window dimming are preserved.
fn set_window_intensity(frame: &mut Frame, area: Rect, overlay: Style) {
    let style = overlay;
    let buf = frame.buffer_mut();
    let bounds = buf.area;
    let x0 = area.x.max(bounds.x);
    let y0 = area.y.max(bounds.y);
    let x1 = (area.x + area.width).min(bounds.x + bounds.width);
    let y1 = (area.y + area.height).min(bounds.y + bounds.height);
    for y in y0..y1 {
        for x in x0..x1 {
            buf[(x, y)].set_style(style);
        }
    }
}

/// Rendered width (in cells) of a legend span run.
fn legend_width(spans: &[RSpan<'static>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// Flatten legend entries into a single span run, separated by two spaces.
fn join_legend(entries: Vec<Vec<RSpan<'static>>>) -> Vec<RSpan<'static>> {
    let mut out = Vec::new();
    for (i, entry) in entries.into_iter().enumerate() {
        if i > 0 {
            out.push(RSpan::raw("  "));
        }
        out.extend(entry);
    }
    out
}

/// Stitch the internal column dividers into the band's top/bottom border so
/// each boundary reads as a connected `┬`/`┴` tee instead of two separate block
/// corners. `chunks` are the per-slot rects, left→right; the divider for every
/// slot but the last sits on that slot's right-border column.
fn stitch_dividers(frame: &mut Frame, area: Rect, chunks: &[Rect]) {
    if area.height < 2 || chunks.len() < 2 {
        return;
    }
    let top = area.y;
    let bottom = area.y + area.height - 1;
    let buf = frame.buffer_mut();
    // Every chunk except the last contributes a divider at its right edge.
    for chunk in &chunks[..chunks.len() - 1] {
        if chunk.width == 0 {
            continue;
        }
        let x = chunk.x + chunk.width - 1;
        if x < area.x || x >= area.x + area.width {
            continue;
        }
        // Preserve each cell's style (e.g. a focused column's border color);
        // only correct the glyph.
        buf[(x, top)].set_symbol("┬");
        buf[(x, bottom)].set_symbol("┴");
    }
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

/// The status chips a commit should show (§8.3), each with an optional trailing
/// count: a `Clean` tick when there are no findings, else `Error`/`Warning`
/// counts, plus a `Twin` flag when the commit has twins. Glyphs and colors are
/// supplied by the theme; this only decides *which* chips appear.
fn chip_specs(c: &CommitSummary) -> Vec<(ChipKind, Option<u32>)> {
    let fc = &c.finding_counts;
    let mut specs = Vec::new();
    if fc.total() == 0 {
        specs.push((ChipKind::Clean, None));
    } else {
        if fc.error > 0 {
            specs.push((ChipKind::Error, Some(fc.error)));
        }
        if fc.warning > 0 {
            specs.push((ChipKind::Warning, Some(fc.warning)));
        }
    }
    if !c.twins.is_empty() {
        specs.push((ChipKind::Twin, None));
    }
    specs
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
