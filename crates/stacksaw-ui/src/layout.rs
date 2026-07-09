//! Responsive column layout logic (§8.1). Pure functions, unit-tested; the
//! ratatui backend consumes the plan.

use std::cmp::Reverse;

use serde::{Deserialize, Serialize};

/// The five columns, left→right (§8.1). The bottom pane is the tabbed
/// **Viewport** (a host for the Diff view and Run terminals); it kept the legacy
/// `Diff` serialized name for compatibility (see the `serde(alias)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnKind {
    Stacks,
    Commits,
    Files,
    #[serde(alias = "Diff")]
    Viewport,
    Checks,
}

impl ColumnKind {
    pub const ALL: [ColumnKind; 5] = [
        ColumnKind::Stacks,
        ColumnKind::Commits,
        ColumnKind::Files,
        ColumnKind::Viewport,
        ColumnKind::Checks,
    ];

    pub fn title(self) -> &'static str {
        match self {
            ColumnKind::Stacks => "Stacks",
            ColumnKind::Commits => "Commits",
            ColumnKind::Files => "Files",
            ColumnKind::Viewport => "Viewport",
            ColumnKind::Checks => "Checks",
        }
    }

    /// Auto-collapse priority: collapse in this order first when narrow (§8.1
    /// "Diff > Commits > Files > Stacks > Checks" — Diff is *kept* longest, so
    /// it collapses last; Checks collapses first).
    fn keep_rank(self) -> u8 {
        match self {
            ColumnKind::Viewport => 5,
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

/// Minimum useful width for an expanded column.
pub const MIN_EXPANDED: u16 = 12;

/// User layout preferences captured by dragging the interior dividers (§8.2).
/// Stored as *fractions* rather than absolute cells so a resized layout keeps
/// its proportions when the terminal size changes. Persisted per-user by the
/// host; an empty value (the default) means "use the automatic layout".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutPrefs {
    /// Top-band height as a fraction of the scene. `None` = the automatic split.
    #[serde(default)]
    pub split_fraction: Option<f32>,
    /// Per-column width, as a fraction of the top band's *expanded* budget (the
    /// band width minus collapsed spines). Only columns the user has dragged
    /// appear here; the rest keep their automatic share.
    #[serde(default)]
    pub columns: Vec<(ColumnKind, f32)>,
}

impl LayoutPrefs {
    /// The stored fraction for `kind`, if the user has dragged it.
    pub fn column(&self, kind: ColumnKind) -> Option<f32> {
        self.columns
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, f)| *f)
    }

    /// Record `kind`'s width as `fraction` of the expanded budget.
    pub fn set_column(&mut self, kind: ColumnKind, fraction: f32) {
        match self.columns.iter_mut().find(|(k, _)| *k == kind) {
            Some(entry) => entry.1 = fraction,
            None => self.columns.push((kind, fraction)),
        }
    }
}

/// Upper bound on the content-sized Stacks column so it never hogs the row.
pub const STACKS_MAX_WIDTH: u16 = 44;

/// Compute a layout for the given width, focus, and zoom.
///
/// * `zoom` — when `true`, the focused column maximizes and others collapse to
///   spines (§8.1 zoom).
/// * `stacks_width` — optional content-based width for the Stacks column. When
///   provided and Stacks is expanded (and not zoomed), Stacks is sized to fit
///   its content instead of taking an equal share; the remaining width is
///   distributed among the other expanded columns.
pub fn plan(
    width: u16,
    focused: ColumnKind,
    zoom: bool,
    checks_open: bool,
    stacks_width: Option<u16>,
    manual: &LayoutPrefs,
) -> LayoutPlan {
    if width < DECK_MODE_COLS {
        return LayoutPlan::Deck { focused };
    }
    let columns: Vec<ColumnKind> = ColumnKind::ALL
        .into_iter()
        .filter(|c| *c != ColumnKind::Checks || checks_open)
        .collect();
    LayoutPlan::Columns(plan_over(
        width,
        focused,
        zoom,
        &columns,
        stacks_width,
        manual,
    ))
}

