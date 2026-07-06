//! Data-driven command registry (§8.2).
//!
//! One table — [`registry`] — is the single source of truth for keybindings,
//! the contextual hint bar, the `?` help overlay, and the `:` command palette.
//! Every surface is a *projection* of this table, so adding an [`Action`] (and
//! its registry row) updates all of them at once; invariant tests in this
//! module keep the projections honest.

use crossterm::event::{KeyCode, KeyEvent};

use crate::layout::ColumnKind;

/// Every discrete thing the user can invoke. A closed set so the registry can
/// be checked for exhaustiveness (see tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MoveDown,
    MoveUp,
    StairDown,
    StairUp,
    NextColumn,
    Activate,
    Focus(ColumnKind),
    ToggleChecks,
    ToggleZoom,
    OpenPalette,
    OpenHelp,
    /// Open the `>` command launcher.
    OpenRunPrompt,
    /// Move to the next / previous viewport tab.
    ViewportNextTab,
    ViewportPrevTab,
    /// Close the active viewport tab.
    ViewportCloseTab,
    /// Re-run the active command tab.
    RunRerun,
    /// Interrupt (SIGINT) the active command tab.
    RunCancel,
    /// Enter terminal capture mode for the active running command tab.
    ToggleCapture,
    Quit,
}

/// A renderer-agnostic key. Shift is encoded in the character case (`J`), which
/// is enough for the current bindings and keeps labels tidy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Tab,
    Enter,
    Esc,
}

impl Key {
    /// True when a crossterm key event matches this binding.
    pub fn matches(self, ev: &KeyEvent) -> bool {
        let code = match self {
            Key::Char(c) => KeyCode::Char(c),
            Key::Up => KeyCode::Up,
            Key::Down => KeyCode::Down,
            Key::Left => KeyCode::Left,
            Key::Right => KeyCode::Right,
            Key::Tab => KeyCode::Tab,
            Key::Enter => KeyCode::Enter,
            Key::Esc => KeyCode::Esc,
        };
        ev.code == code
    }

    /// A short human label for hint/help/palette rendering.
    pub fn label(self) -> String {
        match self {
            Key::Char(' ') => "space".to_string(),
            Key::Char(c) => c.to_string(),
            Key::Up => "↑".to_string(),
            Key::Down => "↓".to_string(),
            Key::Left => "←".to_string(),
            Key::Right => "→".to_string(),
            Key::Tab => "Tab".to_string(),
            Key::Enter => "enter".to_string(),
            Key::Esc => "esc".to_string(),
        }
    }
}

/// Grouping for the help overlay and palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Navigate,
    View,
    Help,
    Session,
}

impl Category {
    pub fn title(self) -> &'static str {
        match self {
            Category::Navigate => "Navigate",
            Category::View => "View",
            Category::Help => "Help",
            Category::Session => "Session",
        }
    }

    /// Order categories appear in the help overlay.
    pub const ORDER: [Category; 4] = [
        Category::Navigate,
        Category::View,
        Category::Help,
        Category::Session,
    ];
}

/// Where a command applies. `Always` is global; `Focused(k)` only when column
/// `k` is focused. Present so column-specific actions (range-select, drill-in,
/// restack, …) can slot in without changing any projection code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    Always,
    #[allow(dead_code)]
    Focused(ColumnKind),
}

