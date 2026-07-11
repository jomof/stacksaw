use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::App;
use super::{Divider, Mode};
use super::{DEFAULT_SPLIT_FRACTION, MIN_PANE_HEIGHT};
use crate::layout::{self, ColumnKind};
use crate::theme::RainbowInput;

mod commits;
mod common;
mod files;
mod overlays;
mod stacks;
mod viewport;

use self::common::stitch_dividers;

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
        // Paint the scene background first (theme \`[base].bg\`); widgets that set
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

    /// Split the frame into the top column band and the bottom viewport pane.
    ///
    /// Zooming the Viewport gives it the whole frame. Zooming a *top* column
    /// keeps the normal split so the viewport pane stays visible — the zoom just
    /// collapses that column's siblings to spines inside the top band (handled
    /// in \`draw_top_columns\`), giving the focused column the band's full width.
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
        // fills \`area\` exactly with no trailing blank.
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
        // top/bottom edges as \`┐─\`/\`┘─\` rather than a connected tee. Stitch
        // those junctions into \`┬\`/\`┴\` for clean elbows (the user-visible fix).
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
    /// bottom borders so the band reads as one continuous frame; \`left_border\`
    /// additionally closes the band's left edge for the first slot. The shared
    /// divider with the neighbor to the right is this spine's right border, with
    /// its junctions stitched into tees by [\`stitch_dividers\`].
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

    /// Draw an expanded column. \`left_border\` is \`false\` for columns that abut a
    /// neighbor on their left (in the top band) so the shared divider is a
    /// single line; standalone columns (viewport pane, deck mode) pass \`true\`.
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
}
