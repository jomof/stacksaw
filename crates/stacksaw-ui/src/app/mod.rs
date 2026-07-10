//! The TUI application: state + ratatui rendering of the column scene (§8).

use std::cell::{Cell, RefCell};
use std::mem;
use std::path::PathBuf;

use ratatui::layout::Rect;
use stacksaw_rainbox::Background;
use stacksaw_ssp::types::{
    CommitSummary, FileEntry, FileStatus, Snapshot, Staircase, WORKTREE_OID,
};

pub mod rendering;
pub mod state;

pub mod handlers;
pub use self::state::{
    Divider, ExecTarget, Hit, Mode, PaletteState, PendingRun, RecentRowView, RecentsView,
    ReshapeOp, ReshapeRequest, RunButton, RunPromptState, ViewState,
};

pub(crate) const MESSAGE_PATH: &str = "commit message";
pub(crate) const DEFAULT_SPLIT_FRACTION: f32 = 0.45;
pub(crate) const MIN_PANE_HEIGHT: u16 = 4;
pub(crate) const RECENTS_HALF_LIFE: f32 = 3.0;
pub(crate) const RECENTS_RELEVANCE_FLOOR: f32 = 0.45;
pub(crate) const RECENTS_MAX_BRANCH: usize = 24;

use crate::command;
use crate::highlight::theme_names;
use crate::layout::{ColumnKind, LayoutPrefs};
use crate::theme::{ChipKind, Ctx, GlyphSet, RainbowInput, Theme};
use crate::viewport::{RunContext, RunView, Tab, Viewport};
use ratatui::text::Span as RSpan;

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
    pub truecolor: bool,
    pub files: Vec<FileEntry>,
    pub(crate) loaded_oid: Option<String>,
    pub(crate) viewport: Viewport,
    pub(crate) mode: Mode,
    pub(crate) palette: PaletteState,
    pub(crate) run_prompt: RunPromptState,
    pub(crate) command_history: Vec<String>,
    pub(crate) pending_runs: Vec<PendingRun>,
    pub(crate) pty_input: Vec<(u64, Vec<u8>)>,
    pub(crate) runs_to_cancel: Vec<u64>,
    pub(crate) runs_to_close: Vec<u64>,
    pub(crate) syntax_theme_override: Option<String>,
    pub(crate) pending_reshape: Option<ReshapeRequest>,
    pub(crate) pending_archive: Option<Vec<String>>,
    pub(crate) pending_undo: bool,
    pub(crate) viewport_content_size: Cell<(u16, u16)>,
    pub should_quit: bool,
    pub(crate) selected_recent: Option<usize>,
    pub pending_switch: Option<PathBuf>,
    pub(crate) hit: RefCell<Hit>,
    pub(crate) layout: LayoutPrefs,
    pub(crate) hovered_divider: Option<Divider>,
    pub(crate) hovered_row: Option<(ColumnKind, u16)>,
    pub(crate) dragging: Option<Divider>,
    pub(crate) recents: RecentsView,
    pub(crate) theme: Theme,
}

impl App {
    pub fn new(snapshot: Snapshot) -> Self {
        let mut app = App {
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
            viewport: Viewport::default(),
            mode: Mode::Normal,
            palette: PaletteState::default(),
            run_prompt: RunPromptState::default(),
            command_history: Vec::new(),
            pending_runs: Vec::new(),
            pty_input: Vec::new(),
            runs_to_cancel: Vec::new(),
            runs_to_close: Vec::new(),
            syntax_theme_override: None,
            pending_reshape: None,
            pending_archive: None,
            pending_undo: false,
            viewport_content_size: Cell::new((80, 24)),
            should_quit: false,
            selected_recent: None,
            pending_switch: None,
            hit: RefCell::new(Hit::default()),
            layout: LayoutPrefs::default(),
            hovered_divider: None,
            hovered_row: None,
            dragging: None,
            recents: RecentsView::default(),
            theme: Theme::load(),
        };
        // Open the Commits column on the stack's tip (ToT), not its base.
        app.selected_commit = app.default_commit_index();
        app
    }

