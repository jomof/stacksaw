use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::App;
use crate::command;
use crate::layout::ColumnKind;
use crate::theme::{ChipKind, RainbowInput};
use stacksaw_ssp::types::{FileStatus, WORKTREE_OID};

impl App {
    /// The always-on hint bar: a projection of the command registry showing the
    /// most relevant keys for the focused column (§8.2).
    /// The bottom hint bar: contextual commands in \`hint_rank\` priority order,
    /// fitted to the available width. Rather than clipping a hint mid-word, whole
    /// low-priority items drop from the end; \`Help\` is pinned to the far right as
    /// the escape hatch to the full list, and a \`…\` signals that hints were
    /// dropped. (§8.2)
    pub(crate) fn draw_hint_bar(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
        let sep = format!(" {} ", self.theme.glyph("hint_separator"));
        let sep_w = sep.chars().count();

        let fit = command::fit_hints(self.focus(), area.width as usize, sep_w);

        // Final left-to-right order: fitted hints, a "…" if any were dropped,
        // then pinned Help. \`None\` marks the ellipsis slot.
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

    /// Tint the hovered row with the \`row_hover\` bar, so the row under the
    /// pointer reads as clickable and previews what a click would select.
    /// Skipped when the hovered row *is* the current selection or no longer
    /// maps to a live row (e.g. the layout shifted since the last move).
    pub(crate) fn paint_hovered_row(&self, frame: &mut Frame) {
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
    /// \`divider_active\` color so it reads as a grabbable resize handle.
    pub(crate) fn paint_active_divider(&self, frame: &mut Frame, active: super::Divider) {
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

    /// Reserve the bottom inner row for [\`column_legend\`] when it has content and
    /// there is room; returns the (possibly shortened) area left for the body.
    pub(crate) fn draw_legend(&self, frame: &mut Frame, inner: Rect, kind: ColumnKind) -> Rect {
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

    /// A legend of the glyphs *currently* displayed in \`kind\`'s column, so the
    /// symbols are self-explanatory. Only glyphs actually present in the current
    /// view are listed; returns an empty vec when there is nothing to explain.
    pub(crate) fn column_legend(&self, kind: ColumnKind) -> Vec<RSpan<'static>> {
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
                        .map(App::reflow_verb)
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

    /// One legend item: a styled \`glyph\` followed by the label in the theme's
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

    /// The churn key: themed \`-\` / \`+\` marks with a shared "lines" label.
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

    pub(crate) fn draw_checks(&self, frame: &mut Frame, area: Rect) {
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

/// Split a path into \`(dir, filename)\`. \`dir\` keeps a trailing component only
/// (no leading slash); it is empty for a top-level file.
pub(crate) fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Rendered width (in cells) of the \`-N +M\` churn annotation, matching the
/// spans built by [\`App::churn_spans\`]: \`-{deleted}\` and \`+{added}\`, each
/// suppressed when zero, joined by a single space when both appear.
pub(crate) fn stat_width(added: u32, deleted: u32) -> usize {
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

/// A run of \`n\` blank cells, used to right-justify trailing content.
pub(crate) fn spaces(n: usize) -> RSpan<'static> {
    RSpan::raw(" ".repeat(n))
}

/// Middle-elide \`label\` to at most \`budget\` characters, preferring a
/// segment-aware \`first/…/last\` form for paths and falling back to a
/// character-level middle elision. Elision happens at render time because it
/// depends on the live column width.
pub(crate) fn elide(label: &str, budget: usize) -> String {
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
pub(crate) fn legend_width(spans: &[RSpan<'static>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// Flatten legend entries into a single span run, separated by two spaces.
pub(crate) fn join_legend(entries: Vec<Vec<RSpan<'static>>>) -> Vec<RSpan<'static>> {
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
/// each boundary reads as a connected \`┬\`/\`┴\` tee instead of two separate block
/// corners. \`chunks\` are the per-slot rects, left→right; the divider for every
/// slot but the last sits on that slot's right-border column.
pub(crate) fn stitch_dividers(frame: &mut Frame, area: Rect, chunks: &[Rect]) {
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

/// A \`w\`×\`h\` rectangle centered within \`area\` (clamped to fit). Used for the
/// help/palette overlays.
pub(crate) fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}
