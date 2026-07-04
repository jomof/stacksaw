//! The TUI application: state + ratatui rendering of the column scene (§8).

use std::cell::RefCell;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use stacksaw_rainbox::{
    golden_angle_hue, staircase_arc_hue, Background, RainboxColor, StaircaseArc,
};
use stacksaw_ssp::types::{FileEntry, Snapshot, Staircase};

use crate::layout::{self, ColumnKind, LayoutPlan};

/// Status marker identifying the virtual "commit message" row in the Files
/// column (an envelope glyph, distinct from git's A/M/D/R status letters).
const MESSAGE_STATUS: &str = "✉";
/// Display label for the virtual commit-message row.
const MESSAGE_PATH: &str = "commit message";

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
    /// Diff text (or raw file content, for added files) of the selected file
    /// (§8.5). Loaded lazily.
    diff: Vec<String>,
    /// True when `diff` holds raw file content (added file) rather than a patch,
    /// so lines are rendered plainly without +/- coloring.
    diff_is_raw: bool,
    /// `(commit oid, file path)` currently loaded into `diff`.
    loaded_diff_key: Option<(String, String)>,
    /// Vertical scroll offset into the diff view.
    diff_scroll: u16,
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
    /// the text as plain file content (added file) rather than a unified patch.
    pub fn set_diff(&mut self, oid: String, path: String, text: &str, raw: bool) {
        self.diff = text.lines().map(str::to_string).collect();
        self.diff_is_raw = raw;
        self.loaded_diff_key = Some((oid, path));
        self.diff_scroll = 0;
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

    /// Draw the full scene for the current terminal size.
    pub fn draw(&self, frame: &mut Frame) {
        {
            let mut hit = self.hit.borrow_mut();
            hit.columns.clear();
            hit.stacks.clear();
            hit.commits.clear();
            hit.files.clear();
        }
        let area = frame.area();
        match layout::plan(
            area.width,
            self.focused,
            self.zoom,
            self.checks_open,
            Some(self.stacks_content_width()),
        ) {
            LayoutPlan::Deck { focused } => self.draw_deck(frame, area, focused),
            LayoutPlan::Columns(slots) => {
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
                let hue = staircase_arc_hue(arc, commit_idx, total);
                let color = self.hue_to_color(hue);
                let chips = commit_chips(c);
                // Fit the subject to whatever width the column actually has,
                // rather than a fixed cap, so a wide Commits column shows more
                // of the message. Reserve the highlight marker, indent, hash,
                // separating space, and the trailing findings chips.
                const MARKER: usize = 2; // "▶ " highlight symbol
                let used = MARKER
                    + indent.chars().count()
                    + c.short.chars().count()
                    + 1
                    + chips.chars().count();
                let budget = (list_area.width as usize).saturating_sub(used).max(8);
                commit_line.push(items.len());
                items.push(ListItem::new(Line::from(vec![
                    RSpan::styled(format!("{indent}"), Style::default()),
                    RSpan::styled(c.short.clone(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    RSpan::styled(format!(" {}", truncate(&c.subject, budget)), Style::default().fg(color)),
                    RSpan::styled(chips, Style::default().fg(color)),
                ])));
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
                let mut spans = vec![
                    RSpan::styled(format!("{status} "), Style::default().fg(status_color(status))),
                    RSpan::styled(name.to_string(), Style::default().fg(name_color).add_modifier(Modifier::BOLD)),
                ];
                if !dir.is_empty() {
                    spans.push(RSpan::styled(
                        format!("  {dir}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
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
        let raw = self.diff_is_raw;
        let lines: Vec<Line> = self
            .diff
            .iter()
            .map(|l| {
                let style = if raw {
                    Style::default()
                } else {
                    diff_line_style(l)
                };
                Line::from(RSpan::styled(l.clone(), style))
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).scroll((self.diff_scroll, 0)),
            area,
        );
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

/// Color a unified-diff line: green add / red delete / cyan hunk header /
/// bold file header (§8.5). Context lines are left unstyled.
fn diff_line_style(line: &str) -> Style {
    if line.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else if line.starts_with("diff ")
        || line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
    {
        Style::default().add_modifier(Modifier::BOLD)
    } else if line.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    }
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