    /// Choose the glyph set (Unicode default or Nerd Font), rebuilding the
    /// theme with the matching glyphs. The host resolves this from config/env
    /// before the first draw; colors and layout are unaffected.
    pub fn set_glyph_set(&mut self, glyphs: GlyphSet) {
        self.theme = Theme::load_with(glyphs);
    }

    /// Install the user's persisted divider layout (§8.2). The host loads this
    /// from disk and applies it before the first draw.
    pub fn set_layout_prefs(&mut self, layout: LayoutPrefs) {
        self.layout = layout;
    }

    /// The current divider layout, for the host to persist after a drag.
    pub fn layout_prefs(&self) -> LayoutPrefs {
        self.layout.clone()
    }

    /// Install the recent-repositories list to show under the staircases.
    pub fn set_recents(&mut self, recents: RecentsView) {
        self.recents = recents;
    }

    /// Capture the current navigation state so it can be restored after a
    /// relaunch. `selected_file` must be re-applied by the host *after* the
    /// Files column reloads, since [`set_files`](Self::set_files) resets it.
    pub fn view_state(&self) -> ViewState {
        ViewState {
            focused: self.focused,
            selected_stair: self.selected_stair,
            selected_commit: self.selected_commit,
            selected_file: self.selected_file,
            zoom: self.zoom,
            checks_open: self.checks_open,
            layout: self.layout.clone(),
        }
    }

    fn selected(&self) -> Option<&Staircase> {
        self.snapshot.staircases.get(self.selected_stair)
    }

    /// The current focus for the command registry: the focused column plus its
    /// sub-contexts — which Stacks row kind is selected (staircase vs recent) and
    /// which viewport contributor is active (Diff vs Run tab). An empty viewport
    /// falls back to the Diff contributor.
    pub fn focus(&self) -> command::Focus {
        let stacks_row = if self.selected_recent.is_some() {
            command::StacksRow::Recent
        } else {
            command::StacksRow::Staircase
        };
        let viewport = match self.viewport.tabs.get(self.viewport.active) {
            Some(Tab::Run(_)) => command::ViewportKind::Run,
            _ => command::ViewportKind::Diff,
        };
        command::Focus {
            column: self.focused,
            stacks_row,
            viewport,
        }
    }

    /// The branch of the segment that owns the currently selected commit — the
    /// branch a Stacks selection means (walking segments in the same flat order
    /// as [`selected_commit_oid`](Self::selected_commit_oid)).
    fn selected_branch(&self) -> Option<String> {
        let stair = self.selected()?;
        let mut idx = self.selected_commit;
        for seg in &stair.segments {
            if idx < seg.commits.len() {
                return Some(seg.branch.to_string());
            }
            idx -= seg.commits.len();
        }
        None
    }

