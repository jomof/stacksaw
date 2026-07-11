use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{
    Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph,
};
use ratatui::Frame;
use stacksaw_rainbox::temporal_decay;
use stacksaw_ssp::types::{FileStatus, RebaseStatus, Staircase, WORKTREE_OID};
use std::collections::HashMap;

use super::App;
use super::{truncate, truncate_front};
use super::{Divider, Mode, RecentRowView, RunButton};
use super::{
    DEFAULT_SPLIT_FRACTION, MIN_PANE_HEIGHT, RECENTS_HALF_LIFE, RECENTS_MAX_BRANCH,
    RECENTS_RELEVANCE_FLOOR,
};
use crate::command::{self, Command};
use crate::layout::{self, ColumnKind};
use crate::theme::{ChipKind, RainbowInput};
use crate::viewport::{DiffKind, RunView, Tab, TabStatus};

impl App {
    pub fn draw(&self, frame: &mut Frame) {
        {
            let mut hit = self.hit.borrow_mut();
            hit.columns.clear();
            hit.stacks.clear();
            hit.recents.clear();
            hit.commits.clear();
            hit.files.clear();
            hit.viewport_tabs.clear();
            hit.viewport_closes.clear();
            hit.viewport_badges.clear();
            hit.viewport_run_buttons.clear();
            hit.dividers.clear();
        }
        let full = frame.area();
        // Paint the scene background first (theme `[base].bg`); widgets that set
        // no bg of their own then show through to it. Skipped when the theme
        // defers to the terminal's own background.
        if let Some(bg) = self.theme.background(self.ctx()) {
            frame.render_widget(Block::default().style(Style::default().bg(bg)), full);
        }
        // Reserve the bottom row for the always-on hint bar (§8.2).
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(full);
        let area = rows[0];
        // Narrow terminals stay in single-column deck mode (§8.1).
        if area.width < layout::DECK_MODE_COLS {
            self.draw_deck(frame, area, self.nav.focused);
            self.paint_hovered_row(frame);
        } else {
            // Wide layout: the master columns (Stacks | Commits | Files
            // [| Checks]) sit in a top band, with the viewport pane full-width
            // below them so source code has room to breathe.
            self.hit.borrow_mut().scene = area;
            let (top, bottom) = self.split_scene(area);
            if top.height > 0 {
                self.draw_top_columns(frame, top);
            }
            if bottom.height > 0 {
                self.draw_viewport(frame, bottom);
                // The top band's bottom border doubles as the draggable split
                // line (only offered when a viewport pane is actually below it).
                if top.height > 0 {
                    let line = Rect {
                        x: area.x,
                        y: top.y + top.height - 1,
                        width: area.width,
                        height: 1,
                    };
                    self.hit.borrow_mut().dividers.push((Divider::Split, line));
                }
            }
            // Hint the row under the pointer, then light up any hovered/dragged
            // divider (the terminal-native stand-in for a resize cursor).
            self.paint_hovered_row(frame);
            if let Some(active) = self.dragging.or(self.hovered_divider) {
                self.paint_active_divider(frame, active);
            }
        }
        self.draw_hint_bar(frame, rows[1]);

        // Overlays sit on top of the scene and capture input (§8.2).
        match self.mode {
            Mode::Help => self.draw_help(frame, full),
            Mode::Palette => self.draw_palette(frame, full),
            Mode::Run => self.draw_run_prompt(frame, full),
            // Terminal capture has no overlay; the tab bar shows the indicator.
            Mode::Normal | Mode::Terminal => {}
        }
    }

