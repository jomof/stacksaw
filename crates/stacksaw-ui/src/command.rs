//! Data-driven command registry (§8.2).
//!
//! One table — [`registry`] — is the single source of truth for keybindings,
//! the contextual hint bar, the `?` help overlay, and the `:` command palette.
//! Every surface is a *projection* of this table, so adding an [`Action`] (and
//! its registry row) updates all of them at once; invariant tests in this
//! module keep the projections honest.

use std::cmp::Reverse;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::layout::ColumnKind;

/// Every discrete thing the user can invoke. A closed set so the registry can
/// be checked for exhaustiveness (see tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MoveDown,
    MoveUp,
    StairDown,
    StairUp,
    /// Cycle focus forward through the unified sequence Stacks → Commits → Files
    /// → each Viewport tab → back to Stacks (bound to →).
    CycleFocusNext,
    /// Cycle focus backward through that same sequence (bound to ←).
    CycleFocusPrev,
    Activate,
    Focus(ColumnKind),
    ToggleChecks,
    ToggleZoom,
    OpenPalette,
    OpenHelp,
    /// Open the `>` command launcher.
    OpenRunPrompt,
    /// Move to the next / previous viewport tab.
    /// Close the active viewport tab.
    ViewportCloseTab,
    /// Cycle the syntect theme used for Diff syntax highlighting.
    CycleDiffTheme,
    /// Re-run the active command tab.
    RunRerun,
    /// Interrupt (SIGINT) the active command tab.
    RunCancel,
    /// Enter terminal capture mode for the active running command tab.
    ToggleCapture,
    /// Indent the selected commit into a new (deeper) staircase branch.
    IndentCommit,
    /// Unindent the selected commit into the prior staircase branch.
    UnindentCommit,
    /// Push the selected stack's branches to their remote (a Run-tab command).
    Push,
    /// Archive the selected stack (park its branches out of `refs/heads/`).
    ArchiveStack,
    /// Undo the last reshape/archive (restore the checkpointed refs).
    Undo,
    Quit,
}

/// A renderer-agnostic key. Shift is encoded in the character case (`J`);
/// `Ctrl` is its own variant so we can bind chords like `⌃z` distinctly from
/// the bare character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// A `Ctrl`+character chord (e.g. `Key::Ctrl('z')`).
    Ctrl(char),
    Up,
    Down,
    Left,
    Right,
    Tab,
    BackTab,
    Enter,
    Esc,
}

impl Key {
    /// True when a crossterm key event matches this binding. `Char` matches only
    /// without `Ctrl` (so `z` and `⌃z` stay distinct); `Ctrl` requires it.
    pub fn matches(self, ev: &KeyEvent) -> bool {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        match self {
            Key::Ctrl(c) => ctrl && ev.code == KeyCode::Char(c),
            Key::Char(c) => !ctrl && ev.code == KeyCode::Char(c),
            Key::Up => ev.code == KeyCode::Up,
            Key::Down => ev.code == KeyCode::Down,
            Key::Left => ev.code == KeyCode::Left,
            Key::Right => ev.code == KeyCode::Right,
            Key::Tab => ev.code == KeyCode::Tab,
            Key::BackTab => ev.code == KeyCode::BackTab,
            Key::Enter => ev.code == KeyCode::Enter,
            Key::Esc => ev.code == KeyCode::Esc,
        }
    }

    /// A short human label for hint/help/palette rendering.
    pub fn label(self) -> String {
        match self {
            Key::Char(' ') => "space".to_string(),
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("⌃{c}"),
            Key::Up => "↑".to_string(),
            Key::Down => "↓".to_string(),
            Key::Left => "←".to_string(),
            Key::Right => "→".to_string(),
            Key::Tab => "Tab".to_string(),
            Key::BackTab => "⇧Tab".to_string(),
            Key::Enter => "enter".to_string(),
            Key::Esc => "esc".to_string(),
        }
    }
}

