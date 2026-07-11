use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use stacksaw_ssp::types::FileStatus;

use super::common::{spaces, split_path, stat_width};
use super::App;
use crate::app::truncate_front;
use crate::layout::ColumnKind;
use crate::theme::RainbowInput;
use crate::viewport::DiffKind;

impl App {
    pub(crate) fn draw_files(&self, frame: &mut Frame, area: Rect) {
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

        // Map rows through the scroll offset ratatui computed (see \`draw_commits\`).
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

    pub(crate) fn draw_diff(&self, frame: &mut Frame, area: Rect) {
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
}