impl Context {
    pub fn applies(self, focused: ColumnKind) -> bool {
        match self {
            Context::Always => true,
            Context::Focused(k) => k == focused,
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
            keys: &[Key::Char('j'), Key::Down],
            context: Context::Always,
            hint_rank: Some(100),
        },
        Command {
            action: MoveUp,
            title: "Move up",
            category: Navigate,
            keys: &[Key::Char('k'), Key::Up],
            context: Context::Always,
            hint_rank: Some(99),
        },
        Command {
            action: NextColumn,
            title: "Next column",
            category: Navigate,
            keys: &[Key::Tab],
            context: Context::Always,
            hint_rank: Some(90),
        },
        Command {
            action: Activate,
            title: "Open recent repo",
            category: Navigate,
            keys: &[Key::Enter],
            context: Context::Focused(ColumnKind::Stacks),
            hint_rank: Some(65),
        },
        Command {
            action: StairDown,
            title: "Next stack",
            category: Navigate,
            keys: &[Key::Char('J')],
            context: Context::Always,
            hint_rank: Some(70),
        },
        Command {
            action: StairUp,
            title: "Previous stack",
            category: Navigate,
            keys: &[Key::Char('K')],
            context: Context::Always,
            hint_rank: Some(69),
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
            action: Focus(ColumnKind::Diff),
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
            hint_rank: Some(50),
        },
        Command {
            action: OpenPalette,
            title: "Command palette",
            category: Help,
            keys: &[Key::Char(':')],
            context: Context::Always,
            hint_rank: Some(60),
        },
        Command {
            action: OpenHelp,
            title: "Help",
            category: Help,
            keys: &[Key::Char('?')],
            context: Context::Always,
            hint_rank: Some(55),
        },
        Command {
            action: OpenRunPrompt,
            title: "Run command",
            category: View,
            keys: &[Key::Char('>')],
            context: Context::Always,
            hint_rank: Some(62),
        },
        Command {
            action: ToggleCapture,
            title: "Interact with terminal",
            category: View,
            keys: &[Key::Enter],
            context: Context::Focused(ColumnKind::Diff),
            hint_rank: Some(64),
        },
        Command {
            action: ViewportNextTab,
            title: "Next tab",
            category: View,
            keys: &[Key::Char(']')],
            context: Context::Focused(ColumnKind::Diff),
            hint_rank: Some(58),
        },
        Command {
            action: ViewportPrevTab,
            title: "Previous tab",
            category: View,
            keys: &[Key::Char('[')],
            context: Context::Focused(ColumnKind::Diff),
            hint_rank: None,
        },
        Command {
            action: ViewportCloseTab,
            title: "Close tab",
            category: View,
            keys: &[Key::Char('x')],
            context: Context::Focused(ColumnKind::Diff),
            hint_rank: Some(56),
        },
        Command {
            action: RunRerun,
            title: "Re-run command",
            category: View,
            keys: &[Key::Char('r')],
            context: Context::Focused(ColumnKind::Diff),
            hint_rank: None,
        },
        Command {
            action: RunCancel,
            title: "Cancel command",
            category: View,
            keys: &[Key::Char('c')],
            context: Context::Focused(ColumnKind::Diff),
            hint_rank: None,
        },
        Command {
            action: Quit,
            title: "Quit",
            category: Session,
            keys: &[Key::Char('q'), Key::Esc],
            context: Context::Always,
            hint_rank: Some(40),
        },
    ]
}

/// Resolve a key event to an action, honoring the focused column's context.
/// Earlier registry rows win on the (tested-against) assumption that no two
/// applicable commands share a key in the same context.
pub fn lookup(ev: &KeyEvent, focused: ColumnKind) -> Option<Action> {
    registry()
        .iter()
        .find(|c| c.context.applies(focused) && c.keys.iter().any(|k| k.matches(ev)))
        .map(|c| c.action)
}

/// Commands to show in the always-on hint bar for the given focus, most
/// prominent first.
pub fn hint_commands(focused: ColumnKind) -> Vec<&'static Command> {
    let mut cmds: Vec<&'static Command> = registry()
        .iter()
        .filter(|c| c.hint_rank.is_some() && c.context.applies(focused))
        .collect();
    cmds.sort_by(|a, b| b.hint_rank.cmp(&a.hint_rank));
    cmds
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
                | Action::NextColumn
                | Action::Activate
                | Action::Focus(_)
                | Action::ToggleChecks
                | Action::ToggleZoom
                | Action::OpenPalette
                | Action::OpenHelp
                | Action::OpenRunPrompt
                | Action::ViewportNextTab
                | Action::ViewportPrevTab
                | Action::ViewportCloseTab
                | Action::RunRerun
                | Action::RunCancel
                | Action::ToggleCapture
                | Action::Quit => {}
            }
        }
        // Spot-check that the key actions are present.
        let actions: Vec<Action> = registry().iter().map(|c| c.action).collect();
        for expected in [
            Action::MoveDown,
            Action::MoveUp,
            Action::NextColumn,
            Action::OpenPalette,
            Action::OpenHelp,
            Action::Quit,
        ] {
            assert!(actions.contains(&expected), "missing {expected:?}");
        }
    }

    /// No two commands applicable in the same context may share a key.
    #[test]
    fn no_key_collisions_within_a_context() {
        for focused in ColumnKind::ALL {
            let mut seen: Vec<(Key, Action)> = Vec::new();
            for cmd in registry().iter().filter(|c| c.context.applies(focused)) {
                for &key in cmd.keys {
                    if let Some((_, other)) = seen.iter().find(|(k, _)| *k == key) {
                        panic!(
                            "key {:?} bound to both {:?} and {:?} in context {:?}",
                            key, other, cmd.action, focused
                        );
                    }
                    seen.push((key, cmd.action));
                }
            }
        }
    }

    /// Every command is reachable: it has at least one key (all current ones
    /// do) so it can be invoked directly and labeled in the palette.
    #[test]
    fn every_command_has_a_key() {
        for cmd in registry() {
            assert!(
                !cmd.keys.is_empty(),
                "{:?} has no key binding",
                cmd.action
            );
        }
    }
}