/// Grouping for the help overlay and palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Navigate,
    Edit,
    View,
    Help,
    Session,
}

impl Category {
    pub fn title(self) -> &'static str {
        match self {
            Category::Navigate => "Navigate",
            Category::Edit => "Reshape",
            Category::View => "View",
            Category::Help => "Help",
            Category::Session => "Session",
        }
    }

    /// Order categories appear in the help overlay.
    pub const ORDER: [Category; 5] = [
        Category::Navigate,
        Category::Edit,
        Category::View,
        Category::Help,
        Category::Session,
    ];
}

/// The Stacks column carries two selection sub-contexts: some actions only make
/// sense on a branch/staircase row, others only on a recent-repository row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StacksRow {
    /// A branch/staircase row: drives the Commits column and is archivable.
    Staircase,
    /// A recent-repository (MRU) row: activate to switch repos.
    Recent,
}

/// The active Viewport contributor type. Contributor-specific actions attach to
/// one of these; the set grows as new contributors land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportKind {
    /// The singleton Diff view (syntax-highlighted diffs).
    Diff,
    /// A Run command terminal.
    Run,
}

/// The current focus — finer than a bare column where selection changes which
/// actions apply. Beyond the focused column it carries the Stacks row kind and
/// the active viewport contributor type; each is ignored outside its column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    pub column: ColumnKind,
    /// Which kind of Stacks row is selected; ignored unless `column == Stacks`.
    pub stacks_row: StacksRow,
    /// The active viewport contributor; ignored unless `column == Diff`.
    pub viewport: ViewportKind,
}

impl Focus {
    /// Focus a column with the default sub-contexts (Stacks staircase, Diff tab).
    pub fn column(column: ColumnKind) -> Self {
        Focus {
            column,
            stacks_row: StacksRow::Staircase,
            viewport: ViewportKind::Diff,
        }
    }

    /// Focus the Stacks column on a specific row kind.
    pub fn stacks(stacks_row: StacksRow) -> Self {
        Focus {
            stacks_row,
            ..Focus::column(ColumnKind::Stacks)
        }
    }

    /// Focus the Viewport column on a specific active contributor type.
    pub fn diff(viewport: ViewportKind) -> Self {
        Focus {
            viewport,
            ..Focus::column(ColumnKind::Viewport)
        }
    }
}

/// Where a command applies. `Always` is global; `Focused(k)` when column `k` is
/// focused; the `Stacks*`/`Viewport` variants further narrow to a Stacks row
/// kind or viewport contributor type. Present so selection-specific actions slot
/// in without changing any projection code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    Always,
    Focused(ColumnKind),
    /// Stacks column with a branch/staircase row selected.
    StacksStaircase,
    /// Stacks column with a recent-repository row selected.
    StacksRecent,
    /// Viewport column with a specific contributor active.
    Viewport(ViewportKind),
}

impl Context {
    pub fn applies(self, focus: Focus) -> bool {
        match self {
            Context::Always => true,
            Context::Focused(k) => k == focus.column,
            Context::StacksStaircase => {
                focus.column == ColumnKind::Stacks && focus.stacks_row == StacksRow::Staircase
            }
            Context::StacksRecent => {
                focus.column == ColumnKind::Stacks && focus.stacks_row == StacksRow::Recent
            }
            Context::Viewport(kind) => {
                focus.column == ColumnKind::Viewport && focus.viewport == kind
            }
        }
    }

    /// Binding specificity: column/row/contributor-specific contexts (0) win over
    /// the global `Always` (1) for the same key (see [`lookup`]).
    fn specificity(self) -> u8 {
        match self {
            Context::Always => 1,
            Context::Focused(_)
            | Context::StacksStaircase
            | Context::StacksRecent
            | Context::Viewport(_) => 0,
        }
    }
}

