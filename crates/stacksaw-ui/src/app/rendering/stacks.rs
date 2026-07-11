use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{HighlightSpacing, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use stacksaw_rainbox::temporal_decay;
use stacksaw_ssp::types::{RebaseStatus, Staircase};
use std::collections::HashMap;

use super::common::{elide, legend_width};
use super::App;
use crate::app::RecentRowView;
use crate::layout::ColumnKind;
use crate::theme::RainbowInput;

impl App {
    pub(crate) fn draw_stacks(&self, frame: &mut Frame, area: Rect) {
        // With other repos in the MRU, the column becomes a ledger: the current
        // repo as a dot-less header line (like the \`upstream\` line in Commits),
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
    /// filling \`area\` (which may be a sub-rect below the current-repo header).
    pub(crate) fn draw_stacks_flat(&self, frame: &mut Frame, area: Rect) {
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

        // Map rows through the scroll offset ratatui computed (see \`draw_commits\`).
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
    pub(crate) fn draw_stacks_ledger(&self, frame: &mut Frame, area: Rect) {
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

        // Current repo: a dot-less header line in the \`upstream\` style. First =
        // most-recent, so no marker is needed to say "you are here".
        // The branch markers form a column for the other repos: reserve its width
        // (the widest branch across those rows) and left-justify each branch
        // within it, so all the branch icons align on one column. The current
        // repo's branch aligns separately on its own.
        let others_branch_col = others
            .iter()
            .filter_map(|r| self.recent_branch_text(r, Some(crate::app::RECENTS_MAX_BRANCH)))
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
        // offset ratatui computed (see \`draw_commits\`).
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

    /// One flush-left ledger line for a non-current repo: a dim \`parent\` prefix,
    /// its \`label\`, and a dimmer trailing \`⎇ branch\` marker — all faded toward
    /// the background by MRU \`rank\` so older repos recede (§8.3 relevance
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
        // The whole row is one uniform color: the \`recent\` identity hue keyed by
        // the branch name, faded by MRU age (§8.3 relevance). The repo name sits
        // flush left; the directory is right-justified so it ends one space
        // before the branch marker, which is itself left-justified in a fixed
        // column at the right edge (\`branch_col\`) so every marker aligns.
        let ctx = self.ctx();
        let relevance = self.recent_relevance(rank);
        let hue = self
            .theme
            .style_at("recent_row", ctx, RainbowInput::Key(key), relevance);

        let limit = if row.current {
            None
        } else {
            Some(crate::app::RECENTS_MAX_BRANCH)
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
        // one-space gap; the branch then starts at \`left_region + 1\`.
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

    /// The relevance for a recents row at MRU \`rank\` (0 = most-recent = full
    /// relevance). Follows §8.3's temporal decay — rank standing in for age —
    /// with a floor so a deep row still keeps its identity/legibility rather
    /// than collapsing into the background.
    fn recent_relevance(&self, rank: usize) -> f32 {
        temporal_decay(rank as f32, crate::app::RECENTS_HALF_LIFE)
            .max(crate::app::RECENTS_RELEVANCE_FLOOR)
    }

    /// The rainbow-identity key for a recents row (§8.3): the **branch name**
    /// when that branch is checked out in more than one known repo (so shared
    /// branches share a hue), otherwise the repo's **path within its root**
    /// (\`label\`) — never the root/parent name, which shouldn't drive color.
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
    /// ledger (current + others), so [\`recent_identity\`] can tell a shared
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

    /// Split a recents row into its left part (\`"parent label"\`, or just
    /// \`label\` for a loose repo) and its right part (\`"{glyph}branch"\`, the
    /// branch elided to the given limit so a long name can't widen the
    /// column unbounded). The right part is \`None\` when the HEAD is unknown.
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

    /// The trailing \`"{glyph}branch"\` marker for a recents row, branch elided to
    /// the given limit so a long name can't widen the column unbounded. \`None\`
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
    /// left-justified in a fixed-width column (\`branch_col\`) at the right edge so
    /// it aligns with the ledger rows below. When the row is too wide the *left*
    /// part is elided so the branch still lands at the column start (rather than
    /// gluing it after a truncated label).
    fn recent_row_layout(&self, row: &RecentRowView, width: usize, branch_col: usize) -> String {
        let limit = if row.current {
            None
        } else {
            Some(crate::app::RECENTS_MAX_BRANCH)
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
            Some(crate::app::RECENTS_MAX_BRANCH)
        };
        let (left, branch) = self.recent_row_parts(row, limit);
        left.chars().count() + branch.map(|b| 2 + b.chars().count()).unwrap_or(0)
    }

    /// The theme role for a staircase's rebase chip, or \`None\` when no rebase is
    /// indicated (in sync, or the probe reached no verdict). Clean = a free
    /// rebase is available; Conflict = a rebase would need manual resolution.
    pub(crate) fn rebase_role(&self, s: &Staircase) -> Option<&'static str> {
        if !App::needs_reflow(s) {
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
    pub(crate) fn needs_reflow(s: &Staircase) -> bool {
        s.segments.iter().any(|seg| seg.stale) || s.behind > 0
    }

    /// The verb for a stack's reflow chip: \`restack\` when a stale child dangles
    /// on an amended parent, else \`rebase\` onto its upstream.
    pub(crate) fn reflow_verb(s: &Staircase) -> &'static str {
        if s.segments.iter().any(|seg| seg.stale) {
            "restack"
        } else {
            "rebase"
        }
    }

    pub(crate) fn stair_line(&self, s: &Staircase) -> Line<'static> {
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
        // tab's \`main*\`) rather than a pencil — the Commits worktree row keeps
        // the pencil via the \`dirty\` role.
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
    pub(crate) fn recents_has_others(&self) -> bool {
        self.recents.rows.iter().any(|r| !r.current)
    }

    /// Outer width the Stacks column needs to show its widest row without
    /// truncation: highlight marker + name + the \`↑a ↓b\` counters + borders,
    /// and wide enough that the glyph legend on the bottom row fits too.
    pub(crate) fn stacks_content_width(&self) -> u16 {
        const MARKER: usize = 2; // "▶ "
        const BORDERS: usize = 2; // left + right column borders
        let content = self
            .snapshot
            .staircases
            .iter()
            // Measure the fully-rendered row (staircase glyph + name + counters +
            // \`(n branches)\` + dirty marker) so the column never truncates it.
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
    /// \`0\` when the recents ledger is hidden (no other repos).
    pub(crate) fn recents_content_width(&self) -> usize {
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
}
