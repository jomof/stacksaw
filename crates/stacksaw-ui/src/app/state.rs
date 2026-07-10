use crate::layout::{ColumnKind, LayoutPrefs};
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which interaction mode the UI is in. Overlays capture input until dismissed
/// (§8.2 command palette / help).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Normal,
    Help,
    Palette,
    /// The `>` command launcher: typing a shell command to run.
    Run,
    /// A focused command terminal is capturing input, forwarding keys to its
    /// PTY until the release chord (`Ctrl-a`).
    Terminal,
}

/// A snapshot of the user's navigation state, small enough to hand across a
/// process relaunch (§8.2 dev self-reload). Everything else (loaded files,
/// diffs, color depth, overlays) is re-derived on startup, so only the
/// selections and layout toggles are carried.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ViewState {
    pub focused: ColumnKind,
    pub selected_stair: usize,
    pub selected_commit: usize,
    pub selected_file: usize,
    pub zoom: bool,
    pub checks_open: bool,
    /// Dragged divider positions, so an in-progress resize survives a reload.
    #[serde(default)]
    pub layout: LayoutPrefs,
}

/// A width-independent view model of the recent-repositories ledger shown in
/// the Stacks column (§8.1).
#[derive(Debug, Clone, Default)]
pub struct RecentsView {
    pub rows: Vec<RecentRowView>,
}

/// One repository row in the recents ledger.
#[derive(Debug, Clone)]
pub struct RecentRowView {
    pub path: PathBuf,
    pub parent: Option<String>,
    pub label: String,
    pub branch: Option<String>,
    pub current: bool,
}

/// Command-palette state: the fuzzy query and the highlighted result row.
#[derive(Default)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
}

/// The `>` command-launcher state.
#[derive(Default)]
pub struct RunPromptState {
    pub input: String,
    /// Cursor into [`App::command_history`] (most-recent first).
    pub hist: Option<usize>,
}

/// The resolved run context (§context rules).
#[derive(Debug, Clone)]
pub struct ExecTarget {
    /// The commit oid to run against, or `None` for the current HEAD/worktree.
    pub oid: Option<String>,
    /// A short label for the tab and prompt (branch name / short oid).
    pub label: String,
}

/// A command the user has asked to run.
#[derive(Debug, Clone)]
pub struct PendingRun {
    pub command: String,
    pub target: ExecTarget,
}

/// Which way to reshape a commit in the Commits column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReshapeOp {
    Indent,
    Unindent,
}

/// A reshape the user has requested in the Commits column.
#[derive(Debug, Clone)]
pub struct ReshapeRequest {
    /// The selected commit to indent/unindent.
    pub oid: String,
    pub op: ReshapeOp,
}

/// A draggable interior boundary between panes (§8.2 mouse resize).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Divider {
    /// The vertical line between two adjacent expanded top columns (left, right).
    Column(ColumnKind, ColumnKind),
    /// The horizontal line between the top band and the viewport pane.
    Split,
}

/// An action button rendered in a finished command tab's body.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RunButton {
    Rerun,
    Close,
}

/// Clickable regions recorded during the last `draw` (§8.2 mouse input).
#[derive(Default)]
pub struct Hit {
    pub columns: Vec<(ColumnKind, Rect)>,
    pub stacks: Vec<(u16, usize)>,
    pub recents: Vec<(u16, usize)>,
    pub commits: Vec<(u16, usize)>,
    pub files: Vec<(u16, usize)>,
    pub viewport_tabs: Vec<(Rect, usize)>,
    pub viewport_closes: Vec<(Rect, usize)>,
    pub viewport_badges: Vec<(Rect, usize)>,
    pub viewport_run_buttons: Vec<(Rect, RunButton)>,
    pub dividers: Vec<(Divider, Rect)>,
    pub band: Rect,
    pub scene: Rect,
    pub expanded_total: u16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::ColumnKind;

    #[test]
    fn test_view_state_serialization() {
        let state = ViewState {
            focused: ColumnKind::Commits,
            selected_stair: 1,
            selected_commit: 2,
            selected_file: 3,
            zoom: true,
            checks_open: false,
            layout: LayoutPrefs::default(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let deserialized: ViewState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, deserialized);
    }
}