/// Lay out an ordered set of columns across `width`, sized by the same rules as
/// [`plan`] but over an arbitrary column list (so the viewport-at-bottom scene can
/// lay out just the top row). Returns slots in the given `columns` order.
pub fn plan_over(
    width: u16,
    focused: ColumnKind,
    zoom: bool,
    columns: &[ColumnKind],
    stacks_width: Option<u16>,
    manual: &LayoutPrefs,
) -> Vec<ColumnSlot> {
    if zoom {
        // Only the focused column expands; all others are spines.
        return columns
            .iter()
            .map(|&kind| ColumnSlot {
                kind,
                width: (kind == focused)
                    .then(|| width.saturating_sub(spine_total(columns, focused))),
            })
            .collect();
    }

    // Greedy: expand columns by keep_rank until we run out of width; the rest
    // collapse to spines.
    let mut order: Vec<ColumnKind> = columns.to_vec();
    order.sort_by_key(|k| Reverse(k.keep_rank()));

    let mut expanded: Vec<ColumnKind> = Vec::new();
    let mut remaining = width;
    for kind in &order {
        let spines_left = (order.len() - expanded.len() - 1) as u16 * SPINE_WIDTH;
        if remaining.saturating_sub(spines_left) >= MIN_EXPANDED {
            expanded.push(*kind);
            remaining = remaining.saturating_sub(MIN_EXPANDED);
        }
    }

    let spine_count = (order.len() - expanded.len()) as u16;
    let usable = width.saturating_sub(spine_count * SPINE_WIDTH);

    // Reserve a content-sized width for Stacks when hinted, it is expanded, and
    // there is at least one other expanded column to share the remainder.
    let stacks_reserved = match stacks_width {
        Some(w) if expanded.contains(&ColumnKind::Stacks) && expanded.len() > 1 => {
            let others = (expanded.len() - 1) as u16;
            let max_for_stacks = usable
                .saturating_sub(others * MIN_EXPANDED)
                .min(STACKS_MAX_WIDTH);
            Some(w.clamp(MIN_EXPANDED, max_for_stacks.max(MIN_EXPANDED)))
        }
        _ => None,
    };

    // Distribute the remaining width among the (other) expanded columns; the
    // highest-kept expanded column (Diff, or Commits when Diff is absent) takes
    // the rounding surplus.
    let (share_count, share_usable) = match stacks_reserved {
        Some(sw) => (
            (expanded.len() - 1).max(1) as u16,
            usable.saturating_sub(sw),
        ),
        None => (expanded.len().max(1) as u16, usable),
    };
    let base = share_usable / share_count;
    let surplus = share_usable % share_count;
    let flex = expanded
        .iter()
        .filter(|k| stacks_reserved.is_none() || **k != ColumnKind::Stacks)
        .max_by_key(|k| k.keep_rank())
        .copied();

    let mut slots: Vec<ColumnSlot> = columns
        .iter()
        .map(|&kind| {
            if kind == ColumnKind::Stacks && stacks_reserved.is_some() {
                ColumnSlot {
                    kind,
                    width: stacks_reserved,
                }
            } else if expanded.contains(&kind) {
                let extra = if Some(kind) == flex { surplus } else { 0 };
                ColumnSlot {
                    kind,
                    width: Some(base + extra),
                }
            } else {
                ColumnSlot { kind, width: None }
            }
        })
        .collect();
    apply_manual(&mut slots, manual);
    slots
}

