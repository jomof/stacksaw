use crate::layout::ColumnKind;

/// Navigation state for the TUI columns (§8).
#[derive(Debug, Clone, PartialEq, Eq)]

pub struct Navigator {
    pub focused: ColumnKind,
    pub selected_stair: usize,
    pub selected_commit: usize,
    pub selected_file: usize,
    pub selected_recent: Option<usize>,
}

impl Navigator {
    pub fn new() -> Self {
        Self {
            focused: ColumnKind::Commits,
            selected_stair: 0,
            selected_commit: 0,
            selected_file: 0,
            selected_recent: None,
        }
    }

    pub fn move_selection(&mut self, down: bool, counts: NavCounts) {
        match self.focused {
            ColumnKind::Stacks => self.move_stacks(down, counts),
            ColumnKind::Files => {
                self.selected_file = step(self.selected_file, down, counts.files.saturating_sub(1));
            }
            ColumnKind::Viewport => {
                // Viewport scrolling is handled by Viewport itself.
            }
            _ => {
                self.selected_commit =
                    step(self.selected_commit, down, counts.commits.saturating_sub(1));
                self.selected_file = 0;
            }
        }
    }

    pub fn move_stair(&mut self, down: bool, counts: NavCounts) {
        self.selected_recent = None;
        let last = counts.stairs.saturating_sub(1);
        self.selected_stair = step(self.selected_stair, down, last);
        self.selected_commit = counts.default_commit;
        self.selected_file = 0;
    }

    pub fn move_stacks(&mut self, down: bool, counts: NavCounts) {
        let n_stairs = counts.stairs;
        let n_others = counts.recents;
        if n_others == 0 {
            self.move_stair(down, counts);
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
                self.selected_commit = counts.default_commit;
                self.selected_file = 0;
            }
        } else {
            self.selected_recent = Some(next - n_stairs);
        }
    }
}

#[derive(Debug, Clone, Copy)]

pub struct NavCounts {
    pub stairs: usize,
    pub commits: usize,
    pub files: usize,
    pub recents: usize,
    pub default_commit: usize,
}

pub(crate) fn step(cur: usize, down: bool, last: usize) -> usize {
    if down {
        (cur + 1).min(last)
    } else {
        cur.saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_selection_commits() {
        let mut nav = Navigator::new();
        nav.focused = ColumnKind::Commits;
        let counts = NavCounts {
            stairs: 1,
            commits: 3,
            files: 0,
            recents: 0,
            default_commit: 2,
        };

        // Move down
        nav.move_selection(true, counts);
        assert_eq!(nav.selected_commit, 1);

        // Move down again
        nav.move_selection(true, counts);
        assert_eq!(nav.selected_commit, 2);

        // Clamp at bottom
        nav.move_selection(true, counts);
        assert_eq!(nav.selected_commit, 2);

        // Move up
        nav.move_selection(false, counts);
        assert_eq!(nav.selected_commit, 1);
    }

    #[test]
    fn test_move_stacks_with_recents() {
        let mut nav = Navigator::new();
        nav.focused = ColumnKind::Stacks;
        let counts = NavCounts {
            stairs: 2,
            commits: 1,
            files: 0,
            recents: 2,
            default_commit: 0,
        };

        // Start at stair 0
        assert_eq!(nav.selected_stair, 0);
        assert_eq!(nav.selected_recent, None);

        // Move to stair 1
        nav.move_selection(true, counts);
        assert_eq!(nav.selected_stair, 1);
        assert_eq!(nav.selected_recent, None);

        // Move to recent 0
        nav.move_selection(true, counts);
        assert_eq!(nav.selected_stair, 1); // stair stays at last
        assert_eq!(nav.selected_recent, Some(0));

        // Move to recent 1
        nav.move_selection(true, counts);
        assert_eq!(nav.selected_recent, Some(1));

        // Clamp at bottom
        nav.move_selection(true, counts);
        assert_eq!(nav.selected_recent, Some(1));

        // Move back to recent 0
        nav.move_selection(false, counts);
        assert_eq!(nav.selected_recent, Some(0));

        // Move back to stair 1
        nav.move_selection(false, counts);
        assert_eq!(nav.selected_stair, 1);
        assert_eq!(nav.selected_recent, None);
    }
}