/// One registry row: an action, how to describe it, how to bind it, where it
/// applies, and how prominently it should appear in the hint bar.
#[derive(Debug, Clone, Copy)]
pub struct Command {
    pub action: Action,
    pub title: &'static str,
    pub category: Category,
    pub keys: &'static [Key],
    pub context: Context,
    /// `Some(rank)` places this command in the hint bar (higher = further left);
    /// `None` keeps it out of the always-on bar (still in help + palette).
    pub hint_rank: Option<u8>,
}

impl Command {
    /// The primary (first) key, used as the label in hint/help/palette.
    pub fn primary_key(&self) -> Option<Key> {
        self.keys.first().copied()
    }

    pub fn primary_key_label(&self) -> String {
        self.primary_key().map(Key::label).unwrap_or_default()
    }
}

/// The command table — the single source of truth (§8.2).
pub fn registry() -> &'static [Command] {
    use Action::*;
    use Category::*;
    &[
        Command {
            action: MoveDown,
            title: "Move down",
            category: Navigate,
            keys: &[Key::Down],
            context: Context::Always,
            hint_rank: Some(100),
        },
        Command {
            action: MoveUp,
            title: "Move up",
            category: Navigate,
            keys: &[Key::Up],
            context: Context::Always,
            hint_rank: Some(99),
        },
        // The arrows walk the panes and viewport tabs as one linear ring — the
        // single focus-movement idiom (they replaced the old h/l column/tab
        // split). Ranked where the column-cycle hints used to sit.
        Command {
            action: CycleFocusPrev,
            title: "Cycle focus back",
            category: Navigate,
            keys: &[Key::Left],
            context: Context::Always,
            hint_rank: Some(93),
        },
        Command {
            action: CycleFocusNext,
            title: "Cycle focus forward",
            category: Navigate,
            keys: &[Key::Right],
            context: Context::Always,
            hint_rank: Some(92),
        },
        Command {
            action: Activate,
            title: "Open recent repo",
            category: Navigate,
            keys: &[Key::Enter],
            context: Context::StacksRecent,
            hint_rank: Some(79),
        },
        Command {
            action: StairDown,
            title: "Next stack",
            category: Navigate,
            keys: &[],
            context: Context::Always,
            hint_rank: None,
        },
        Command {
            action: StairUp,
            title: "Previous stack",
            category: Navigate,
            keys: &[],
            context: Context::Always,
            hint_rank: None,
        },
        Command {
            action: Focus(ColumnKind::Stacks),
            title: "Focus Stacks",
            category: Navigate,
            keys: &[Key::Char('1')],
            context: Context::Always,
            hint_rank: None,
        },
        Command {
            action: Focus(ColumnKind::Commits),
            title: "Focus Commits",
            category: Navigate,
            keys: &[Key::Char('2')],
            context: Context::Always,
            hint_rank: None,
        },
        Command {
            action: Focus(ColumnKind::Files),
            title: "Focus Files",
            category: Navigate,
            keys: &[Key::Char('3')],
            context: Context::Always,
            hint_rank: None,
        },
        Command {
            action: Focus(ColumnKind::Viewport),
            title: "Focus Diff",
            category: Navigate,
            keys: &[Key::Char('4')],
            context: Context::Always,
            hint_rank: None,
        },
        Command {
            action: ToggleChecks,
            title: "Toggle Checks",
            category: View,
            keys: &[Key::Char('5')],
            context: Context::Always,
            hint_rank: None,
        },
        Command {
            action: ToggleZoom,
            title: "Zoom column",
            category: View,
            keys: &[Key::Char('z')],
            context: Context::Always,
            hint_rank: Some(62),
        },
        Command {
            action: OpenPalette,
            title: "Command palette",
            category: Help,
            keys: &[Key::Char(':')],
            context: Context::Always,
            hint_rank: Some(64),
        },
        Command {
            action: OpenHelp,
            title: "Help",
            category: Help,
            keys: &[Key::Char('?')],
            context: Context::Always,
            // Pinned to the far right of the hint bar (see `draw_hint_bar`); the
            // rank only orders it among the tail conveniences.
            hint_rank: Some(48),
        },
        Command {
            action: OpenRunPrompt,
            title: "Run command",
            category: View,
            keys: &[Key::Char('>')],
            context: Context::Always,
            hint_rank: Some(66),
        },
        Command {
            action: ToggleCapture,
            title: "Interact with terminal",
            category: View,
            keys: &[Key::Enter],
            context: Context::Viewport(ViewportKind::Run),
            hint_rank: Some(76),
        },
        Command {
            action: IndentCommit,
            title: "Indent",
            category: Edit,
            keys: &[Key::Tab],
            context: Context::Focused(ColumnKind::Commits),
            // Sits with its sibling Unindent in the context-action band; in
            // Commits it shadows the Tab column-cycle (tested).
            hint_rank: Some(82),
        },
        Command {
            action: UnindentCommit,
            title: "Unindent",
            category: Edit,
            keys: &[Key::BackTab],
            context: Context::Focused(ColumnKind::Commits),
            hint_rank: Some(81),
        },
        Command {
            action: Push,
            title: "Push stack",
            category: Session,
            keys: &[Key::Char('p')],
            context: Context::StacksStaircase,
            hint_rank: Some(83),
        },
        Command {
            action: ArchiveStack,
            title: "Archive stack",
            category: Edit,
            keys: &[Key::Char('a')],
            context: Context::StacksStaircase,
            hint_rank: Some(80),
        },
        Command {
            action: Undo,
            title: "Undo",
            category: Edit,
            keys: &[Key::Ctrl('z')],
            context: Context::Always,
            hint_rank: Some(70),
        },
        Command {
            action: ViewportCloseTab,
            title: "Close tab",
            category: View,
            keys: &[Key::Char('x')],
            context: Context::Focused(ColumnKind::Viewport),
            hint_rank: Some(77),
        },
        Command {
            action: CycleDiffTheme,
            title: "Cycle diff theme",
            category: View,
            keys: &[Key::Char('t')],
            context: Context::Viewport(ViewportKind::Diff),
            hint_rank: None,
        },
        Command {
            action: RunRerun,
            title: "Re-run command",
            category: View,
            keys: &[Key::Char('r')],
            context: Context::Viewport(ViewportKind::Run),
            hint_rank: None,
        },
        Command {
            action: RunCancel,
            title: "Cancel command",
            category: View,
            keys: &[Key::Char('c')],
            context: Context::Viewport(ViewportKind::Run),
            hint_rank: None,
        },
        Command {
            action: Quit,
            title: "Quit",
            category: Session,
            keys: &[Key::Esc],
            context: Context::Always,
            hint_rank: Some(40),
        },
    ]
}

