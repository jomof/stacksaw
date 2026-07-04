//! Responsive column layout logic (§8.1). Pure functions, unit-tested; the
//! ratatui backend consumes the plan.

/// The five columns, left→right (§8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Stacks,
    Commits,
    Files,
    Diff,
    Checks,
}

impl ColumnKind {
    pub const ALL: [ColumnKind; 5] = [
        ColumnKind::Stacks,
        ColumnKind::Commits,
        ColumnKind::Files,
        ColumnKind::Diff,
        ColumnKind::Checks,
    ];

    pub fn title(self) -> &'static str {
        match self {
            ColumnKind::Stacks => "Stacks",
            ColumnKind::Commits => "Commits",
            ColumnKind::Files => "Files",
            ColumnKind::Diff => "Diff",
            ColumnKind::Checks => "Checks",
        }
    }

    /// Auto-collapse priority: collapse in this order first when narrow (§8.1
    /// "Diff > Commits > Files > Stacks > Checks" — Diff is *kept* longest, so
    /// it collapses last; Checks collapses first).
    fn keep_rank(self) -> u8 {
        match self {
            ColumnKind::Diff => 5,
            ColumnKind::Commits => 4,
            ColumnKind::Files => 3,
            ColumnKind::Stacks => 2,
            ColumnKind::Checks => 1,
        }
    }
}

/// Width of a collapsed column spine (§8.1: a 3-cell spine).
pub const SPINE_WIDTH: u16 = 3;

/// Minimum supported terminal size (§8.1).
pub const MIN_COLS: u16 = 80;
pub const MIN_ROWS: u16 = 24;

/// Below this width, drop to single-column deck mode (§8.1).
pub const DECK_MODE_COLS: u16 = 100;

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutPlan {
    /// A single full-width column with a breadcrumb (§8.1 deck mode).
    Deck { focused: ColumnKind },
    /// Each column is either expanded (with a width) or collapsed to a spine.
    Columns(Vec<ColumnSlot>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnSlot {
    pub kind: ColumnKind,
    /// `None` means collapsed to a spine.
    pub width: Option<u16>,
}

/// Compute a layout for the given width, focus, and zoom.
///
/// * `zoom` — when `Some`, the focused column maximizes and others collapse to
///   spines (§8.1 zoom).
pub fn plan(width: u16, focused: ColumnKind, zoom: bool, checks_open: bool) -> LayoutPlan {
    if width < DECK_MODE_COLS {
        return LayoutPlan::Deck { focused };
    }

    // Determine which columns are candidates to be expanded.
    let mut visible: Vec<ColumnKind> = ColumnKind::ALL
        .into_iter()
        .filter(|c| *c != ColumnKind::Checks || checks_open)
        .collect();

    if zoom {
        // Only the focused column expands; all others are spines.
        let slots = ColumnKind::ALL
            .into_iter()
            .filter(|c| *c != ColumnKind::Checks || checks_open)
            .map(|kind| ColumnSlot {
                kind,
                width: if kind == focused {
                    Some(width.saturating_sub(spine_total(&visible, focused)))
                } else {
                    None
                },
            })
            .collect();
        return LayoutPlan::Columns(slots);
    }

    // Greedy: expand columns by keep_rank until we run out of width; the rest
    // collapse to spines.
    visible.sort_by(|a, b| b.keep_rank().cmp(&a.keep_rank()));

    let mut expanded: Vec<ColumnKind> = Vec::new();
    let min_expanded = 12u16; // a column needs at least this to be useful
    let mut remaining = width;
    for kind in &visible {
        let spines_left = (visible.len() - expanded.len() - 1) as u16 * SPINE_WIDTH;
        if remaining.saturating_sub(spines_left) >= min_expanded {
            expanded.push(*kind);
            remaining = remaining.saturating_sub(min_expanded);
        }
    }

    // Distribute leftover width among expanded columns (Diff gets the surplus).
    let expanded_count = expanded.len().max(1) as u16;
    let spine_count = (visible.len() - expanded.len()) as u16;
    let usable = width.saturating_sub(spine_count * SPINE_WIDTH);
    let base = usable / expanded_count;
    let surplus = usable % expanded_count;

    let slots = ColumnKind::ALL
        .into_iter()
        .filter(|c| *c != ColumnKind::Checks || checks_open)
        .map(|kind| {
            if expanded.contains(&kind) {
                let extra = if kind == ColumnKind::Diff { surplus } else { 0 };
                ColumnSlot {
                    kind,
                    width: Some(base + extra),
                }
            } else {
                ColumnSlot { kind, width: None }
            }
        })
        .collect();
    LayoutPlan::Columns(slots)
}

fn spine_total(visible: &[ColumnKind], focused: ColumnKind) -> u16 {
    (visible.iter().filter(|c| **c != focused).count() as u16) * SPINE_WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_terminals_use_deck_mode() {
        assert!(matches!(
            plan(90, ColumnKind::Commits, false, false),
            LayoutPlan::Deck { focused: ColumnKind::Commits }
        ));
    }

    #[test]
    fn wide_terminal_expands_multiple_columns() {
        let LayoutPlan::Columns(slots) = plan(200, ColumnKind::Diff, false, true) else {
            panic!("expected columns");
        };
        let expanded = slots.iter().filter(|s| s.width.is_some()).count();
        assert!(expanded >= 3, "wide layout should expand several columns");
        // Total width (expanded + spines) fits within the terminal.
        let used: u16 = slots
            .iter()
            .map(|s| s.width.unwrap_or(SPINE_WIDTH))
            .sum();
        assert!(used <= 200);
    }

    #[test]
    fn zoom_expands_only_focused() {
        let LayoutPlan::Columns(slots) = plan(200, ColumnKind::Commits, true, false) else {
            panic!("expected columns");
        };
        for s in &slots {
            if s.kind == ColumnKind::Commits {
                assert!(s.width.is_some());
            } else {
                assert!(s.width.is_none(), "{:?} should be a spine under zoom", s.kind);
            }
        }
    }

    #[test]
    fn diff_is_kept_longest() {
        // At a modest width, Diff should be among the expanded columns.
        let LayoutPlan::Columns(slots) = plan(110, ColumnKind::Diff, false, false) else {
            panic!("expected columns");
        };
        let diff = slots.iter().find(|s| s.kind == ColumnKind::Diff).unwrap();
        assert!(diff.width.is_some(), "Diff collapses last");
    }
}
