use super::navigator::step;
use super::{
    contains, remote_from_upstream, App, Divider, ExecTarget, Mode, PaletteState, PendingRun,
    ReshapeOp, ReshapeRequest, RunButton, RunPromptState, MIN_PANE_HEIGHT, WORKTREE_OID,
};
use crate::command::{self, Action, Command};
use crate::layout::{self, ColumnKind};
use crate::viewport::{RunView, Tab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

impl App {
    /// Handle a left click at screen coordinates: focus the clicked column and,
    /// in Stacks/Commits, select the clicked row (§8.2).
    pub fn on_click(&mut self, x: u16, y: u16) {
        // The tab bar rides the bottom pane's top border, which is within the
        // split divider's 1-cell grab tolerance — so test the tab controls
        // first and consume the click, or a tab/close/badge press would be
        // stolen by the divider-drag affordance below.
        enum TabHit {
            Select(usize),
            Close(usize),
            Cancel(usize),
        }
        let tab_hit = {
            let hit = self.hit.borrow();
            if let Some((_, i)) = hit.viewport_closes.iter().find(|(r, _)| contains(*r, x, y)) {
                Some(TabHit::Close(*i))
            } else if let Some((_, i)) =
                hit.viewport_badges.iter().find(|(r, _)| contains(*r, x, y))
            {
                Some(TabHit::Cancel(*i))
            } else if let Some((_, i)) = hit.viewport_tabs.iter().find(|(r, _)| contains(*r, x, y))
            {
                Some(TabHit::Select(*i))
            } else {
                None
            }
        };
        if let Some(tab_hit) = tab_hit {
            self.nav.focused = ColumnKind::Viewport;
            match tab_hit {
                TabHit::Select(i) => {
                    if i < self.viewport.tabs.len() {
                        self.viewport.active = i;
                    }
                }
                TabHit::Close(i) => {
                    if let Some(id) = self.viewport.close(i) {
                        self.runs_to_close.push(id);
                    }
                }
                TabHit::Cancel(i) => {
                    if let Some(Tab::Run(r)) = self.viewport.tabs.get(i) {
                        if r.is_running() {
                            self.runs_to_cancel.push(r.id);
                        }
                    }
                }
            }
            return;
        }
        // Action buttons in a finished command tab's body (Run Again / Close Tab).
        let run_button = {
            let hit = self.hit.borrow();
            hit.viewport_run_buttons
                .iter()
                .find(|(r, _)| contains(*r, x, y))
                .map(|(_, b)| *b)
        };
        if let Some(button) = run_button {
            match button {
                RunButton::Rerun => self.rerun_active(),
                RunButton::Close => self.close_active_tab(),
            }
            return;
        }
        // A press on a draggable divider begins a resize and is consumed, so it
        // never also moves a selection underneath.
        if let Some(div) = self.divider_at(x, y) {
            self.dragging = Some(div);
            return;
        }
        enum Target {
            Focus(ColumnKind),
            Stair(usize),
            Commit(usize),
            File(usize),
            Recent(usize),
            Switch(PathBuf),
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
                    } else if let Some((_, idx)) = hit.recents.iter().find(|(ry, _)| *ry == y) {
                        // First click selects the recent repo (like arrowing to
                        // it); clicking the already-selected row opens it — so a
                        // click never switches out from under you unexpectedly.
                        if let Some(row) = self.recents_others().get(*idx) {
                            if self.nav.selected_recent == Some(*idx) {
                                actions.push(Target::Switch(row.path.clone()));
                            } else {
                                actions.push(Target::Recent(*idx));
                            }
                        }
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
                Target::Focus(k) => self.nav.focused = k,
                Target::Stair(i) => {
                    self.nav.selected_recent = None;
                    self.nav.selected_stair = i;
                    self.nav.selected_commit = self.default_commit_index();
                    self.nav.selected_file = 0;
                }
                Target::Commit(i) => {
                    self.nav.selected_commit = i;
                    self.nav.selected_file = 0;
                }
                Target::File(i) => self.nav.selected_file = i,
                Target::Recent(i) => self.nav.selected_recent = Some(i),
                Target::Switch(path) => self.pending_switch = Some(path),
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
        let over = over.unwrap_or(self.nav.focused);
        match over {
            ColumnKind::Stacks => self.move_stacks(down),
            ColumnKind::Files => {
                let last = self.files.len().saturating_sub(1);
                self.nav.selected_file = step(self.nav.selected_file, down, last);
            }
            ColumnKind::Viewport => self.viewport.scroll_active(down),
            _ => {
                let last = self.commit_count().saturating_sub(1);
                self.nav.selected_commit = step(self.nav.selected_commit, down, last);
                self.nav.selected_file = 0;
            }
        }
    }

    /// Track the pointer: highlight the divider it hovers (the resize
    /// affordance) or, failing that, the selectable row it's over. A no-op while
    /// dragging, where the dragged divider stays lit.
    /// Update hover state for the pointer at `(x, y)`. Returns `true` when the
    /// hovered divider or row actually changed, so the host can skip an
    /// (expensive) redraw for motion that leaves the affordance untouched.
    pub fn on_mouse_move(&mut self, x: u16, y: u16) -> bool {
        if self.dragging.is_some() {
            return false;
        }
        // A tab control sits on the pane's top border, overlapping the split
        // divider's grab zone; when the pointer is over one, it's a tab (not a
        // resize handle), so suppress the divider hover there.
        let divider = if self.over_tab_control(x, y) {
            None
        } else {
            self.divider_at(x, y)
        };
        let row = if divider.is_some() {
            None
        } else {
            self.selectable_row_at(x, y)
        };
        let changed = divider != self.hovered_divider || row != self.hovered_row;
        self.hovered_divider = divider;
        self.hovered_row = row;
        changed
    }

    /// The selectable row under `(x, y)` as `(column, screen_row)`, if the
    /// pointer is over a clickable row (a staircase/recent in Stacks, a commit,
    /// or a file). Diff scrolls rather than selects, so it never hovers.
    fn selectable_row_at(&self, x: u16, y: u16) -> Option<(ColumnKind, u16)> {
        let hit = self.hit.borrow();
        let (kind, _) = hit.columns.iter().find(|(_, r)| contains(*r, x, y))?;
        let on_row = match kind {
            ColumnKind::Stacks => {
                hit.stacks.iter().any(|(ry, _)| *ry == y)
                    || hit.recents.iter().any(|(ry, _)| *ry == y)
            }
            ColumnKind::Commits => hit.commits.iter().any(|(ry, _)| *ry == y),
            ColumnKind::Files => hit.files.iter().any(|(ry, _)| *ry == y),
            _ => false,
        };
        on_row.then_some((*kind, y))
    }

    /// Drag the active divider to `(x, y)`, updating the stored layout fraction.
    /// Column drags reapportion the two neighbors; the split drag resizes the
    /// top band vs. the viewport pane. Both clamp so no pane collapses.
    pub fn on_drag(&mut self, x: u16, y: u16) {
        let Some(div) = self.dragging else { return };
        match div {
            Divider::Column(left, right) => self.drag_column(left, right, x),
            Divider::Split => self.drag_split(y),
        }
    }

    /// End a drag. The host persists [`layout_prefs`](Self::layout_prefs) after.
    pub fn on_mouse_up(&mut self) {
        self.dragging = None;
    }

    /// Reapportion two adjacent expanded columns so their shared divider sits at
    /// screen column `x`, storing each as a fraction of the expanded budget.
    fn drag_column(&mut self, left: ColumnKind, right: ColumnKind, x: u16) {
        let (lrect, rrect, total) = {
            let hit = self.hit.borrow();
            let find = |k| hit.columns.iter().find(|(c, _)| *c == k).map(|(_, r)| *r);
            match (find(left), find(right)) {
                (Some(l), Some(r)) if hit.expanded_total > 0 => (l, r, hit.expanded_total),
                _ => return,
            }
        };
        let pair = lrect.width + rrect.width;
        let min = layout::MIN_EXPANDED;
        if pair < min * 2 {
            return;
        }
        // The divider cell belongs to the left column's right border, so the
        // left width is the pointer column minus the pair's left edge, plus one.
        let new_left = (x.saturating_sub(lrect.x) + 1).clamp(min, pair - min);
        let new_right = pair - new_left;
        self.layout.set_column(left, new_left as f32 / total as f32);
        self.layout
            .set_column(right, new_right as f32 / total as f32);
    }

    /// Resize the top band vs. the viewport pane so the split sits at screen row `y`.
    fn drag_split(&mut self, y: u16) {
        let scene = self.hit.borrow().scene;
        if scene.height <= MIN_PANE_HEIGHT * 2 {
            return;
        }
        let top_h =
            (y.saturating_sub(scene.y) + 1).clamp(MIN_PANE_HEIGHT, scene.height - MIN_PANE_HEIGHT);
        self.layout.split_fraction = Some(top_h as f32 / scene.height as f32);
    }

    /// The divider whose 1-cell line is at (or within one cell of) `(x, y)`, if
    /// any. The small tolerance makes the thin lines easier to grab.
    fn divider_at(&self, x: u16, y: u16) -> Option<Divider> {
        let hit = self.hit.borrow();
        hit.dividers
            .iter()
            .find(|(d, r)| match d {
                // Vertical line: within one column horizontally, on its rows.
                Divider::Column(..) => {
                    y >= r.y && y < r.y + r.height && (x as i32 - r.x as i32).abs() <= 1
                }
                // Horizontal line: within one row vertically, on its columns.
                Divider::Split => {
                    x >= r.x && x < r.x + r.width && (y as i32 - r.y as i32).abs() <= 1
                }
            })
            .map(|(d, _)| *d)
    }

    /// True when `(x, y)` is over a viewport tab button, its close `x`, or its
    /// badge — the controls painted on the pane's top border.
    fn over_tab_control(&self, x: u16, y: u16) -> bool {
        let hit = self.hit.borrow();
        hit.viewport_tabs.iter().any(|(r, _)| contains(*r, x, y))
            || hit.viewport_closes.iter().any(|(r, _)| contains(*r, x, y))
            || hit.viewport_badges.iter().any(|(r, _)| contains(*r, x, y))
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
            Action::CycleFocusNext => self.cycle_focus(true),
            Action::CycleFocusPrev => self.cycle_focus(false),
            Action::Focus(k) => self.nav.focused = k,
            Action::ToggleChecks => {
                self.checks_open = !self.checks_open;
                self.nav.focused = ColumnKind::Checks;
            }
            Action::ToggleZoom => self.zoom = !self.zoom,
            Action::OpenPalette => {
                self.palette = PaletteState::default();
                self.mode = Mode::Palette;
            }
            Action::OpenHelp => self.mode = Mode::Help,
            Action::Activate => self.activate_selection(),
            Action::OpenRunPrompt => self.open_run_prompt(),
            Action::ViewportCloseTab => self.close_active_tab(),
            Action::CycleDiffTheme => self.cycle_diff_theme(),
            Action::RunRerun => self.rerun_active(),
            Action::RunCancel => self.cancel_active(),
            Action::ToggleCapture => {
                if self.viewport.active_is_running() {
                    self.nav.focused = ColumnKind::Viewport;
                    self.mode = Mode::Terminal;
                }
            }
            Action::IndentCommit => self.request_reshape(ReshapeOp::Indent),
            Action::UnindentCommit => self.request_reshape(ReshapeOp::Unindent),
            Action::Push => self.request_push(),
            Action::ArchiveStack => self.request_archive(),
            Action::Undo => self.pending_undo = true,
            Action::Quit => self.should_quit = true,
        }
    }

    /// Queue an indent/unindent of the selected commit for the host to apply.
    /// Only real commits reshape — the virtual "uncommitted changes" row (the
    /// worktree tip) is ignored.
    fn request_reshape(&mut self, op: ReshapeOp) {
        let Some(oid) = self.selected_commit_oid() else {
            return;
        };
        if oid == WORKTREE_OID {
            return;
        }
        self.pending_reshape = Some(ReshapeRequest { oid, op });
    }

    /// Queue an archive of the selected Stacks row (its whole staircase). The
    /// segment branch names are handed to the host, which parks the real ones
    /// out of `refs/heads/`; synthetic rows (a detached-HEAD stack) carry no
    /// real branch and are dropped there.
    fn request_archive(&mut self) {
        // Archive applies to a staircase row, not a recent-repo row.
        if self.nav.selected_recent.is_some() {
            return;
        }
        let Some(stair) = self.selected() else {
            return;
        };
        let branches: Vec<String> = stair
            .segments
            .iter()
            .map(|seg| seg.branch.to_string())
            .collect();
        if !branches.is_empty() {
            self.pending_archive = Some(branches);
        }
    }

    /// Publish the selected stack: queue a Run-tab `git push` of every branch in
    /// the staircase to its remote, so the transfer streams in the viewport with
    /// full output (and can be re-run). `--force-with-lease` keeps reshaped
    /// branches publishable without clobbering unexpected remote moves. Runs in
    /// the physical repo (target oid `None`) since push touches refs, not the
    /// working tree. Applies to a staircase row, never a recent-repo row.
    fn request_push(&mut self) {
        if self.nav.selected_recent.is_some() {
            return;
        }
        let Some(stair) = self.selected() else {
            return;
        };
        let branches: Vec<String> = stair
            .segments
            .iter()
            .map(|seg| seg.branch.to_string())
            .collect();
        if branches.is_empty() {
            return;
        }
        let remote = remote_from_upstream(&stair.upstream);
        let label = stair.name.clone();
        let command = format!(
            "git push --force-with-lease {remote} {}",
            branches.join(" ")
        );
        self.pending_runs.push(PendingRun {
            command,
            target: ExecTarget { oid: None, label },
        });
        self.nav.focused = ColumnKind::Viewport;
    }

    /// Close the active viewport tab, scheduling any command process for
    /// teardown by the host.
    fn close_active_tab(&mut self) {
        let idx = self.viewport.active;
        if let Some(id) = self.viewport.close(idx) {
            self.runs_to_close.push(id);
        }
    }

    /// Re-run the active command tab: relaunch the same command in the same
    /// context and close the old tab.
    fn rerun_active(&mut self) {
        let Some(run) = self.viewport.active_run() else {
            return;
        };
        let pending = PendingRun {
            command: run.command.clone(),
            target: ExecTarget {
                oid: run.target_oid.clone(),
                label: run.label.clone(),
            },
        };
        let id = run.id;
        self.pending_runs.push(pending);
        self.runs_to_close.push(id);
        let idx = self.viewport.active;
        self.viewport.close(idx);
    }

    /// Interrupt the active command tab's process.
    fn cancel_active(&mut self) {
        if let Some(run) = self.viewport.active_run() {
            if run.is_running() {
                self.runs_to_cancel.push(run.id);
            }
        }
    }

    /// Activate the current Stacks selection. On a recent-repo row this requests
    /// a switch to that repo (the host re-execs the window there); on a
    /// staircase it does nothing (staircases activate via the Commits column).
    fn activate_selection(&mut self) {
        let target = self
            .nav.selected_recent
            .and_then(|i| self.recents_others().get(i).map(|r| r.path.clone()));
        if let Some(path) = target {
            self.pending_switch = Some(path);
        }
    }

    /// Cycle focus through the unified ring where each Viewport tab is its own
    /// stop: Stacks → Commits → Files → Viewport(tab 0) → … → Viewport(tab N-1)
    /// → back to Stacks (and the reverse for `forward == false`). Bound to the
    /// arrows (→/←), this is the sole focus-movement idiom — it walks the whole
    /// UI, panes and tabs, as a single ring. With no open tabs the Viewport is
    /// simply skipped. (Checks, an optional overlay column, stays out of this
    /// ring; reach it directly with `5`.)
    fn cycle_focus(&mut self, forward: bool) {
        let n_tabs = self.viewport.tabs.len();
        if forward {
            match self.nav.focused {
                ColumnKind::Stacks => self.nav.focused = ColumnKind::Commits,
                ColumnKind::Commits => self.nav.focused = ColumnKind::Files,
                ColumnKind::Files if n_tabs > 0 => {
                    self.nav.focused = ColumnKind::Viewport;
                    self.viewport.active = 0;
                }
                ColumnKind::Viewport if self.viewport.active + 1 < n_tabs => {
                    self.viewport.active += 1;
                }
                // Files with no tabs, the last tab, or Checks all wrap to Stacks.
                _ => self.nav.focused = ColumnKind::Stacks,
            }
        } else {
            match self.nav.focused {
                ColumnKind::Stacks if n_tabs > 0 => {
                    self.nav.focused = ColumnKind::Viewport;
                    self.viewport.active = n_tabs - 1;
                }
                ColumnKind::Viewport if self.viewport.active > 0 => {
                    self.viewport.active -= 1;
                }
                ColumnKind::Viewport => self.nav.focused = ColumnKind::Files,
                ColumnKind::Files => self.nav.focused = ColumnKind::Commits,
                ColumnKind::Commits => self.nav.focused = ColumnKind::Stacks,
                // Stacks with no tabs, or Checks, wrap to Files.
                _ => self.nav.focused = ColumnKind::Files,
            }
        }
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
    pub(crate) fn palette_results(&self) -> Vec<&'static Command> {
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

    // --- Command launcher (`>`) ------------------------------------------

    /// Install the shell command history (most-recent first) for the launcher.
    pub fn set_command_history(&mut self, history: Vec<String>) {
        self.command_history = history;
    }

    fn open_run_prompt(&mut self) {
        self.run_prompt = RunPromptState::default();
        self.mode = Mode::Run;
    }

    /// The command text currently typed in the `>` launcher.
    pub fn run_prompt_input(&self) -> &str {
        &self.run_prompt.input
    }

    /// The inline autocomplete suggestion (the most-recent history entry that
    /// the current input is a prefix of), or `None`.
    pub fn run_prompt_suggestion(&self) -> Option<String> {
        let input = self.run_prompt.input.as_str();
        if input.is_empty() {
            return None;
        }
        self.command_history
            .iter()
            .find(|c| c.len() > input.len() && c.starts_with(input))
            .cloned()
    }

    pub fn run_prompt_push(&mut self, c: char) {
        self.run_prompt.input.push(c);
        self.run_prompt.hist = None;
    }

    pub fn run_prompt_backspace(&mut self) {
        self.run_prompt.input.pop();
        self.run_prompt.hist = None;
    }

    /// Accept the inline ghost suggestion, if any.
    pub fn run_prompt_accept_ghost(&mut self) {
        if let Some(s) = self.run_prompt_suggestion() {
            self.run_prompt.input = s;
            self.run_prompt.hist = None;
        }
    }

    /// Arrow through history: `older` moves toward older commands.
    pub fn run_prompt_history(&mut self, older: bool) {
        if self.command_history.is_empty() {
            return;
        }
        let last = self.command_history.len() - 1;
        let next = match (self.run_prompt.hist, older) {
            (None, true) => Some(0),
            (None, false) => None,
            (Some(i), true) => Some((i + 1).min(last)),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
        };
        self.run_prompt.hist = next;
        match next {
            Some(i) => self.run_prompt.input = self.command_history[i].clone(),
            None => {
                if !older {
                    self.run_prompt.input.clear();
                }
            }
        }
    }

    /// Confirm the launcher: queue the command with the current context and
    /// return to the viewport. The host drains [`take_pending_runs`].
    pub fn run_prompt_confirm(&mut self) {
        let command = self.run_prompt.input.trim().to_string();
        self.close_overlay();
        if command.is_empty() {
            return;
        }
        self.command_history.retain(|c| c != &command);
        self.command_history.insert(0, command.clone());
        let target = self.exec_target();
        self.pending_runs.push(PendingRun { command, target });
        self.nav.focused = ColumnKind::Viewport;
    }

    /// Resolve the run context from the current focus + selection: in every
    /// focused column the context is the selected commit (Files/Viewport fall
    /// back to the Commits selection, which they already track).
    pub fn exec_target(&self) -> ExecTarget {
        // A Stacks selection means "this whole stack": target its tip — the
        // stack's checked-out state — named by the staircase (its tip branch),
        // exactly as the Stacks column shows it. For the current stack / detached
        // HEAD the tip is the live working tree (or HEAD), so the run stays in
        // the physical checkout instead of needlessly spinning up an ephemeral
        // worktree. Commits/Files target the specific selected commit.
        if self.nav.focused == ColumnKind::Stacks {
            let oid = self.selected_stair_tip_oid();
            let label = self
                .selected()
                .map(|s| s.name.clone())
                .or_else(|| oid.as_ref().map(|o| o.chars().take(7).collect()))
                .unwrap_or_else(|| "HEAD".to_string());
            return ExecTarget { oid, label };
        }
        let oid = self.selected_commit_oid();
        let label = match &oid {
            // The working tree is the live on-disk checkout (no isolated git
            // worktree is created), so name it after the branch that owns it
            // rather than the bare word "worktree", which collides with git's
            // own worktree concept. A live `*` dirty marker is added at render
            // time (see `run_display_label`), so it tracks edits/commits.
            Some(o) if o == WORKTREE_OID => self
                .selected_branch()
                .unwrap_or_else(|| "worktree".to_string()),
            Some(o) => o.chars().take(7).collect(),
            None => "HEAD".to_string(),
        };
        ExecTarget { oid, label }
    }

    /// Whether the working tree currently has uncommitted changes. Reads the
    /// snapshot (which the host refreshes on a timer and after actions), so it
    /// tracks edits and commits made while a run tab is open rather than
    /// freezing the state at launch. Only the checked-out staircase can be
    /// dirty, so any dirty staircase means the live worktree is dirty.
    fn repo_dirty(&self) -> bool {
        self.snapshot.staircases.iter().any(|s| s.dirty)
    }

    /// The label to render for a run tab: the stored label, plus a live `*`
    /// when the run targets the working tree and the tree is currently dirty.
    pub(crate) fn run_display_label(&self, run: &RunView) -> String {
        if run.target_oid.as_deref() == Some(WORKTREE_OID) && self.repo_dirty() {
            format!("{}*", run.label)
        } else {
            run.label.clone()
        }
    }

    // --- Terminal capture ------------------------------------------------

    /// Forward a key to the active running terminal, or release capture on the
    /// reserved chord (`Ctrl-a`). `Esc` is forwarded (programs like vim need
    /// it), so it can't be the release key.
    pub fn terminal_input(&mut self, key: &KeyEvent) {
        if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.mode = Mode::Normal;
            return;
        }
        if let Some(run) = self.viewport.active_run() {
            if run.is_running() {
                let bytes = run.key_bytes(key);
                let id = run.id;
                if !bytes.is_empty() {
                    self.pty_input.push((id, bytes));
                }
                return;
            }
        }
        // Nothing live to receive input; drop back to navigation.
        self.mode = Mode::Normal;
    }

    /// If capture is active but the terminal has exited, leave capture.
    pub fn refresh_capture(&mut self) {
        if self.mode == Mode::Terminal && !self.viewport.active_is_running() {
            self.mode = Mode::Normal;
        }
    }
}