/// Resolve a key event to an action, honoring the current focus. A column- or
/// row-specific binding overrides a global (`Always`) one for the same key —
/// that's how `Tab` indents inside Commits but still cycles columns elsewhere.
/// Within a single specificity no two commands share a key (tested).
pub fn lookup(ev: &KeyEvent, focus: Focus) -> Option<Action> {
    registry()
        .iter()
        .filter(|c| c.context.applies(focus) && c.keys.iter().any(|k| k.matches(ev)))
        .min_by_key(|c| c.context.specificity())
        .map(|c| c.action)
}

/// Commands to show in the always-on hint bar for the given focus, most
/// prominent first.
pub fn hint_commands(focus: Focus) -> Vec<&'static Command> {
    let mut cmds: Vec<&'static Command> = registry()
        .iter()
        .filter(|c| c.hint_rank.is_some() && c.context.applies(focus))
        .collect();
    // A specific binding overrides a global one on the same key (see `lookup`);
    // drop the shadowed global so the bar shows the live action.
    let overridden: Vec<Key> = cmds
        .iter()
        .filter(|c| c.context.specificity() == 0)
        .flat_map(|c| c.keys.iter().copied())
        .collect();
    cmds.retain(|c| {
        !(matches!(c.context, Context::Always) && c.keys.iter().any(|k| overridden.contains(k)))
    });
    cmds.sort_by_key(|a| Reverse(a.hint_rank));
    cmds
}

