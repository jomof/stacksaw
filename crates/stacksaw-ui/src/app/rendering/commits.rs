use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use stacksaw_ssp::types::{RebaseStatus, WORKTREE_OID};

use super::common::{spaces, stat_width};
use super::App;
use crate::app::truncate;
use crate::layout::ColumnKind;
use crate::theme::RainbowInput;

impl App {
    pub(crate) fn draw_commits(&self, frame: &mut Frame, area: Rect) {
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
            let verb = App::reflow_verb(stair);
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
                // glyph + label (the \`commit_worktree\` role), churn right-aligned.
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
                // The \`-N +M\` churn is right-justified against the column edge;
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
    pub(crate) fn selected_conflict_paths(&self) -> Vec<String> {
        let Some(oid) = self.selected_commit_oid() else {
            return Vec::new();
        };
        match self.selected().and_then(|s| s.conflict.as_ref()) {
            Some(ci) if ci.commit == oid => ci.paths.clone(),
            _ => Vec::new(),
        }
    }
}