    /// The overall tip commit of the selected staircase — what a Stacks
    /// selection ("this whole stack") runs against. The Stacks column shows one
    /// row per staircase (labeled by its tip branch), so a run targets the top
    /// of the stack, not whatever interior commit the Commits cursor sits on.
    /// Commits are stored oldest-first, so the tip is the last commit of the last
    /// segment; when the tree is dirty that is the virtual worktree commit (the
    /// live on-disk state). For the checked-out stack this keeps the run in the
    /// physical checkout rather than isolating an interior commit in a worktree.
    fn selected_stair_tip_oid(&self) -> Option<String> {
        self.selected()?
            .segments
            .last()
            .and_then(|seg| seg.commits.last())
            .map(|c| c.oid.clone())
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
    /// is one selection away and shows in the Diff view (§8.1).
    pub fn set_files(&mut self, oid: String, files: Vec<FileEntry>) {
        self.loaded_oid = Some(oid);
        self.files = Vec::with_capacity(files.len() + 1);
        self.files.push(FileEntry {
            status: FileStatus::Message,
            path: MESSAGE_PATH.to_string(),
            ..Default::default()
        });
        self.files.extend(files);
        self.selected_file = 0;
    }

    /// True when the selected Files row is the virtual commit-message entry, so
    /// the Diff view should show the full message rather than a patch.
    pub fn selected_file_is_message(&self) -> bool {
        self.files
            .get(self.selected_file)
            .map(|f| f.status == FileStatus::Message)
            .unwrap_or(false)
    }

    /// Path of the currently selected file, if any.
    pub fn selected_file_path(&self) -> Option<String> {
        self.files.get(self.selected_file).map(|f| f.path.clone())
    }

    /// True when the selected file was added by this commit, so the Diff view
    /// should show its full content rather than an all-`+` patch.
    pub fn selected_file_is_added(&self) -> bool {
        self.files
            .get(self.selected_file)
            .map(|f| f.status == FileStatus::Added)
            .unwrap_or(false)
    }

    /// The `(oid, path)` whose diff needs loading, if the selection has moved
    /// off the currently loaded diff. `None` means the Diff view is current.
    pub fn diff_needing_load(&self) -> Option<(String, String)> {
        let oid = self.selected_commit_oid()?;
        let path = self.selected_file_path()?;
        let key = (oid, path);
        (self.viewport.diff().loaded_key.as_ref() != Some(&key)).then_some(key)
    }

    /// Install the diff text for `(oid, path)` (called by the host). `raw` marks
    /// the text as plain file content (added file / commit message) rather than
    /// a unified patch. The text is syntax-highlighted (by `path`) and cached as
    /// rendered rows in the Diff contributor, reopening its tab as the leftmost
    /// tab if it had been closed.
    pub fn set_diff(&mut self, oid: String, path: String, text: &str, raw: bool) {
        let is_message = self.selected_file_is_message();
        let truecolor = self.truecolor;
        let syntax_theme = self.effective_syntax_theme();
        self.viewport.diff_mut_open().set_diff(
            oid,
            path,
            text,
            raw,
            is_message,
            truecolor,
            &syntax_theme,
        );
    }

    /// The syntect theme in effect for Diff highlighting: the user's chosen
    /// override if any, else the UI theme's configured default.
    fn effective_syntax_theme(&self) -> String {
        self.syntax_theme_override
            .clone()
            .unwrap_or_else(|| self.theme.syntax_theme().to_string())
    }

    /// Advance to the next built-in syntect theme and re-highlight the loaded
    /// diff in place (§8.5). The choice persists for later diffs this session.
    fn cycle_diff_theme(&mut self) {
        let names = theme_names();
        if names.is_empty() {
            return;
        }
        let current = self.effective_syntax_theme();
        let idx = names.iter().position(|n| *n == current).unwrap_or(0);
        self.syntax_theme_override = Some(names[(idx + 1) % names.len()].clone());
        let truecolor = self.truecolor;
        let theme = self.effective_syntax_theme();
        self.viewport.diff_mut().restyle(truecolor, &theme);
        self.focused = ColumnKind::Viewport;
    }

    /// Current vertical scroll offset of the Diff viewport (rendered rows).
    pub fn diff_scroll(&self) -> u16 {
        self.viewport.diff().scroll
    }

    /// Move the selection within the currently focused column (§8.2). Selecting
    /// a different stack or commit resets the dependent selections below it.
    pub fn move_selection(&mut self, down: bool) {
        match self.focused {
            ColumnKind::Stacks => self.move_stacks(down),
            ColumnKind::Files => {
                let last = self.files.len().saturating_sub(1);
                self.selected_file = step(self.selected_file, down, last);
            }
            // The viewport scrolls its active tab rather than moving a commit.
            ColumnKind::Viewport => self.viewport.scroll_active(down),
            _ => {
                let last = self.commit_count().saturating_sub(1);
                self.selected_commit = step(self.selected_commit, down, last);
                self.selected_file = 0;
            }
        }
    }

    /// Move the stack selection, resetting the commit/file selections beneath.
    /// Always lands on a staircase (used by `J`/`K` and scroll), so it also
    /// pulls the cursor back out of the recents ledger.
    pub fn move_stair(&mut self, down: bool) {
        self.selected_recent = None;
        let last = self.snapshot.staircases.len().saturating_sub(1);
        self.selected_stair = step(self.selected_stair, down, last);
        self.selected_commit = self.default_commit_index();
        self.selected_file = 0;
    }

    /// Move the Stacks-column cursor through the combined list of staircases
    /// then recent-repo rows: arrowing past the last staircase drops into the
    /// ledger, and arrowing back up returns to the staircases. Selecting a
    /// staircase drives the Commits column as usual; landing on a recent row
    /// only highlights it (activation switches repos). Falls back to plain
    /// staircase movement when there are no other repos.
    pub fn move_stacks(&mut self, down: bool) {
        let n_stairs = self.snapshot.staircases.len();
        let n_others = self.recents_others().len();
        if n_others == 0 {
            self.move_stair(down);
            return;
        }
        let total = n_stairs + n_others;
        let pos = match self.selected_recent {
            Some(i) => n_stairs + i.min(n_others - 1),
            None => self.selected_stair.min(n_stairs.saturating_sub(1)),
        };
        let next = if down {
            (pos + 1).min(total - 1)
        } else {
            pos.saturating_sub(1)
        };
        if next < n_stairs {
            self.selected_recent = None;
            if next != self.selected_stair {
                self.selected_stair = next;
                self.selected_commit = self.default_commit_index();
                self.selected_file = 0;
            }
        } else {
            self.selected_recent = Some(next - n_stairs);
        }
    }

    /// Number of commits in the selected staircase (for clamping selection).
    fn commit_count(&self) -> usize {
        self.selected()
            .map(|s| s.segments.iter().map(|seg| seg.commits.len()).sum())
            .unwrap_or(0)
    }

    /// Clamp the Stacks/Commits selections into the current snapshot and drop any
    /// stale Files/Diff when nothing is selectable. A rebuilt snapshot (a refresh
    /// or a mutation) can shrink the selected stack — e.g. the worktree row
    /// disappears when the tree goes clean — leaving `selected_commit` dangling
    /// past the end. Without this, `selected_commit_oid` returns `None`, so the
    /// lazy Files/Diff loaders short-circuit and a prior commit's content lingers
    /// on screen even though clicking a file can no longer refresh it. The host
    /// calls this after every snapshot swap.
    pub fn reconcile_selection(&mut self) {
        let stairs = self.snapshot.staircases.len();
        self.selected_stair = self.selected_stair.min(stairs.saturating_sub(1));
        let commits = self.commit_count();
        if commits == 0 {
            self.selected_commit = 0;
        } else {
            self.selected_commit = self.selected_commit.min(commits - 1);
        }
        // With no browsable commit, the Files/Diff have nothing to describe.
        if self.selected_commit_oid().is_none()
            && (self.loaded_oid.is_some()
                || !self.files.is_empty()
                || self.viewport.diff().loaded_key.is_some())
        {
            self.loaded_oid = None;
            self.files.clear();
            self.selected_file = 0;
            self.viewport.diff_mut().clear();
        }
    }

    /// The default commit selection for the selected staircase: its tip (ToT) —
    /// the last commit in flat order. The Commits column renders the base at the
    /// top and the tip at the bottom, so opening on the tip matches "the latest
    /// commit on the branch" rather than its oldest ancestor.
    fn default_commit_index(&self) -> usize {
        self.commit_count().saturating_sub(1)
    }

    // --- Host-facing command lifecycle -----------------------------------

    /// Commands the user has confirmed; the host resolves context and spawns.
    pub fn take_pending_runs(&mut self) -> Vec<PendingRun> {
        mem::take(&mut self.pending_runs)
    }

    /// Queued PTY input to forward to command terminals.
    pub fn take_pty_input(&mut self) -> Vec<(u64, Vec<u8>)> {
        mem::take(&mut self.pty_input)
    }

    /// Command tab ids to interrupt (SIGINT).
    pub fn take_runs_to_cancel(&mut self) -> Vec<u64> {
        mem::take(&mut self.runs_to_cancel)
    }

    /// Command tab ids to kill and reclaim (tab closed).
    pub fn take_runs_to_close(&mut self) -> Vec<u64> {
        mem::take(&mut self.runs_to_close)
    }

    /// A queued reshape (indent/unindent) for the host to apply, if any.
    pub fn take_pending_reshape(&mut self) -> Option<ReshapeRequest> {
        self.pending_reshape.take()
    }

    /// Branch names of a stack the user asked to archive, if any (consumed).
    pub fn take_pending_archive(&mut self) -> Option<Vec<String>> {
        self.pending_archive.take()
    }

    /// Whether the user asked to undo the last reshape (consumes the request).
    pub fn take_pending_undo(&mut self) -> bool {
        mem::take(&mut self.pending_undo)
    }

    /// Open a command terminal tab (called by the host after spawning the PTY).
    // Each argument is a distinct spawn parameter forwarded straight to
    // `RunView::new`; grouping them into a struct would only add indirection.
    #[allow(clippy::too_many_arguments)]
    pub fn open_run(
        &mut self,
        id: u64,
        command: String,
        label: String,
        target_oid: Option<String>,
        context: RunContext,
        rows: u16,
        cols: u16,
    ) {
        self.viewport.open_run(RunView::new(
            id, command, label, target_oid, context, rows, cols,
        ));
        self.focused = ColumnKind::Viewport;
    }

    /// Feed streamed PTY bytes into a command terminal.
    pub fn push_pty_output(&mut self, id: u64, bytes: &[u8]) {
        if let Some(run) = self.viewport.find_run_mut(id) {
            run.push(bytes);
        }
    }

    /// Record that a command's process exited.
    pub fn finish_run(&mut self, id: u64, code: i32) {
        if let Some(run) = self.viewport.find_run_mut(id) {
            run.finish(code);
        }
    }

    /// Resize every command terminal's emulator to the current viewport content
    /// size, returning `(id, rows, cols)` for any that changed so the host can
    /// resize the underlying PTY.
    pub fn sync_run_sizes(&mut self) -> Vec<(u64, u16, u16)> {
        let (cols, rows) = self.viewport_content_size.get();
        let mut changed = Vec::new();
        for tab in &mut self.viewport.tabs {
            if let Tab::Run(run) = tab {
                if run.size() != (rows, cols) {
                    run.set_size(rows, cols);
                    changed.push((run.id, rows, cols));
                }
            }
        }
        changed
    }

    /// True while any command terminal is still executing (drives a tighter
    /// event-loop poll so streamed output stays responsive).
    pub fn has_active_runs(&self) -> bool {
        self.viewport.tabs.iter().any(|t| match t {
            Tab::Run(r) => r.is_running(),
            _ => false,
        })
    }

    /// The content-area size (cols, rows) the viewport last rendered at.
    pub fn viewport_content_size(&self) -> (u16, u16) {
        self.viewport_content_size.get()
    }

    /// Index of the active viewport tab (exposed for tests/host introspection).
    pub fn viewport_active_tab(&self) -> usize {
        self.viewport.active
    }

    /// The other (non-current) repos, in MRU order (most-recent first).
    pub(crate) fn recents_others(&self) -> Vec<&RecentRowView> {
        self.recents.rows.iter().filter(|r| !r.current).collect()
    }
}
pub(crate) fn truncate_front(s: &str, max: usize) -> String {
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

/// True when screen point `(x, y)` lies inside `rect`.
pub(crate) fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// Step an index toward `last` (down) or `0` (up), saturating at the bounds.
/// Derive the push remote from a staircase's `upstream`, which may be a full
/// tracking ref (`refs/remotes/origin/main`), an already-short remote-tracking
/// name (`origin/main`), or a local upstream (`refs/heads/main`). Returns the
/// remote name, defaulting to `origin` when the upstream is local or empty.
pub(crate) fn remote_from_upstream(upstream: &str) -> String {
    let short = upstream.strip_prefix("refs/remotes/").unwrap_or(upstream);
    // A local upstream (still `refs/…`, e.g. `refs/heads/main`) has no remote.
    if short.starts_with("refs/") {
        return "origin".to_string();
    }
    short
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("origin")
        .to_string()
}

pub(crate) fn step(cur: usize, down: bool, last: usize) -> usize {
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
pub(crate) fn chip_specs(c: &CommitSummary) -> Vec<(ChipKind, Option<u32>)> {
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

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

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