/// A group of related hints rendered as one compact entry — e.g. Move Down/Up
/// collapses to `j/k Down/Up`. `members` are listed in display order (their keys
/// and the label read left→right in the same order); `label` is the combined
/// text. A group only fires when *all* its members are present in the current
/// context, and it takes the members' highest `hint_rank` so it keeps their slot.
struct HintGroup {
    members: &'static [Action],
    label: &'static str,
}

const HINT_GROUPS: &[HintGroup] = &[
    HintGroup {
        members: &[Action::MoveUp, Action::MoveDown],
        label: "Up/Down",
    },
    HintGroup {
        members: &[Action::CycleFocusPrev, Action::CycleFocusNext],
        label: "Cycle focus",
    },
    HintGroup {
        members: &[Action::IndentCommit, Action::UnindentCommit],
        label: "Indent/Unindent",
    },
];

/// One rendered hint-bar entry: a key label (possibly combined, e.g. `j/k`), a
/// text label, its priority `rank`, and whether it is the pinned Help.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintItem {
    pub keys: String,
    pub label: String,
    pub rank: u8,
    pub pinned: bool,
}

impl HintItem {
    fn width(&self) -> usize {
        self.keys.chars().count() + 1 + self.label.chars().count()
    }
}

/// The contextual hints as rendered entries, most prominent first, after the
/// grouping layer collapses related pairs (see [`HINT_GROUPS`]).
pub fn hint_items(focus: Focus) -> Vec<HintItem> {
    let cmds = hint_commands(focus);
    let present: Vec<Action> = cmds.iter().map(|c| c.action).collect();
    let key_of = |a: Action| {
        cmds.iter()
            .find(|c| c.action == a)
            .map(|c| c.primary_key_label())
            .unwrap_or_default()
    };
    let rank_of = |a: Action| {
        cmds.iter()
            .find(|c| c.action == a)
            .and_then(|c| c.hint_rank)
            .unwrap_or(0)
    };

    let mut items: Vec<HintItem> = Vec::new();
    let mut consumed: Vec<Action> = Vec::new();
    for c in &cmds {
        if consumed.contains(&c.action) {
            continue;
        }
        // Collapse a related pair into one entry when every member is present.
        if let Some(g) = HINT_GROUPS.iter().find(|g| {
            g.members.contains(&c.action) && g.members.iter().all(|a| present.contains(a))
        }) {
            let keys = g
                .members
                .iter()
                .map(|a| key_of(*a))
                .collect::<Vec<_>>()
                .join("/");
            let rank = g.members.iter().map(|a| rank_of(*a)).max().unwrap_or(0);
            items.push(HintItem {
                keys,
                label: g.label.to_string(),
                rank,
                pinned: false,
            });
            consumed.extend_from_slice(g.members);
            continue;
        }
        items.push(HintItem {
            keys: c.primary_key_label(),
            label: c.title.to_string(),
            rank: c.hint_rank.unwrap_or(0),
            pinned: c.action == Action::OpenHelp,
        });
        consumed.push(c.action);
    }
    items.sort_by_key(|a| Reverse(a.rank));
    items
}

/// The overflow marker drawn when hints don't all fit.
pub const HINT_ELLIPSIS: &str = "…";

/// How the contextual hints fit into a fixed number of columns. `shown` are the
/// highest-priority hints that fit (left→right); `pinned` (Help) always renders
/// at the far right; `dropped` fell off the end and `truncated` says a `…` marker
/// belongs before `pinned`. Shared by the renderer ([`crate::app`]) and goldens
/// so there is one fitting rule.
pub struct HintFit {
    pub shown: Vec<HintItem>,
    pub dropped: Vec<HintItem>,
    pub pinned: Option<HintItem>,
    pub truncated: bool,
}

