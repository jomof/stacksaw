//! The TUI application: state + ratatui rendering of the column scene (§8).

use std::cell::RefCell;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;
use stacksaw_rainbox::{
    golden_angle_hue, staircase_arc_hue, Background, RainboxColor, Relevance, RelevanceSignals,
    StaircaseArc, Topological,
};
use stacksaw_ssp::types::{FileEntry, Snapshot, Staircase};

use crate::layout::{self, ColumnKind, LayoutPlan};

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
    /// Changed files of the currently selected commit (§8.1). Populated lazily
    /// by the host as the selection changes.
    pub files: Vec<FileEntry>,
    /// The commit oid whose files are currently loaded into `files`.
    loaded_oid: Option<String>,
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
            files: Vec::new(),
            loaded_oid: None,
            hit: RefCell::new(Hit::default()),
        }
    }

    fn selected(&self) -> Option<&Staircase> {
        self.snapshot.staircases.get(self.selected_stair)
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
    pub fn set_files(&mut self, oid: String, files: Vec<FileEntry>) {
        self.loaded_oid = Some(oid);
        self.files = files;
        self.selected_file = 0;
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
        match layout::plan(area.width, self.focused, self.zoom, self.checks_open) {
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
            .enumerate()
            .map(|(i, s)| {
                let selected = i == self.selected_stair;
                let hue = golden_angle_hue(&s.name);
                let color = to_ratatui(
                    RainboxColor::from_hue(hue),
                    if selected { 1.0 } else { 0.7 },
                    self.background,
                    selected,
                );
                let marker = if selected { '●' } else { '○' };
                let dirty = if s.dirty { " ✎" } else { "" };
                let name_style = if selected {
                    Style::default().fg(color).add_modifier(Modifier::REVERSED | Modifier::BOLD)
                } else {
                    Style::default().fg(color)
                };
                let line = Line::from(vec![
                    RSpan::styled(format!("{marker} "), Style::default().fg(color)),
                    RSpan::styled(s.name.clone(), name_style),
                    RSpan::raw(format!("  ↑{} ↓{}{}", s.ahead, s.behind, dirty)),
                ]);
                ListItem::new(line)
            })
            .collect();
        frame.render_widget(List::new(items), area);
    }

    fn draw_commits(&self, frame: &mut Frame, area: Rect) {
        let Some(stair) = self.selected() else {
            frame.render_widget(Paragraph::new("no staircase"), area);
            return;
        };
        let arc = StaircaseArc::default();
        let n = stair.segments.len().max(1);
        let mut lines: Vec<Line> = Vec::new();
        // Which commit index each line in `lines` renders (None = riser/decor).
        let mut owners: Vec<Option<usize>> = Vec::new();
        let mut commit_idx = 0usize;

        for (si, seg) in stair.segments.iter().enumerate() {
            let hue = staircase_arc_hue(arc, si, n);
            let indent = "  ".repeat(seg.parent.map_or(0, |_| si.min(6)));
            // Riser pill (§8.4).
            let riser_color = to_ratatui(RainboxColor::from_hue(hue), 0.9, self.background, false);
            lines.push(Line::from(vec![
                RSpan::raw(indent.clone()),
                RSpan::styled(format!("╭┴ {} ─", seg.branch), Style::default().fg(riser_color)),
            ]));
            owners.push(None);
            for c in &seg.commits {
                let selected = commit_idx == self.selected_commit;
                let rel = Relevance::compute(RelevanceSignals {
                    topological: if selected {
                        Topological::Focused
                    } else {
                        Topological::SameSegment
                    },
                    attention: c.finding_counts.total() > 0,
                    ..Default::default()
                });
                let color = to_ratatui(
                    RainboxColor::from_hue(hue),
                    rel.0,
                    self.background,
                    selected,
                );
                let chips = commit_chips(c);
                let marker = if selected { "▶ " } else { "  " };
                // A reversed bar makes the current commit unmistakable (§8.3:
                // selection is not conveyed by color alone).
                let base = Style::default().fg(color);
                let text_style = if selected {
                    base.add_modifier(Modifier::REVERSED | Modifier::BOLD)
                } else {
                    base
                };
                let card = Line::from(vec![
                    RSpan::styled(format!("{indent}{marker}"), text_style),
                    RSpan::styled(c.short.clone(), text_style),
                    RSpan::styled(format!(" {}", truncate(&c.subject, 40)), text_style),
                    RSpan::styled(chips, text_style),
                ]);
                lines.push(card);
                owners.push(Some(commit_idx));
                commit_idx += 1;
            }
        }
        // Record commit rows: the header occupies row 0, then `lines` follow.
        {
            let mut hit = self.hit.borrow_mut();
            for (j, owner) in owners.iter().enumerate() {
                let Some(ci) = owner else { continue };
                let ry = area.y + 1 + j as u16;
                if ry >= area.y + area.height {
                    break;
                }
                hit.commits.push((ry, *ci));
            }
        }
        let header = format!(
            "upstream {} ↑{} ↓{}",
            stair.upstream, stair.ahead, stair.behind
        );
        let mut all = vec![Line::from(RSpan::styled(
            header,
            Style::default().add_modifier(Modifier::DIM),
        ))];
        all.extend(lines);
        frame.render_widget(Paragraph::new(all), area);
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
            .enumerate()
            .map(|(i, f)| {
                let status = f.status.chars().next().unwrap_or('?');
                let color = status_color(status);
                let selected = i == self.selected_file;
                let marker = if selected { "▶ " } else { "  " };
                let path_style = if selected {
                    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    RSpan::styled(format!("{marker}{status} "), Style::default().fg(color)),
                    RSpan::styled(f.path.clone(), path_style),
                ]))
            })
            .collect();
        frame.render_widget(List::new(items), area);
    }

    fn draw_diff(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new("(unified/side-by-side diff — `s` toggles, `I` interdiff)"),
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

fn to_ratatui(color: RainboxColor, relevance: f32, bg: Background, selected: bool) -> Color {
    let resolved = if selected {
        color.selected()
    } else {
        color.dimmed(relevance, bg)
    };
    let (r, g, b) = resolved.to_rgb();
    Color::Rgb(r, g, b)
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