    /// The always-on hint bar: a projection of the command registry showing the
    /// most relevant keys for the focused column (§8.2).
    /// The bottom hint bar: contextual commands in `hint_rank` priority order,
    /// fitted to the available width. Rather than clipping a hint mid-word, whole
    /// low-priority items drop from the end; `Help` is pinned to the far right as
    /// the escape hatch to the full list, and a `…` signals that hints were
    /// dropped. (§8.2)
    fn draw_hint_bar(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
        let sep = format!(" {} ", self.theme.glyph("hint_separator"));
        let sep_w = sep.chars().count();

        let fit = command::fit_hints(self.focus(), area.width as usize, sep_w);

        // Final left-to-right order: fitted hints, a "…" if any were dropped,
        // then pinned Help. `None` marks the ellipsis slot.
        let mut items: Vec<Option<&command::HintItem>> = fit.shown.iter().map(Some).collect();
        if fit.truncated {
            items.push(None);
        }
        items.extend(fit.pinned.as_ref().map(Some));

        let mut spans: Vec<RSpan> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                spans.push(RSpan::styled(
                    sep.clone(),
                    self.theme.style("hint_separator", ctx, RainbowInput::None),
                ));
            }
            match item {
                Some(hint) => {
                    spans.push(RSpan::styled(
                        hint.keys.clone(),
                        self.theme.style("hint_key", ctx, RainbowInput::None),
                    ));
                    spans.push(RSpan::raw(" "));
                    spans.push(RSpan::styled(
                        hint.label.clone(),
                        self.theme.style("hint_label", ctx, RainbowInput::None),
                    ));
                }
                None => spans.push(RSpan::styled(
                    command::HINT_ELLIPSIS.to_string(),
                    self.theme.style("hint_separator", ctx, RainbowInput::None),
                )),
            }
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
                    RSpan::styled(
                        key,
                        self.theme.style("palette_key", ctx, RainbowInput::None),
                    ),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        if !results.is_empty() {
            state.select(Some(self.palette.selected.min(results.len() - 1)));
        }
        let list = List::new(items)
            .highlight_style(self.theme.selection_style(true, self.ctx()))
            .highlight_symbol(self.theme.selection_symbol());
        frame.render_stateful_widget(list, rows[1], &mut state);
    }

    /// Split the frame into the top column band and the bottom viewport pane.
    ///
    /// Zooming the Viewport gives it the whole frame. Zooming a *top* column
    /// keeps the normal split so the viewport pane stays visible — the zoom just
    /// collapses that column's siblings to spines inside the top band (handled
    /// in `draw_top_columns`), giving the focused column the band's full width.
    fn split_scene(&self, area: Rect) -> (Rect, Rect) {
        let empty = Rect { height: 0, ..area };
        if self.zoom && self.nav.focused == ColumnKind::Viewport {
            return (empty, area);
        }
        // The top band takes the dragged fraction of the scene (default 0.45),
        // clamped so both panes keep a usable minimum height.
        let frac = self
            .layout
            .split_fraction
            .unwrap_or(DEFAULT_SPLIT_FRACTION)
            .clamp(0.0, 1.0);
        let raw = (area.height as f32 * frac).round() as u16;
        let top_h = raw.clamp(
            MIN_PANE_HEIGHT,
            area.height
                .saturating_sub(MIN_PANE_HEIGHT)
                .max(MIN_PANE_HEIGHT),
        );
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(top_h), Constraint::Min(0)])
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
        let zoom = self.zoom && self.nav.focused != ColumnKind::Viewport;
        let slots = layout::plan_over(
            area.width,
            self.nav.focused,
            zoom,
            &columns,
            Some(self.stacks_content_width()),
            &self.layout,
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
        let constraints: Vec<Constraint> = widths.iter().map(|w| Constraint::Length(*w)).collect();
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

        // Record the geometry a divider drag needs: the band, the expanded
        // budget, and the 1-cell vertical line between each pair of *expanded*
        // neighbors (a boundary against a collapsed spine isn't draggable).
        let mut hit = self.hit.borrow_mut();
        hit.band = area;
        hit.expanded_total = slots
            .iter()
            .zip(widths.iter())
            .filter(|(s, _)| s.width.is_some())
            .map(|(_, w)| *w)
            .sum();
        for i in 0..slots.len().saturating_sub(1) {
            if slots[i].width.is_some() && slots[i + 1].width.is_some() {
                let chunk = chunks[i];
                let line = Rect {
                    x: chunk.x + chunk.width - 1,
                    y: area.y,
                    width: 1,
                    height: area.height,
                };
                hit.dividers
                    .push((Divider::Column(slots[i].kind, slots[i + 1].kind), line));
            }
        }
    }

    /// Tint the hovered row with the `row_hover` bar, so the row under the
    /// pointer reads as clickable and previews what a click would select.
    /// Skipped when the hovered row *is* the current selection or no longer
    /// maps to a live row (e.g. the layout shifted since the last move).
    fn paint_hovered_row(&self, frame: &mut Frame) {
        let Some((kind, y)) = self.hovered_row else {
            return;
        };
        let (col, live, selected) = {
            let hit = self.hit.borrow();
            let col = hit
                .columns
                .iter()
                .find(|(k, _)| *k == kind)
                .map(|(_, r)| *r);
            let row_idx =
                |rows: &[(u16, usize)]| rows.iter().find(|(ry, _)| *ry == y).map(|(_, i)| *i);
            let (live, selected) = match kind {
                ColumnKind::Stacks => {
                    if let Some(i) = row_idx(&hit.stacks) {
                        (
                            true,
                            self.nav.selected_recent.is_none() && i == self.nav.selected_stair,
                        )
                    } else if let Some(i) = row_idx(&hit.recents) {
                        (true, self.nav.selected_recent == Some(i))
                    } else {
                        (false, false)
                    }
                }
                ColumnKind::Commits => match row_idx(&hit.commits) {
                    Some(i) => (true, i == self.nav.selected_commit),
                    None => (false, false),
                },
                ColumnKind::Files => match row_idx(&hit.files) {
                    Some(i) => (true, i == self.nav.selected_file),
                    None => (false, false),
                },
                _ => (false, false),
            };
            (col, live, selected)
        };
        let (Some(col), true, false) = (col, live, selected) else {
            return;
        };
        let Some(bg) = self
            .theme
            .style("row_hover", self.ctx(), RainbowInput::None)
            .bg
        else {
            return;
        };
        // Span the row's content: from just inside the left border (or flush to
        // the shared divider for a column with an expanded left neighbor) up to
        // the right border.
        let has_left = self
            .hit
            .borrow()
            .columns
            .iter()
            .any(|(_, r)| r.x + r.width == col.x);
        let x0 = if has_left { col.x } else { col.x + 1 };
        let x1 = col.x + col.width.saturating_sub(1);
        let buf = frame.buffer_mut();
        for x in x0..x1 {
            if x < buf.area.right() && y < buf.area.bottom() {
                buf[(x, y)].set_bg(bg);
            }
        }
    }

    /// Repaint the active (hovered or dragging) divider's line in the accent
    /// `divider_active` color so it reads as a grabbable resize handle.
    fn paint_active_divider(&self, frame: &mut Frame, active: Divider) {
        let rect = self
            .hit
            .borrow()
            .dividers
            .iter()
            .find(|(d, _)| *d == active)
            .map(|(_, r)| *r);
        let Some(rect) = rect else { return };
        let style = self
            .theme
            .style("divider_active", self.ctx(), RainbowInput::None);
        let buf = frame.buffer_mut();
        for y in rect.y..rect.y.saturating_add(rect.height) {
            for x in rect.x..rect.x.saturating_add(rect.width) {
                if x < buf.area.right() && y < buf.area.bottom() {
                    buf[(x, y)].set_style(style);
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
            Paragraph::new(crumb).style(self.theme.style(
                "breadcrumb",
                self.ctx(),
                RainbowInput::None,
            )),
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
        let focused = kind == self.nav.focused;
        let style = self.theme.column_title_style(focused, self.ctx());
        let inner_h = area.height.saturating_sub(2) as usize;
        let title: String = kind.title().chars().take(inner_h).collect();
        let vertical: Vec<Line> = title
            .chars()
            .map(|c| Line::from(RSpan::styled(c.to_string(), style)))
            .collect();
        let block = Block::default()
            .borders(borders)
            .border_style(
                self.theme
                    .style("column_border", self.ctx(), RainbowInput::None),
            );
        frame.render_widget(Paragraph::new(vertical).block(block), area);
    }

    /// Draw an expanded column. `left_border` is `false` for columns that abut a
    /// neighbor on their left (in the top band) so the shared divider is a
    /// single line; standalone columns (viewport pane, deck mode) pass `true`.
    fn draw_column(&self, frame: &mut Frame, area: Rect, kind: ColumnKind, left_border: bool) {
        self.hit.borrow_mut().columns.push((kind, area));
        let focused = kind == self.nav.focused;
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
            .border_style(
                self.theme
                    .style("column_border", self.ctx(), RainbowInput::None),
            )
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
            ColumnKind::Viewport => self.draw_viewport_active(frame, body),
            ColumnKind::Checks => self.draw_checks(frame, body),
        }
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
            // Measure the fully-rendered row (staircase glyph + name + counters +
            // `(n branches)` + dirty marker) so the column never truncates it.
            .map(|s| self.stair_line(s).width())
            .max()
            .unwrap_or(0);
        // Ensure the "Stacks" title still fits in the border.
        let title = "Stacks".len();
        // The legend row has no marker; it just needs the inner width.
        let legend = legend_width(&self.column_legend(ColumnKind::Stacks));
        // Let the recents rows widen the column too, so short labels (e.g.
        // "bazel-mono libs/proto") show in full rather than eliding. Recents
        // sit flush-left with no marker, so they need no extra lead width.
        let recents = self.recents_content_width();
        let inner = (MARKER + content.max(title)).max(legend).max(recents);
        (inner + BORDERS) as u16
    }

    /// Widest fully-rendered recents row ("parent label"), in inner columns.
    /// `0` when the recents ledger is hidden (no other repos).
    fn recents_content_width(&self) -> usize {
        if !self.recents_has_others() {
            return 0;
        }
        self.recents
            .rows
            .iter()
            .map(|r| self.recent_display_width(r))
            .max()
            .unwrap_or(0)
    }

    fn draw_stacks(&self, frame: &mut Frame, area: Rect) {
        // With other repos in the MRU, the column becomes a ledger: the current
        // repo as a dot-less header line (like the `upstream` line in Commits),
        // its staircases below, then every other repo as its own single line in
        // MRU order, dimmed by recency. Alone, it's just the staircases — no
        // needless repo header.
        if self.recents_has_others() {
            self.draw_stacks_ledger(frame, area);
        } else {
            self.draw_stacks_flat(frame, area);
        }
    }

    /// The plain staircase list (no recents): the original Stacks rendering,
    /// filling `area` (which may be a sub-rect below the current-repo header).
    fn draw_stacks_flat(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .snapshot
            .staircases
            .iter()
            .map(|s| ListItem::new(self.stair_line(s)))
            .collect();
        let mut state = ListState::default();
        // When the cursor has dropped into the recents ledger, the staircase
        // list shows no highlight (the ledger owns the cursor); Commits still
        // follows the last-selected staircase.
        state.select(
            self.nav
                .selected_recent
                .is_none()
                .then_some(self.nav.selected_stair),
        );
        let focused = self.nav.focused == ColumnKind::Stacks;
        let list = List::new(items)
            .highlight_style(self.theme.selection_style(focused, self.ctx()))
            .highlight_symbol(self.theme.selection_symbol())
            // Always reserve the marker column so staircases keep a constant
            // indent whether or not the cursor is in the list (it may sit in the
            // recents ledger, which clears this list's selection).
            .highlight_spacing(HighlightSpacing::Always);
        frame.render_stateful_widget(list, area, &mut state);

        // Map rows through the scroll offset ratatui computed (see `draw_commits`).
        {
            let offset = state.offset();
            let mut hit = self.hit.borrow_mut();
            for i in offset..self.snapshot.staircases.len() {
                let ry = area.y + (i - offset) as u16;
                if ry >= area.y + area.height {
                    break;
                }
                hit.stacks.push((ry, i));
            }
        }
    }

    /// The multi-repo ledger: current-repo header, staircases, then the other
    /// repos flush-left in MRU order (most-recent first), each getting dimmer as
    /// it ages. Repo rows aren't selectable yet — switching lands in a later
    /// pass; for now they're an at-a-glance "what else is open" list.
    fn draw_stacks_ledger(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
        let others = self.recents_others();
        // Pin the others region to the bottom but never starve the header + at
        // least one staircase row; the gap between short stacks and the ledger
        // reads as a natural separator.
        let cap = area.height.saturating_sub(2);
        let others_h = (others.len() as u16).min(cap);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(others_h),
            ])
            .split(area);

        // Current repo: a dot-less header line in the `upstream` style. First =
        // most-recent, so no marker is needed to say "you are here".
        // The branch markers form a column for the other repos: reserve its width
        // (the widest branch across those rows) and left-justify each branch
        // within it, so all the branch icons align on one column. The current
        // repo's branch aligns separately on its own.
        let others_branch_col = others
            .iter()
            .filter_map(|r| self.recent_branch_text(r, Some(RECENTS_MAX_BRANCH)))
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(0);

        if let Some(current) = self.recents.rows.iter().find(|r| r.current) {
            let current_branch_col = self
                .recent_branch_text(current, None)
                .map(|s| s.chars().count())
                .unwrap_or(0);
            let text = self.recent_row_layout(current, rows[0].width as usize, current_branch_col);
            frame.render_widget(
                Paragraph::new(text).style(self.theme.style(
                    "commit_header",
                    ctx,
                    RainbowInput::None,
                )),
                rows[0],
            );
        }

        self.draw_stacks_flat(frame, rows[1]);

        let width = rows[2].width as usize;
        let branch_counts = self.recent_branch_counts();
        let items: Vec<ListItem> = others
            .iter()
            .enumerate()
            .map(|(rank, row)| {
                let key = self.recent_identity(row, &branch_counts);
                ListItem::new(self.recent_ledger_line(row, key, rank, width, others_branch_col))
            })
            .collect();
        let mut state = ListState::default();
        state.select(self.nav.selected_recent);
        // No highlight symbol: it would reserve a left gutter and shift the
        // aligned branch column. Selection reads as a background tint.
        let focused = self.nav.focused == ColumnKind::Stacks;
        let list = List::new(items).highlight_style(self.theme.selection_style(focused, ctx));
        frame.render_stateful_widget(list, rows[2], &mut state);

        // Record screen rows for click-to-switch, mapped through the scroll
        // offset ratatui computed (see `draw_commits`).
        {
            let offset = state.offset();
            let mut hit = self.hit.borrow_mut();
            for i in offset..others.len() {
                let ry = rows[2].y + (i - offset) as u16;
                if ry >= rows[2].y + rows[2].height {
                    break;
                }
                hit.recents.push((ry, i));
            }
        }
    }

    /// One flush-left ledger line for a non-current repo: a dim `parent` prefix,
    /// its `label`, and a dimmer trailing `⎇ branch` marker — all faded toward
    /// the background by MRU `rank` so older repos recede (§8.3 relevance
    /// dimming), the parent and branch tinted further as secondary detail. When
    /// the column is too narrow for the tinted segments, the whole row falls
    /// back to a single elided run.
    fn recent_ledger_line(
        &self,
        row: &RecentRowView,
        key: &str,
        rank: usize,
        width: usize,
        branch_col: usize,
    ) -> Line<'static> {
        // The whole row is one uniform color: the `recent` identity hue keyed by
        // the branch name, faded by MRU age (§8.3 relevance). The repo name sits
        // flush left; the directory is right-justified so it ends one space
        // before the branch marker, which is itself left-justified in a fixed
        // column at the right edge (`branch_col`) so every marker aligns.
        let ctx = self.ctx();
        let relevance = self.recent_relevance(rank);
        let hue = self
            .theme
            .style_at("recent_row", ctx, RainbowInput::Key(key), relevance);

        let limit = if row.current {
            None
        } else {
            Some(RECENTS_MAX_BRANCH)
        };
        let branch = self.recent_branch_text(row, limit);
        let has_branch = branch.is_some() && branch_col > 0;
        // Too narrow for any left part → show the branch alone.
        if has_branch && width <= branch_col + 1 {
            return Line::from(RSpan::styled(
                elide(branch.as_deref().unwrap_or(""), width),
                hue,
            ));
        }
        // Columns for "name … dir". With a branch we reserve its column plus a
        // one-space gap; the branch then starts at `left_region + 1`.
        let left_region = if has_branch {
            width - branch_col - 1
        } else {
            width
        };

        let mut spans: Vec<RSpan<'static>> = Vec::new();
        let used: usize;
        match &row.parent {
            // Repo name flush left, directory right-justified after it.
            Some(name) => {
                let nl = name.chars().count() + 1; // name + trailing space
                if nl >= left_region {
                    let fit = elide(name, left_region);
                    used = fit.chars().count();
                    spans.push(RSpan::styled(fit, hue));
                } else {
                    let dir_budget = left_region - nl;
                    let dir = elide(&row.label, dir_budget);
                    let dl = dir.chars().count();
                    spans.push(RSpan::styled(format!("{name} "), hue));
                    if has_branch {
                        // Right-justify the directory within its budget.
                        let mid = dir_budget - dl;
                        spans.push(RSpan::styled(" ".repeat(mid), hue));
                        spans.push(RSpan::styled(dir, hue));
                        used = nl + mid + dl;
                    } else {
                        spans.push(RSpan::styled(dir, hue));
                        used = nl + dl;
                    }
                }
            }
            // Loose repo (no monorepo root): its label is the repo root, so it
            // stays flush left like the named repos — nothing to right-justify.
            None => {
                let fit = elide(&row.label, left_region);
                used = fit.chars().count();
                spans.push(RSpan::styled(fit, hue));
            }
        }

        if has_branch {
            // Fill any remainder of the left region (only the name-only fallback
            // needs it), then exactly one space, then the aligned branch marker.
            let tail = left_region.saturating_sub(used);
            spans.push(RSpan::styled(" ".repeat(tail + 1), hue));
            spans.push(RSpan::styled(branch.unwrap_or_default(), hue));
        }
        Line::from(spans)
    }

    /// The relevance for a recents row at MRU `rank` (0 = most-recent = full
    /// relevance). Follows §8.3's temporal decay — rank standing in for age —
    /// with a floor so a deep row still keeps its identity/legibility rather
    /// than collapsing into the background.
    fn recent_relevance(&self, rank: usize) -> f32 {
        temporal_decay(rank as f32, RECENTS_HALF_LIFE).max(RECENTS_RELEVANCE_FLOOR)
    }

    /// The rainbow-identity key for a recents row (§8.3): the **branch name**
    /// when that branch is checked out in more than one known repo (so shared
    /// branches share a hue), otherwise the repo's **path within its root**
    /// (`label`) — never the root/parent name, which shouldn't drive color.
    fn recent_identity<'a>(
        &'a self,
        row: &'a RecentRowView,
        branch_counts: &HashMap<&str, usize>,
    ) -> &'a str {
        match &row.branch {
            Some(b) if branch_counts.get(b.as_str()).copied().unwrap_or(0) > 1 => b,
            _ => &row.label,
        }
    }

    /// How many known repos have each branch checked out, across the whole
    /// ledger (current + others), so [`recent_identity`] can tell a shared
    /// branch (hue by branch) from a unique one (hue by path).
    fn recent_branch_counts(&self) -> HashMap<&str, usize> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for row in &self.recents.rows {
            if let Some(branch) = &row.branch {
                *counts.entry(branch.as_str()).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Split a recents row into its left part (`"parent label"`, or just
    /// `label` for a loose repo) and its right part (`"{glyph}branch"`, the
    /// branch elided to the given limit so a long name can't widen the
    /// column unbounded). The right part is `None` when the HEAD is unknown.
    fn recent_row_parts(
        &self,
        row: &RecentRowView,
        limit: Option<usize>,
    ) -> (String, Option<String>) {
        let left = match &row.parent {
            Some(parent) => format!("{parent} {}", row.label),
            None => row.label.clone(),
        };
        (left, self.recent_branch_text(row, limit))
    }

    /// The trailing `"{glyph}branch"` marker for a recents row, branch elided to
    /// the given limit so a long name can't widen the column unbounded. `None`
    /// when the HEAD is unknown.
    fn recent_branch_text(&self, row: &RecentRowView, limit: Option<usize>) -> Option<String> {
        row.branch.as_ref().map(|b| {
            format!(
                "{}{}",
                self.theme.glyph("recent_branch"),
                if let Some(l) = limit {
                    elide(b, l)
                } else {
                    b.to_string()
                }
            )
        })
    }

    /// A recents row laid out on one line: the left part, then the branch part
    /// left-justified in a fixed-width column (`branch_col`) at the right edge so
    /// it aligns with the ledger rows below. When the row is too wide the *left*
    /// part is elided so the branch still lands at the column start (rather than
    /// gluing it after a truncated label).
    fn recent_row_layout(&self, row: &RecentRowView, width: usize, branch_col: usize) -> String {
        let limit = if row.current {
            None
        } else {
            Some(RECENTS_MAX_BRANCH)
        };
        let (left, branch) = self.recent_row_parts(row, limit);
        let Some(branch) = branch else {
            return elide(&left, width);
        };
        // No shared column, or too narrow for any left part: branch only.
        if branch_col == 0 {
            return elide(&left, width);
        }
        if width <= branch_col + 1 {
            return elide(&branch, width);
        }
        // Reserve the shared branch column (plus a one-space minimum gap) at the
        // right edge, fit the left part into what remains, and pad to the branch
        // column start so the (left-justified) branch aligns with the rows below.
        let start = width - branch_col;
        let left_fit = elide(&left, start - 1);
        let pad = start - left_fit.chars().count();
        format!("{left_fit}{}{branch}", " ".repeat(pad))
    }

    /// Width (in cells) a recents row wants: left part + branch part + a
    /// two-space gap, so the column is wide enough to right-justify the branch.
    fn recent_display_width(&self, row: &RecentRowView) -> usize {
        let limit = if row.current {
            None
        } else {
            Some(RECENTS_MAX_BRANCH)
        };
        let (left, branch) = self.recent_row_parts(row, limit);
        left.chars().count() + branch.map(|b| 2 + b.chars().count()).unwrap_or(0)
    }

    /// One staircase row: its identity-hued name, ahead/behind counters, and a
    /// dirty marker. A true staircase (more than one branch) leads with a
    /// staircase glyph and trails a `(n branches)` count; its `name` is already
    /// the family prefix its branches share (§2). A lone branch shows plainly.
    /// The theme role for a staircase's rebase chip, or `None` when no rebase is
    /// indicated (in sync, or the probe reached no verdict). Clean = a free
    /// rebase is available; Conflict = a rebase would need manual resolution.
    fn rebase_role(&self, s: &Staircase) -> Option<&'static str> {
        if !Self::needs_reflow(s) {
            return None;
        }
        match s.rebase {
            RebaseStatus::Clean => Some("stack_rebase_clean"),
            RebaseStatus::Conflict => Some("stack_rebase_conflict"),
            RebaseStatus::Unknown => None,
        }
    }

    /// Whether a stack needs *reflowing* — either a restack (a stale child on an
    /// amended parent) or a rebase onto its upstream (behind). Restack takes
    /// priority, matching the host's probe selection.
    fn needs_reflow(s: &Staircase) -> bool {
        s.segments.iter().any(|seg| seg.stale) || s.behind > 0
    }

    /// The verb for a stack's reflow chip: `restack` when a stale child dangles
    /// on an amended parent, else `rebase` onto its upstream.
    fn reflow_verb(s: &Staircase) -> &'static str {
        if s.segments.iter().any(|seg| seg.stale) {
            "restack"
        } else {
            "rebase"
        }
    }

    fn stair_line(&self, s: &Staircase) -> Line<'static> {
        let ctx = self.ctx();
        let mut spans: Vec<RSpan<'static>> = Vec::new();
        let branches = s.segments.len();
        let is_staircase = branches > 1;
        // Each staircase keeps its own identity hue (§8.3), keyed by its name.
        if is_staircase {
            let glyph = self.theme.glyph("stack_staircase");
            if !glyph.is_empty() {
                spans.push(RSpan::styled(
                    format!("{glyph} "),
                    self.theme
                        .style("stack_staircase", ctx, RainbowInput::Key(&s.name)),
                ));
            }
        }
        spans.push(RSpan::styled(
            s.name.clone(),
            self.theme
                .style("stack_name", ctx, RainbowInput::Key(&s.name)),
        ));
        // Dirtiness rides the name as a trailing "*" (glued on, like the run
        // tab's `main*`) rather than a pencil — the Commits worktree row keeps
        // the pencil via the `dirty` role.
        if s.dirty {
            spans.push(RSpan::styled(
                self.theme.glyph("stack_dirty").to_string(),
                self.theme.style("stack_dirty", ctx, RainbowInput::None),
            ));
        }
        spans.push(RSpan::styled(
            self.counters_text(s.ahead, s.behind),
            self.theme.style("stack_counters", ctx, RainbowInput::None),
        ));
        if is_staircase {
            spans.push(RSpan::styled(
                format!(" ({branches} branches)"),
                self.theme.style("stack_counters", ctx, RainbowInput::None),
            ));
        }
        // Rebase-onto-upstream affordance: a compact glyph, shown only when the
        // stack is actually behind and the probe reached a verdict. Clean and
        // conflict use distinct glyphs (so hue is never the sole carrier — P6);
        // the Commits header spells out the verdict where there's room, and the
        // Stacks legend documents the glyphs.
        if let Some(role) = self.rebase_role(s) {
            spans.push(RSpan::styled(
                format!("  {}", self.theme.glyph(role)),
                self.theme.style(role, ctx, RainbowInput::None),
            ));
        }
        Line::from(spans)
    }

    /// Whether the recents ledger has any repo other than the current one.
    fn recents_has_others(&self) -> bool {
        self.recents.rows.iter().any(|r| !r.current)
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
        let mut header_spans = vec![RSpan::styled(
            format!(
                "upstream {} {}{} {}{}",
                stair.upstream,
                self.theme.glyph("ahead"),
                stair.ahead,
                self.theme.glyph("behind"),
                stair.behind,
            ),
            self.theme.style("commit_header", ctx, RainbowInput::None),
        )];
        // Spell out the rebase verdict here, where the header has room: the
        // Stacks row shows only the compact glyph. "clean" invites a one-click
        // rebase; "will conflict" flags that manual resolution is needed.
        if let Some(role) = self.rebase_role(stair) {
            let verb = Self::reflow_verb(stair);
            let label = match stair.rebase {
                RebaseStatus::Clean => format!("{verb} available"),
                // Name where it breaks (§4): "restack — will conflict on Amd.kt".
                RebaseStatus::Conflict => {
                    let mut l = format!("{verb} — will conflict");
                    if let Some(file) = stair
                        .conflict
                        .as_ref()
                        .and_then(|ci| ci.paths.first())
                        .map(|p| p.rsplit('/').next().unwrap_or(p))
                    {
                        let more = stair.conflict.as_ref().map_or(0, |ci| ci.paths.len());
                        l.push_str(&format!(" on {file}"));
                        if more > 1 {
                            l.push_str(&format!(" +{}", more - 1));
                        }
                    }
                    l
                }
                RebaseStatus::Unknown => String::new(),
            };
            header_spans.push(RSpan::styled(
                format!("   {} {label}", self.theme.glyph(role)),
                self.theme.style(role, ctx, RainbowInput::None),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(header_spans)), rows[0]);
        let list_area = rows[1];

        // Rainbow the commits across the whole stack: each commit gets its own
        // step along the staircase arc (§8.3), not one hue per segment.
        let total = stair
            .segments
            .iter()
            .map(|s| s.commits.len())
            .sum::<usize>();
        let mut items: Vec<ListItem> = Vec::new();
        // Line index (within the list) of each commit, for hit-testing + state.
        let mut commit_line: Vec<usize> = Vec::new();
        let mut commit_idx = 0usize;

        for (si, seg) in stair.segments.iter().enumerate() {
            let indent = " ".repeat(seg.parent.map_or(0, |_| si.min(6)));
            // Commit rows sit two spaces in from their branch riser.
            let body_indent = format!("{indent}  ");
            let body_indent_w = body_indent.chars().count();
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
                    let label = format!(
                        "{} Uncommitted changes",
                        self.theme.glyph("commit_worktree")
                    );
                    let churn_w = stat_width(c.added, c.deleted);
                    let pad = content_w
                        .saturating_sub(body_indent_w + label.chars().count() + churn_w)
                        .max(1);
                    commit_line.push(items.len());
                    let mut spans = vec![
                        RSpan::raw(body_indent.clone()),
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
                let pos = RainbowInput::Position {
                    index: commit_idx,
                    total,
                };
                let (chip_spans, chips_w) = self.chip_spans(c);
                // Pin the reflow conflict to its commit (§4): the offending step
                // gets a warn glyph so "will conflict" points at an exact row.
                let conflict_here = stair
                    .conflict
                    .as_ref()
                    .is_some_and(|ci| !ci.commit.is_empty() && ci.commit == c.oid);
                let (conflict_span, conflict_w) = if conflict_here {
                    let g = self.theme.glyph("commit_conflict");
                    (
                        Some(RSpan::styled(
                            format!(" {g}"),
                            self.theme.style("commit_conflict", ctx, RainbowInput::None),
                        )),
                        g.chars().count() + 1,
                    )
                } else {
                    (None, 0)
                };
                let churn_w = stat_width(c.added, c.deleted);
                // Optional leading commit marker (Nerd mode only; empty otherwise).
                // Carries the row's commit hue like the hash, so the row reads as
                // one identity.
                let marker = self.theme.glyph("commit");
                let marker = if marker.is_empty() {
                    String::new()
                } else {
                    format!("{marker} ")
                };
                let marker_w = marker.chars().count();
                // The `-N +M` churn is right-justified against the column edge;
                // the subject fills the space in between, truncated (from the
                // back) only when it would otherwise collide with the churn.
                // Reserve the highlight marker, indent, hash, chips, and churn.
                let indent_w = body_indent_w;
                let short_w = c.short.chars().count();
                let fixed = indent_w + marker_w + short_w + 1 + chips_w + conflict_w + churn_w;
                let budget = content_w.saturating_sub(fixed + 1).max(8);
                let subject = truncate(&c.subject, budget);
                let used_left = indent_w
                    + marker_w
                    + short_w
                    + 1
                    + subject.chars().count()
                    + chips_w
                    + conflict_w;
                let pad = content_w.saturating_sub(used_left + churn_w).max(1);
                commit_line.push(items.len());
                // Identity hue is carried by the hash and chips; the subject is
                // the plain row-text class, brightened when its row is selected.
                let selected = commit_idx == self.nav.selected_commit;
                let subject_style = if selected {
                    self.theme.style_state(
                        "commit_subject",
                        "row_selected",
                        ctx,
                        RainbowInput::None,
                    )
                } else {
                    self.theme.style("commit_subject", ctx, RainbowInput::None)
                };
                let mut spans = vec![RSpan::raw(body_indent.clone())];
                if !marker.is_empty() {
                    spans.push(RSpan::styled(
                        marker,
                        self.theme.style("commit_hash", ctx, pos),
                    ));
                }
                spans.extend([
                    RSpan::styled(c.short.clone(), self.theme.style("commit_hash", ctx, pos)),
                    RSpan::styled(format!(" {subject}"), subject_style),
                ]);
                spans.extend(chip_spans);
                if let Some(conflict_span) = conflict_span {
                    spans.push(conflict_span);
                }
                spans.push(spaces(pad));
                spans.extend(self.churn_spans(c.added, c.deleted));
                items.push(ListItem::new(Line::from(spans)));
                commit_idx += 1;
            }
        }

        let selected_line = commit_line.get(self.nav.selected_commit).copied();
        let mut state = ListState::default();
        state.select(selected_line);
        let focused = self.nav.focused == ColumnKind::Commits;
        let list = List::new(items)
            .highlight_style(self.theme.selection_style(focused, ctx))
            .highlight_symbol(self.theme.selection_symbol());
        frame.render_stateful_widget(list, list_area, &mut state);

        // Hit rows: map each commit's line index through the scroll offset
        // ratatui just computed, so clicks land on the row actually shown even
        // when the list is scrolled (offset > 0). Built after render because the
        // offset isn't known until the stateful widget lays itself out.
        {
            let offset = state.offset();
            let mut hit = self.hit.borrow_mut();
            for (ci, &line) in commit_line.iter().enumerate() {
                let Some(vis) = line.checked_sub(offset) else {
                    continue;
                };
                let ry = list_area.y + vis as u16;
                if ry >= list_area.y + list_area.height {
                    break;
                }
                hit.commits.push((ry, ci));
            }
        }
    }

    /// Repo-relative paths that would conflict *at the currently selected
    /// commit* — non-empty only when that commit is the one the reflow probe
    /// halted on (§4). Lets the Files column flag exactly which files clash.
    fn selected_conflict_paths(&self) -> Vec<String> {
        let Some(oid) = self.selected_commit_oid() else {
            return Vec::new();
        };
        match self.selected().and_then(|s| s.conflict.as_ref()) {
            Some(ci) if ci.commit == oid => ci.paths.clone(),
            _ => Vec::new(),
        }
    }

    fn draw_files(&self, frame: &mut Frame, area: Rect) {
        if self.files.is_empty() {
            let msg = if self.selected_commit_oid().is_some() {
                "(no changed files)"
            } else {
                "(select a commit)"
            };
            frame.render_widget(
                Paragraph::new(msg).style(self.theme.style(
                    "diff_placeholder",
                    self.ctx(),
                    RainbowInput::None,
                )),
                area,
            );
            return;
        }
        let conflict_paths = self.selected_conflict_paths();
        let items: Vec<ListItem> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let ctx = self.ctx();
                let selected = i == self.nav.selected_file;
                // The pinned commit-message row renders as a labelled envelope,
                // not a path (no directory split, no rainbow-by-folder).
                if f.status == FileStatus::Message {
                    return ListItem::new(Line::from(vec![
                        RSpan::styled(
                            format!("{} ", self.theme.glyph("file_message_glyph")),
                            self.theme
                                .style("file_message_glyph", ctx, RainbowInput::None),
                        ),
                        RSpan::styled(
                            f.path.clone(),
                            self.theme
                                .style("file_message_path", ctx, RainbowInput::None),
                        ),
                    ]));
                }
                let status = f.status;
                let (dir, name) = split_path(&f.path);
                let churn_w = stat_width(f.added, f.deleted);
                const MARKER: usize = 2; // "▶ " highlight symbol
                let content_w = (area.width as usize).saturating_sub(MARKER);
                let status_w = 2; // "M "
                let name_w = name.chars().count();
                // Flag files that clash at the offending commit (§4): a warn
                // glyph riding the filename, so "will conflict on X" is visible
                // right in the Files column.
                let conflicted = conflict_paths.iter().any(|p| p == &f.path);
                let conflict_glyph = self.theme.glyph("file_conflict");
                let conflict_w = if conflicted && !conflict_glyph.is_empty() {
                    conflict_glyph.chars().count() + 1
                } else {
                    0
                };
                // Right-justify the churn; give whatever space is left to the
                // directory, shortening it from the *front* so the leaf folder
                // stays visible. The filename is never truncated.
                let reserved = status_w + name_w + conflict_w + churn_w + 1; // +1 min gap
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
                if conflict_w > 0 {
                    spans.push(RSpan::styled(
                        format!(" {conflict_glyph}"),
                        self.theme.style("file_conflict", ctx, RainbowInput::None),
                    ));
                }
                let mut used_left = status_w + name_w + conflict_w;
                if !dir.is_empty() {
                    // Directory block is "  {dir}"; budget its dir portion.
                    let dir_max = content_w.saturating_sub(reserved + 2); // 2 = "  "
                    if dir_max > 0 {
                        let shown = truncate_front(dir, dir_max);
                        used_left += 2 + shown.chars().count();
                        // Directory is the plain row-text class; brighten it on
                        // the selected row to match the commit title's behavior.
                        let dir_style = if selected {
                            self.theme.style_state(
                                "file_dir",
                                "row_selected",
                                ctx,
                                RainbowInput::None,
                            )
                        } else {
                            self.theme.style("file_dir", ctx, RainbowInput::None)
                        };
                        spans.push(RSpan::styled(format!("  {shown}"), dir_style));
                    }
                }
                let pad = content_w.saturating_sub(used_left + churn_w).max(1);
                spans.push(spaces(pad));
                spans.extend(self.churn_spans(f.added, f.deleted));
                ListItem::new(Line::from(spans))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.nav.selected_file));
        let focused = self.nav.focused == ColumnKind::Files;
        let list = List::new(items)
            .highlight_style(self.theme.selection_style(focused, self.ctx()))
            .highlight_symbol(self.theme.selection_symbol());
        frame.render_stateful_widget(list, area, &mut state);

        // Map rows through the scroll offset ratatui computed (see `draw_commits`).
        {
            let offset = state.offset();
            let mut hit = self.hit.borrow_mut();
            for i in offset..self.files.len() {
                let ry = area.y + (i - offset) as u16;
                if ry >= area.y + area.height {
                    break;
                }
                hit.files.push((ry, i));
            }
        }
    }

    fn draw_diff(&self, frame: &mut Frame, area: Rect) {
        let diff = self.viewport.diff();
        if diff.rows.is_empty() {
            let msg = match (self.selected_commit_oid(), self.selected_file_path()) {
                (Some(_), Some(_)) if diff.is_raw => "(empty file)",
                (Some(_), Some(_)) => "(no diff for this file)",
                _ => "(select a file)",
            };
            frame.render_widget(
                Paragraph::new(msg).style(self.theme.style(
                    "diff_placeholder",
                    self.ctx(),
                    RainbowInput::None,
                )),
                area,
            );
            return;
        }
        // Every row is syntax-highlighted (cached at load). Each row leads with a
        // one-cell change bar — a colored marker on added/deleted rows, blank on
        // context — then the before/after line-number gutters, then the code. The
        // bar keeps the change legible even where the row's tinted background is
        // faint (and in 256-color, where that tint is neutral). The background
        // then fills the whole row width.
        let width = area.width as usize;
        let ctx = self.ctx();
        let add = self.theme.style("diff_added", ctx, RainbowInput::None);
        let del = self.theme.style("diff_deleted", ctx, RainbowInput::None);
        let add_glyph = self.theme.glyph("diff_added").to_string();
        let del_glyph = self.theme.glyph("diff_deleted").to_string();
        let lineno = self.theme.style("diff_lineno", ctx, RainbowInput::None);
        // Fixed-width before/after line-number gutters, sized to their widest
        // number. Suppressed for the commit-message view (line numbers are noise
        // there). Layout per row: change marker then "{old} {new} ".
        let gutter = (!diff.is_message)
            .then(|| {
                let digits = |n: u32| n.max(1).to_string().len();
                let ow = diff
                    .rows
                    .iter()
                    .filter_map(|r| r.old)
                    .max()
                    .map_or(0, digits);
                let nw = diff
                    .rows
                    .iter()
                    .filter_map(|r| r.new)
                    .max()
                    .map_or(0, digits);
                (ow, nw)
            })
            .filter(|(ow, nw)| *ow > 0 || *nw > 0);
        let lines: Vec<Line> = diff
            .rows
            .iter()
            .skip(diff.scroll as usize)
            .take(area.height as usize)
            .map(|row| {
                let (marker, bg) = match row.kind {
                    DiffKind::Add => (RSpan::styled(add_glyph.clone(), add), add.bg),
                    DiffKind::Del => (RSpan::styled(del_glyph.clone(), del), del.bg),
                    DiffKind::Context => (RSpan::raw(" "), None),
                };
                let mut used = marker.content.chars().count();
                let mut spans: Vec<RSpan> = Vec::with_capacity(row.spans.len() + 3);
                // The change bar leads the row, then the line-number gutters.
                spans.push(marker);
                if let Some((ow, nw)) = gutter {
                    let cell = |n: Option<u32>, w: usize| {
                        n.map_or_else(|| " ".repeat(w), |v| format!("{v:>w$}"))
                    };
                    let text = format!("{} {} ", cell(row.old, ow), cell(row.new, nw));
                    used += text.chars().count();
                    let mut s = lineno;
                    if let Some(c) = bg {
                        s = s.bg(c);
                    }
                    spans.push(RSpan::styled(text, s));
                }
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
        frame.render_widget(Paragraph::new(lines), area);
    }

    /// Draw the tabbed bottom pane: the tab buttons ride the first row, with the
    /// active contributor's content filling the rest. Borderless — the top band's
    /// bottom borders already separate it, so the space is given to the body.
    fn draw_viewport(&self, frame: &mut Frame, area: Rect) {
        self.hit
            .borrow_mut()
            .columns
            .push((ColumnKind::Viewport, area));
        if area.height == 0 {
            return;
        }
        let bar = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        self.draw_viewport_tabs(frame, bar);
        let body = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height - 1,
        };
        self.draw_viewport_active(frame, body);
    }

    /// Render the active tab's content into `area` and record the content size
    /// (so command terminals can be sized to match).
    fn draw_viewport_active(&self, frame: &mut Frame, area: Rect) {
        // Command terminals reserve the top row for a fixed context header, so
        // every run terminal is sized to the area below it (Diff uses the full
        // area and ignores the content size). Sizing all runs alike — regardless
        // of which tab is active — keeps a backgrounded run's grid stable.
        let run_area = Rect {
            x: area.x,
            y: area.y.saturating_add(1),
            width: area.width,
            height: area.height.saturating_sub(1),
        };
        self.viewport_content_size
            .set((run_area.width, run_area.height));
        // All tabs can be closed (Diff included); with none left, show a hint
        // rather than indexing into an empty tab list.
        if self.viewport.tabs.is_empty() {
            frame.render_widget(
                Paragraph::new(
                    "(no tabs — select a file to open Diff, or press > to run a command)",
                )
                .style(self.theme.style(
                    "diff_placeholder",
                    self.ctx(),
                    RainbowInput::None,
                )),
                area,
            );
            return;
        }
        let run_idx = match self.viewport.active_tab() {
            Tab::Diff(_) => None,
            Tab::Run(_) => Some(self.viewport.active),
        };
        match run_idx {
            None => self.draw_diff(frame, area),
            Some(i) => {
                if let Some(Tab::Run(run)) = self.viewport.tabs.get(i) {
                    let header = Rect { height: 1, ..area };
                    self.draw_run_header(frame, header, run);
                    run.render(frame, run_area);
                    if !run.is_running() {
                        self.draw_run_buttons(frame, run_area, run);
                    }
                }
            }
        }
    }

    /// Draw the finished-command action buttons (Run Again / Close Tab) side by
    /// side, left-aligned just past the command's output, and record their click
    /// regions. Both share one uniform style, distinct from the tab pills. When
    /// output fills the pane, the strip pins to the last row (over that line).
    fn draw_run_buttons(&self, frame: &mut Frame, run_area: Rect, run: &RunView) {
        if run_area.height == 0 {
            return;
        }
        let ctx = self.ctx();
        // Leave one blank row between the output and the buttons (none after).
        let row = (run.content_height() + 1).min(run_area.height - 1);
        let strip = Rect {
            x: run_area.x,
            y: run_area.y + row,
            width: run_area.width,
            height: 1,
        };
        // A clean strip keeps the buttons legible even over a row of output.
        frame.render_widget(Clear, strip);
        let style = self.theme.style("action_button", ctx, RainbowInput::None);
        let cap = match style.bg {
            Some(bg) => Style::default().fg(bg),
            None => Style::default(),
        };
        let lead = self.theme.lead("action_button");
        let trail = self.theme.trail("action_button");
        let mut spans: Vec<RSpan> = Vec::new();
        let mut x = strip.x;
        let mut rects: Vec<(Rect, RunButton)> = Vec::new();
        for (glyph_role, text, action) in [
            ("run_rerun", "Run Again", RunButton::Rerun),
            ("run_close", "Close Tab", RunButton::Close),
        ] {
            let start = x;
            if !lead.is_empty() {
                spans.push(RSpan::styled(lead.to_string(), cap));
                x += lead.chars().count() as u16;
            }
            let glyph = self.theme.glyph(glyph_role);
            let body = if glyph.is_empty() {
                format!(" {text} ")
            } else {
                format!(" {glyph} {text} ")
            };
            x += body.chars().count() as u16;
            spans.push(RSpan::styled(body, style));
            if !trail.is_empty() {
                spans.push(RSpan::styled(trail.to_string(), cap));
                x += trail.chars().count() as u16;
            }
            rects.push((
                Rect {
                    x: start,
                    y: strip.y,
                    width: x - start,
                    height: 1,
                },
                action,
            ));
            spans.push(RSpan::raw("  "));
            x += 2;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), strip);
        self.hit.borrow_mut().viewport_run_buttons = rects;
    }

    /// A fixed, one-line context header at the top of a command tab, styled like
    /// the context header rows in Stacks/Commits: the command, the commit/branch
    /// it runs against, and — once finished — its exit code (color is never the
    /// sole carrier of the pass/fail state, per P6).
    fn draw_run_header(&self, frame: &mut Frame, area: Rect, run: &RunView) {
        let ctx = self.ctx();
        let glyph = self.theme.glyph("run_header");
        let lead = if glyph.is_empty() {
            String::new()
        } else {
            format!("{glyph} ")
        };
        // "{command}   {repo} ({git}) @ {target}": the action, then the repo
        // root, .git folder, and branch/commit it ran against.
        let mut text = format!("{lead}{}", run.command);
        let mut whence = String::new();
        if !run.context.repo_root.is_empty() {
            whence.push_str(&run.context.repo_root);
        }
        if !run.context.git_dir.is_empty() {
            whence.push_str(&format!(" ({})", run.context.git_dir));
        }
        // Name the target by its label (branch or short oid); when the label is a
        // branch, also pin the exact commit it resolved to.
        let label = self.run_display_label(run);
        let short: Option<String> = run
            .target_oid
            .as_ref()
            .filter(|o| o.as_str() != WORKTREE_OID)
            .map(|o| o.chars().take(7).collect());
        match &short {
            Some(s) if *s != run.label => whence.push_str(&format!(" @ {} · {}", label, s)),
            _ => whence.push_str(&format!(" @ {}", label)),
        }
        text.push_str(&format!("   {}", whence.trim_start()));
        if let TabStatus::Exited(code) = run.status() {
            text.push_str(&format!("   · exited {code}"));
        }
        frame.render_widget(
            Paragraph::new(text).style(self.theme.style("run_header", ctx, RainbowInput::None)),
            area,
        );
    }

    /// Render the tab buttons (`[badge] label x`) on the pane's top border and
    /// record their clickable regions.
    fn draw_viewport_tabs(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
        let capture = self.mode == Mode::Terminal;
        let close_glyph = self.theme.glyph("tab_close").to_string();
        let mut spans: Vec<RSpan> = Vec::new();
        let mut tabs: Vec<(Rect, usize)> = Vec::new();
        let mut closes: Vec<(Rect, usize)> = Vec::new();
        let mut badges: Vec<(Rect, usize)> = Vec::new();
        let mut x = area.x;
        let end = area.x + area.width;
        for (i, tab) in self.viewport.tabs.iter().enumerate() {
            if x >= end {
                break;
            }
            let active = i == self.viewport.active;
            let role = if active { "tab_active" } else { "tab" };
            // The button surface (with its background) styles the whole pill; the
            // caps borrow that background as their foreground so the rounded ends
            // blend into the surface.
            let btn = self.theme.style(role, ctx, RainbowInput::None);
            let on_btn = |s: Style| match btn.bg {
                Some(bg) => s.bg(bg),
                None => s,
            };
            let cap = match btn.bg {
                Some(bg) => Style::default().fg(bg),
                None => Style::default(),
            };
            let lead = self.theme.lead(role);
            let trail = self.theme.trail(role);
            let start = x;
            // A one-cell gap separates adjacent pills (not before the first).
            if i > 0 {
                spans.push(RSpan::raw(" "));
                x += 1;
            }
            if !lead.is_empty() {
                spans.push(RSpan::styled(lead.to_string(), cap));
                x += lead.chars().count() as u16;
            }
            spans.push(RSpan::styled(" ", btn));
            x += 1;
            if let Some(badge) = tab.badge() {
                let g = self.theme.glyph(badge.role);
                if !g.is_empty() {
                    let bx = x;
                    let w = g.chars().count() as u16 + 1;
                    spans.push(RSpan::styled(
                        format!("{g} "),
                        on_btn(self.theme.style(badge.role, ctx, RainbowInput::None)),
                    ));
                    if badge.cancel {
                        badges.push((
                            Rect {
                                x: bx,
                                y: area.y,
                                width: w,
                                height: 1,
                            },
                            i,
                        ));
                    }
                    x += w;
                }
            }
            // A per-kind type glyph leads the label (code for Diff, terminal for
            // Run), styled with the button surface. Skipped when the theme
            // defines no glyph (e.g. Unicode mode may leave it blank).
            let type_glyph = match tab {
                Tab::Diff(_) => self.theme.glyph("tab_diff"),
                Tab::Run(_) => self.theme.glyph("tab_run"),
            };
            if !type_glyph.is_empty() {
                spans.push(RSpan::styled(format!("{type_glyph} "), btn));
                x += type_glyph.chars().count() as u16 + 1;
            }
            let label = match tab {
                Tab::Run(r) => self.run_display_label(r),
                // Once the diff theme is switched, name it on the tab so the
                // choice is visible (default stays a plain "Diff").
                Tab::Diff(_) if self.syntax_theme_override.is_some() => {
                    format!("{} · {}", tab.label(), self.effective_syntax_theme())
                }
                _ => tab.label(),
            };
            spans.push(RSpan::styled(label.clone(), btn));
            x += label.chars().count() as u16;
            if !close_glyph.is_empty() {
                spans.push(RSpan::styled(" ", btn));
                x += 1;
                let cx = x;
                let cw = close_glyph.chars().count() as u16;
                let close_role = if active {
                    "tab_close_active"
                } else {
                    "tab_close"
                };
                spans.push(RSpan::styled(
                    close_glyph.clone(),
                    on_btn(self.theme.style(close_role, ctx, RainbowInput::None)),
                ));
                closes.push((
                    Rect {
                        x: cx,
                        y: area.y,
                        width: cw,
                        height: 1,
                    },
                    i,
                ));
                x += cw;
            }
            spans.push(RSpan::styled(" ", btn));
            x += 1;
            if !trail.is_empty() {
                spans.push(RSpan::styled(trail.to_string(), cap));
                x += trail.chars().count() as u16;
            }
            tabs.push((
                Rect {
                    x: start,
                    y: area.y,
                    width: x - start,
                    height: 1,
                },
                i,
            ));
        }
        if capture {
            let g = self.theme.glyph("tab_capture");
            let text = if g.is_empty() {
                " [capture]".to_string()
            } else {
                format!(" {g} capture")
            };
            spans.push(RSpan::styled(
                text,
                self.theme.style("tab_capture", ctx, RainbowInput::None),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        let mut hit = self.hit.borrow_mut();
        hit.viewport_tabs = tabs;
        hit.viewport_closes = closes;
        hit.viewport_badges = badges;
    }

    /// The `>` command launcher overlay: the command being typed with an inline
    /// history suggestion, and the resolved run context in the frame title.
    fn draw_run_prompt(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
        let popup = centered_rect(64, 5.min(area.height), area);
        frame.render_widget(Clear, popup);
        let target = self.exec_target();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!("Run command — {}", target.label))
            .border_style(self.theme.style("overlay_frame", ctx, RainbowInput::None));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        let secondary = self.theme.style("secondary", ctx, RainbowInput::None);
        let mut spans = vec![
            RSpan::styled(
                self.theme.glyph("palette_prompt").to_string(),
                self.theme.style("palette_prompt", ctx, RainbowInput::None),
            ),
            RSpan::raw(self.run_prompt.input.clone()),
        ];
        match self.run_prompt_suggestion() {
            // With a suggestion, the cursor rides the first suggested char
            // (reversed) so the completion reads contiguously — no caret cell
            // splitting "ca|rgo test".
            Some(sugg) => {
                let mut tail = sugg[self.run_prompt.input.len()..].chars();
                if let Some(first) = tail.next() {
                    spans.push(RSpan::styled(
                        first.to_string(),
                        secondary.add_modifier(Modifier::REVERSED),
                    ));
                    let rest: String = tail.collect();
                    if !rest.is_empty() {
                        spans.push(RSpan::styled(rest, secondary));
                    }
                }
            }
            // Otherwise the caret sits at the end of the input.
            None => spans.push(RSpan::styled(
                self.theme.glyph("palette_cursor").to_string(),
                self.theme.style("palette_cursor", ctx, RainbowInput::None),
            )),
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), rows[0]);
        frame.render_widget(
            Paragraph::new("enter: run   →/tab: accept   ↑↓: history   esc: cancel")
                .style(self.theme.style("help_footer", ctx, RainbowInput::None)),
            rows[1],
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
                        self.theme.glyph("stack_dirty"),
                        self.theme.style("stack_dirty", ctx, RainbowInput::None),
                        "uncommitted",
                    ));
                }
                // Document the reflow glyphs whenever one is showing, labelled by
                // the verb of a stack carrying it (rebase onto upstream, or
                // restack onto an amended parent).
                let verb_for = |role: &str| {
                    self.snapshot
                        .staircases
                        .iter()
                        .find(|s| self.rebase_role(s) == Some(role))
                        .map(Self::reflow_verb)
                };
                if let Some(verb) = verb_for("stack_rebase_clean") {
                    entries.push(
                        self.legend_entry(
                            self.theme.glyph("stack_rebase_clean"),
                            self.theme
                                .style("stack_rebase_clean", ctx, RainbowInput::None),
                            verb,
                        ),
                    );
                }
                if let Some(verb) = verb_for("stack_rebase_conflict") {
                    entries.push(
                        self.legend_entry(
                            self.theme.glyph("stack_rebase_conflict"),
                            self.theme
                                .style("stack_rebase_conflict", ctx, RainbowInput::None),
                            &format!("{verb}!"),
                        ),
                    );
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
                    entries.push(self.legend_entry(
                        self.theme.lead("segment_riser"),
                        secondary,
                        "branch",
                    ));
                }
                // Document the per-commit conflict pin when this stack has one.
                if stair
                    .conflict
                    .as_ref()
                    .is_some_and(|ci| !ci.commit.is_empty())
                {
                    entries.push(self.legend_entry(
                        self.theme.glyph("commit_conflict"),
                        self.theme.style("commit_conflict", ctx, RainbowInput::None),
                        "conflict",
                    ));
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
                if self.files.iter().any(|f| f.status == FileStatus::Message) {
                    entries.push(self.legend_entry(
                        self.theme.glyph("file_message_glyph"),
                        secondary,
                        "message",
                    ));
                }
                for (status, label) in [
                    (FileStatus::Added, "added"),
                    (FileStatus::Modified, "modified"),
                    (FileStatus::Deleted, "deleted"),
                    (FileStatus::Renamed, "renamed"),
                    (FileStatus::Copied, "copied"),
                ] {
                    if self
                        .files
                        .iter()
                        .any(|f| f.status != FileStatus::Message && f.status == status)
                    {
                        entries.push(self.legend_entry(
                            &status.to_string(),
                            self.theme.file_status_style(status, ctx),
                            label,
                        ));
                    }
                }
                if self.files.iter().any(|f| f.added > 0 || f.deleted > 0) {
                    entries.push(self.churn_legend_entry());
                }
            }
            // The Diff and Checks columns carry no glyph vocabulary worth a key.
            ColumnKind::Viewport | ColumnKind::Checks => return Vec::new(),
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
                self.theme
                    .style("legend_label", self.ctx(), RainbowInput::None),
            ),
        ]
    }

    /// The churn key: themed `-` / `+` marks with a shared "lines" label.
    fn churn_legend_entry(&self) -> Vec<RSpan<'static>> {
        let ctx = self.ctx();
        let label = self.theme.style("legend_label", ctx, RainbowInput::None);
        vec![
            RSpan::styled(
                "-",
                self.theme.style("churn_deleted", ctx, RainbowInput::None),
            ),
            RSpan::styled("/", label),
            RSpan::styled(
                "+",
                self.theme.style("churn_added", ctx, RainbowInput::None),
            ),
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
            Paragraph::new(format!(
                "{} {total} findings",
                self.theme.glyph("checks_summary")
            ))
            .style(
                self.theme
                    .style("checks_summary", self.ctx(), RainbowInput::None),
            ),
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

/// Middle-elide `label` to at most `budget` characters, preferring a
/// segment-aware `first/…/last` form for paths and falling back to a
/// character-level middle elision. Elision happens at render time because it
/// depends on the live column width.
fn elide(label: &str, budget: usize) -> String {
    let n = label.chars().count();
    if n <= budget {
        return label.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    if budget == 1 {
        return "…".to_string();
    }
    let segs: Vec<&str> = label.split('/').collect();
    if segs.len() >= 3 {
        let candidate = format!("{}/…/{}", segs[0], segs[segs.len() - 1]);
        if candidate.chars().count() <= budget {
            return candidate;
        }
    }
    let chars: Vec<char> = label.chars().collect();
    let keep = budget - 1; // room for the ellipsis
    let front = keep.div_ceil(2);
    let back = keep - front;
    let head: String = chars[..front].iter().collect();
    let tail: String = chars[chars.len() - back..].iter().collect();
    format!("{head}…{tail}")
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