/// Fit the contextual hint bar into `budget` columns, where `sep_w` is the
/// rendered width of the inter-item separator (`" · "` = 3 in the default theme).
/// Highest-priority hints win; Help is pinned to the end; when items are dropped,
/// room is reserved for a trailing `…`.
pub fn fit_hints(focus: Focus, budget: usize, sep_w: usize) -> HintFit {
    let all = hint_items(focus);
    let pinned = all.iter().find(|i| i.pinned).cloned();
    let rest: Vec<HintItem> = all.into_iter().filter(|i| !i.pinned).collect();
    let reserved = pinned.as_ref().map_or(0, |p| sep_w + p.width());

    let mut shown: Vec<HintItem> = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;
    for item in &rest {
        let need = if shown.is_empty() {
            item.width()
        } else {
            sep_w + item.width()
        };
        if used + need + reserved <= budget {
            used += need;
            shown.push(item.clone());
        } else {
            truncated = true;
            break;
        }
    }
    if truncated {
        let ell = sep_w + HINT_ELLIPSIS.chars().count();
        while used + ell + reserved > budget {
            match shown.pop() {
                Some(item) => {
                    used -= if shown.is_empty() {
                        item.width()
                    } else {
                        sep_w + item.width()
                    }
                }
                None => break,
            }
        }
    }
    let dropped = rest[shown.len()..].to_vec();
    HintFit {
        shown,
        dropped,
        pinned,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `Action` variant must have exactly one registry row. The `match`
    /// forces a compile error if a variant is added without wiring it up.
    #[test]
    fn every_action_is_registered() {
        for cmd in registry() {
            // Exhaustive match: adding an Action variant breaks the build here
            // until it is handled (and thus, by convention, registered).
            match cmd.action {
                Action::MoveDown
                | Action::MoveUp
                | Action::StairDown
                | Action::StairUp
                | Action::CycleFocusNext
                | Action::CycleFocusPrev
                | Action::Activate
                | Action::Focus(_)
                | Action::ToggleChecks
                | Action::ToggleZoom
                | Action::OpenPalette
                | Action::OpenHelp
                | Action::OpenRunPrompt
                | Action::ViewportCloseTab
                | Action::CycleDiffTheme
                | Action::RunRerun
                | Action::RunCancel
                | Action::ToggleCapture
                | Action::IndentCommit
                | Action::UnindentCommit
                | Action::Push
                | Action::ArchiveStack
                | Action::Undo
                | Action::Quit => {}
            }
        }
        // Spot-check that the key actions are present.
        let actions: Vec<Action> = registry().iter().map(|c| c.action).collect();
        for expected in [
            Action::MoveDown,
            Action::MoveUp,
            Action::CycleFocusNext,
            Action::OpenPalette,
            Action::OpenHelp,
            Action::Quit,
        ] {
            assert!(actions.contains(&expected), "missing {expected:?}");
        }
    }

    /// A key may be claimed by at most one global (`Always`) command and at most
    /// one column-specific (`Focused`) command; the focused one overrides (see
    /// `lookup`). Two commands of the *same* specificity sharing a key is the
    /// real ambiguity, so that is what we forbid.
    #[test]
    fn no_key_collisions_within_a_specificity() {
        // Every reachable focus, including both Stacks sub-rows and both viewport
        // contributor types.
        let focuses: Vec<Focus> = ColumnKind::ALL
            .into_iter()
            .map(Focus::column)
            .chain([
                Focus::stacks(StacksRow::Recent),
                Focus::diff(ViewportKind::Run),
            ])
            .collect();
        for focus in focuses {
            let mut always: Vec<(Key, Action)> = Vec::new();
            let mut specific: Vec<(Key, Action)> = Vec::new();
            for cmd in registry().iter().filter(|c| c.context.applies(focus)) {
                let bucket = if cmd.context.specificity() == 0 {
                    &mut specific
                } else {
                    &mut always
                };
                for &key in cmd.keys {
                    if let Some((_, other)) = bucket.iter().find(|(k, _)| *k == key) {
                        panic!(
                            "key {:?} bound to both {:?} and {:?} at the same specificity in {:?}",
                            key, other, cmd.action, focus
                        );
                    }
                    bucket.push((key, cmd.action));
                }
            }
        }
    }

    /// Tab is a Commits-only reshape key: it indents there and is unbound in
    /// every other column, so it never collides with column navigation.
    #[test]
    fn tab_indents_only_in_commits() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            lookup(&tab, Focus::column(ColumnKind::Commits)),
            Some(Action::IndentCommit)
        );
        assert_eq!(lookup(&tab, Focus::column(ColumnKind::Stacks)), None);
        assert_eq!(lookup(&tab, Focus::column(ColumnKind::Files)), None);
    }

    /// Focus movement lives entirely on the arrows now: →/← cycle the unified
    /// pane+tab ring in every column, and the old h/l bindings are gone.
    #[test]
    fn arrows_cycle_focus_and_hl_is_unbound() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let arrow = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for kind in ColumnKind::ALL {
            let focus = Focus::column(kind);
            assert_eq!(
                lookup(&arrow(KeyCode::Right), focus),
                Some(Action::CycleFocusNext)
            );
            assert_eq!(
                lookup(&arrow(KeyCode::Left), focus),
                Some(Action::CycleFocusPrev)
            );
            assert_eq!(lookup(&key('h'), focus), None, "h is unbound in {kind:?}");
            assert_eq!(lookup(&key('l'), focus), None, "l is unbound in {kind:?}");
        }
    }

    /// The Stacks sub-contexts split cleanly: Archive only on a staircase row,
    /// Open-recent only on a recent-repo row.
    #[test]
    fn stacks_sub_contexts_gate_row_specific_actions() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        let staircase = Focus::stacks(StacksRow::Staircase);
        let recent = Focus::stacks(StacksRow::Recent);

        assert_eq!(lookup(&a, staircase), Some(Action::ArchiveStack));
        assert_eq!(lookup(&a, recent), None);
        assert_eq!(lookup(&enter, recent), Some(Action::Activate));
        assert_eq!(lookup(&enter, staircase), None);
    }

    /// Viewport sub-contexts split by active contributor: theme-cycling is a Diff
    /// action, interact/re-run/cancel are Run actions. (§8.2)
    #[test]
    fn viewport_sub_contexts_gate_contributor_actions() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        let diff = Focus::diff(ViewportKind::Diff);
        let run = Focus::diff(ViewportKind::Run);

        // Contributor-specific.
        assert_eq!(lookup(&key('t'), diff), Some(Action::CycleDiffTheme));
        assert_eq!(lookup(&key('t'), run), None);
        assert_eq!(lookup(&enter, run), Some(Action::ToggleCapture));
        assert_eq!(lookup(&enter, diff), None);
        assert_eq!(lookup(&key('r'), run), Some(Action::RunRerun));
        assert_eq!(lookup(&key('r'), diff), None);

        // Tab movement is folded into the global arrow ring (→/←), not h/l.
        assert_eq!(lookup(&key('h'), diff), None);
        assert_eq!(lookup(&key('l'), run), None);
    }

    /// Every command is reachable: it has at least one key (all current ones
    /// do) so it can be invoked directly and labeled in the palette.
    #[test]
    fn every_command_has_a_key() {
        // A few commands are intentionally palette-only (reachable via the
        // command palette / help, not a dedicated key). Stack navigation lost
        // its `J`/`K` keys but stays available here as a cross-column jump.
        let palette_only = [Action::StairUp, Action::StairDown];
        for cmd in registry() {
            if palette_only.contains(&cmd.action) {
                assert!(
                    cmd.keys.is_empty(),
                    "{:?} is expected to be palette-only",
                    cmd.action
                );
                continue;
            }
            assert!(!cmd.keys.is_empty(), "{:?} has no key binding", cmd.action);
        }
    }
}