/// Redistribute the expanded columns' budget according to the user's dragged
/// fractions. This only *reapportions* the cells the automatic layout already
/// handed to expanded columns (it never changes which columns collapse), so the
/// band still fits exactly and collapsed spines are untouched. Dragged columns
/// take `fraction * expanded_total`; the rest keep their automatic share, and
/// everything is normalized so the total is preserved and each column keeps at
/// least [`MIN_EXPANDED`]. A no-op when nothing here has been dragged.
fn apply_manual(slots: &mut [ColumnSlot], manual: &LayoutPrefs) {
    let expanded_total: u16 = slots.iter().filter_map(|s| s.width).sum();
    let idxs: Vec<usize> = slots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.width.is_some())
        .map(|(i, _)| i)
        .collect();
    if idxs.len() < 2 || expanded_total == 0 {
        return;
    }
    if !idxs.iter().any(|&i| manual.column(slots[i].kind).is_some()) {
        return;
    }

    // Dragged columns claim their stored fraction of the expanded budget; the
    // remaining cells are split among the untouched columns in proportion to
    // their automatic share (so dragging one divider leaves the rest balanced).
    let et = expanded_total as f32;
    let manual_cells: f32 = idxs
        .iter()
        .filter_map(|&i| manual.column(slots[i].kind).map(|f| (f * et).round()))
        .sum();
    let auto_rest: f32 = idxs
        .iter()
        .filter(|&&i| manual.column(slots[i].kind).is_none())
        .map(|&i| slots[i].width.unwrap_or(0) as f32)
        .sum();
    let remaining = (et - manual_cells).max(0.0);

    let mut widths: Vec<u16> = idxs
        .iter()
        .map(|&i| {
            let cells = match manual.column(slots[i].kind) {
                Some(f) => f * et,
                None if auto_rest > 0.0 => {
                    slots[i].width.unwrap_or(0) as f32 / auto_rest * remaining
                }
                None => remaining / (idxs.len() as f32),
            };
            (cells.round() as u16).max(MIN_EXPANDED)
        })
        .collect();
    let total: i32 = widths.iter().map(|&w| w as i32).sum();
    let drift = expanded_total as i32 - total;
    if let Some((flex_pos, _)) = idxs
        .iter()
        .enumerate()
        .max_by_key(|(_, &i)| slots[i].kind.keep_rank())
    {
        let adjusted = widths[flex_pos] as i32 + drift;
        widths[flex_pos] = adjusted.max(MIN_EXPANDED as i32) as u16;
    }

    for (pos, &i) in idxs.iter().enumerate() {
        slots[i].width = Some(widths[pos]);
    }
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
            plan(
                90,
                ColumnKind::Commits,
                false,
                false,
                None,
                &LayoutPrefs::default()
            ),
            LayoutPlan::Deck {
                focused: ColumnKind::Commits
            }
        ));
    }

    #[test]
    fn wide_terminal_expands_multiple_columns() {
        let LayoutPlan::Columns(slots) = plan(
            200,
            ColumnKind::Viewport,
            false,
            true,
            None,
            &LayoutPrefs::default(),
        ) else {
            panic!("expected columns");
        };
        let expanded = slots.iter().filter(|s| s.width.is_some()).count();
        assert!(expanded >= 3, "wide layout should expand several columns");
        // Total width (expanded + spines) fits within the terminal.
        let used: u16 = slots.iter().map(|s| s.width.unwrap_or(SPINE_WIDTH)).sum();
        assert!(used <= 200);
    }

    #[test]
    fn stacks_width_hint_sizes_stacks_and_fits() {
        let LayoutPlan::Columns(slots) = plan(
            200,
            ColumnKind::Viewport,
            false,
            true,
            Some(18),
            &LayoutPrefs::default(),
        ) else {
            panic!("expected columns");
        };
        let stacks = slots.iter().find(|s| s.kind == ColumnKind::Stacks).unwrap();
        assert_eq!(stacks.width, Some(18), "Stacks sized to the content hint");
        let used: u16 = slots.iter().map(|s| s.width.unwrap_or(SPINE_WIDTH)).sum();
        assert!(used <= 200, "columns still fit the terminal");
    }

    #[test]
    fn stacks_width_hint_is_clamped_to_max() {
        let LayoutPlan::Columns(slots) = plan(
            200,
            ColumnKind::Viewport,
            false,
            true,
            Some(500),
            &LayoutPrefs::default(),
        ) else {
            panic!("expected columns");
        };
        let stacks = slots.iter().find(|s| s.kind == ColumnKind::Stacks).unwrap();
        assert_eq!(stacks.width, Some(STACKS_MAX_WIDTH), "clamped to the cap");
    }

    #[test]
    fn zoom_expands_only_focused() {
        let LayoutPlan::Columns(slots) = plan(
            200,
            ColumnKind::Commits,
            true,
            false,
            None,
            &LayoutPrefs::default(),
        ) else {
            panic!("expected columns");
        };
        for s in &slots {
            if s.kind == ColumnKind::Commits {
                assert!(s.width.is_some());
            } else {
                assert!(
                    s.width.is_none(),
                    "{:?} should be a spine under zoom",
                    s.kind
                );
            }
        }
    }

    #[test]
    fn diff_is_kept_longest() {
        // At a modest width, Diff should be among the expanded columns.
        let LayoutPlan::Columns(slots) = plan(
            110,
            ColumnKind::Viewport,
            false,
            false,
            None,
            &LayoutPrefs::default(),
        ) else {
            panic!("expected columns");
        };
        let diff = slots
            .iter()
            .find(|s| s.kind == ColumnKind::Viewport)
            .unwrap();
        assert!(diff.width.is_some(), "Diff collapses last");
    }

    #[test]
    fn manual_fraction_widens_a_column_and_still_fits() {
        // Drag Commits out to ~40% of the expanded budget.
        let mut manual = LayoutPrefs::default();
        manual.set_column(ColumnKind::Commits, 0.40);
        let LayoutPlan::Columns(slots) =
            plan(200, ColumnKind::Viewport, false, true, None, &manual)
        else {
            panic!("expected columns");
        };
        let expanded_total: u16 = slots.iter().filter_map(|s| s.width).sum();
        let commits = slots
            .iter()
            .find(|s| s.kind == ColumnKind::Commits)
            .unwrap()
            .width
            .unwrap();
        // Commits gets roughly the requested share (within rounding).
        let want = (expanded_total as f32 * 0.40).round() as i32;
        assert!(
            (commits as i32 - want).abs() <= 2,
            "Commits honored the dragged fraction (got {commits}, want ~{want})"
        );
        let used: u16 = slots.iter().map(|s| s.width.unwrap_or(SPINE_WIDTH)).sum();
        assert!(used <= 200, "columns still fit the terminal");
    }

    #[test]
    fn manual_fraction_is_clamped_to_min_expanded() {
        // Ask for an absurdly small slice; it clamps up to MIN_EXPANDED.
        let mut manual = LayoutPrefs::default();
        manual.set_column(ColumnKind::Files, 0.001);
        let LayoutPlan::Columns(slots) =
            plan(200, ColumnKind::Viewport, false, true, None, &manual)
        else {
            panic!("expected columns");
        };
        let files = slots
            .iter()
            .find(|s| s.kind == ColumnKind::Files)
            .unwrap()
            .width
            .unwrap();
        assert!(
            files >= MIN_EXPANDED,
            "clamped to the minimum expanded width"
        );
        for s in &slots {
            if let Some(w) = s.width {
                assert!(w >= MIN_EXPANDED, "{:?} stays >= MIN_EXPANDED", s.kind);
            }
        }
    }

    #[test]
    fn manual_never_disturbs_collapsed_spines() {
        // At a modest width some columns collapse; a manual fraction for an
        // expanded column must not resurrect a spine.
        let mut manual = LayoutPrefs::default();
        manual.set_column(ColumnKind::Viewport, 0.6);
        let LayoutPlan::Columns(auto) = plan(
            110,
            ColumnKind::Viewport,
            false,
            false,
            None,
            &LayoutPrefs::default(),
        ) else {
            panic!("expected columns");
        };
        let LayoutPlan::Columns(dragged) =
            plan(110, ColumnKind::Viewport, false, false, None, &manual)
        else {
            panic!("expected columns");
        };
        for (a, d) in auto.iter().zip(dragged.iter()) {
            assert_eq!(
                a.width.is_some(),
                d.width.is_some(),
                "{:?} expanded/collapsed state is unchanged by dragging",
                a.kind
            );
        }
    }
}
