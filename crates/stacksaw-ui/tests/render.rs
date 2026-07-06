//! Golden-frame rendering tests via ratatui `TestBackend` (§14).

use stacksaw_ssp::types::{
    CommitSummary, FileEntry, FindingCounts, Segment, Snapshot, Staircase, SCHEMA_VERSION,
    WORKTREE_OID,
};
use stacksaw_ui::command::{self, Action};
use stacksaw_ui::layout::ColumnKind;
use stacksaw_ui::viewport::RunContext;
use stacksaw_ui::{render_to_lines, App, RecentRowView, RecentsView};

fn fixture_snapshot() -> Snapshot {
    let commit = |short: &str, subject: &str| CommitSummary {
        oid: format!("{short}0000000000000000000000000000000000"),
        short: short.into(),
        subject: subject.into(),
        author: "Ada".into(),
        author_time: 1_780_000_000,
        parents: vec![],
        change_id: None,
        finding_counts: FindingCounts::default(),
        twins: vec![],
        added: 0,
        deleted: 0,
    };
    Snapshot {
        schema_version: SCHEMA_VERSION,
        generation: 1,
        head: Some("feat3".into()),
        detached: false,
        staircases: vec![Staircase {
            name: "feat/use-proto".into(),
            upstream: "origin/main".into(),
            ahead: 2,
            behind: 3,
            dirty: true,
            segments: vec![
                Segment {
                    branch: "feat/wire-proto".into(),
                    parent: None,
                    commits: vec![commit("8c1f", "Add codec")],
                },
                Segment {
                    branch: "feat/use-proto".into(),
                    parent: Some(0),
                    commits: vec![commit("22ab", "Route calls")],
                },
            ],
        }],
    }
}

#[test]
fn renders_columns_at_220x60() {
    let app = App::new(fixture_snapshot());
    let lines = render_to_lines(&app, 220, 60);
    let joined = lines.join("\n");
    assert!(joined.contains("Stacks"));
    assert!(joined.contains("Commits"));
    assert!(joined.contains("Diff"));
    assert!(joined.contains("feat/wire-proto"));
    assert!(joined.contains("8c1f"));
    assert!(joined.contains("Add codec"));
}

#[test]
fn multi_branch_staircase_shows_its_branch_count() {
    // The fixture staircase has two segments (branches), so the Stacks row is a
    // true staircase: its name plus a "(n branches)" count.
    let app = App::new(fixture_snapshot());
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(
        joined.contains("(2 branches)"),
        "a multi-branch staircase reports its branch count:\n{joined}"
    );
}

#[test]
fn dirty_stack_row_wears_a_star_not_a_pencil() {
    // The fixture stack is dirty: its Stacks row glues a "*" to the name, like
    // the run tab's `main*`, rather than the Commits worktree pencil.
    let app = App::new(fixture_snapshot());
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(
        joined.contains("feat/use-proto*"),
        "a dirty stack row marks dirtiness with a trailing *:\n{joined}"
    );
}

#[test]
fn lone_branch_is_not_labeled_a_staircase() {
    // A single-segment stack is just a branch: no "(n branches)" annotation.
    let mut snap = fixture_snapshot();
    snap.staircases[0].segments.truncate(1);
    let app = App::new(snap);
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(
        !joined.contains("branches)"),
        "a lone branch carries no branch-count annotation:\n{joined}"
    );
}

#[test]
fn stacks_ledger_shows_current_repo_header_stacks_then_other_repos() {
    let mut app = App::new(fixture_snapshot());
    app.set_recents(RecentsView {
        rows: vec![
            RecentRowView {
                path: "/repos/bazel-mono/services/payments".into(),
                parent: Some("bazel-mono".into()),
                label: "services/payments".into(),
                branch: Some("feat/pay".into()),
                current: true,
            },
            RecentRowView {
                path: "/repos/bazel-mono/services/auth".into(),
                parent: Some("bazel-mono".into()),
                label: "services/auth".into(),
                branch: Some("main".into()),
                current: false,
            },
            RecentRowView {
                path: "/repos/dotfiles".into(),
                parent: None,
                label: "dotfiles".into(),
                branch: None,
                current: false,
            },
        ],
    });
    let lines = render_to_lines(&app, 220, 60);
    let joined = lines.join("\n");
    // The current repo is a header line: "parent label" with no dot.
    assert!(
        joined.contains("bazel-mono services/payments"),
        "current repo header shows parent + label"
    );
    // Its staircase renders below the header.
    assert!(joined.contains("feat/use-proto"), "current repo's stack shown");
    // Other repos appear as their own single lines (parent prefix + label).
    assert!(joined.contains("services/auth"), "other monorepo repo row");
    assert!(joined.contains("dotfiles"), "loose repo row");
    // The checked-out branch trails each line where known.
    assert!(joined.contains("⎇ feat/pay"), "current repo branch marker");
    assert!(joined.contains("⎇ main"), "other repo branch marker");

    let current_line = lines.iter().find(|l| l.contains("services/payments")).unwrap();
    let other_line = lines.iter().find(|l| l.contains("services/auth")).unwrap();
    let current_marker_idx = current_line.find("⎇").unwrap();
    let other_marker_idx = other_line.find("⎇").unwrap();
    // current has "⎇ feat/pay" (len 10), other has "⎇ main" (len 6).
    // They are aligned separately, so the other's branch marker starts 4 characters later.
    assert_eq!(
        other_marker_idx - current_marker_idx,
        4,
        "current and other branch markers should align separately"
    );

    // Ordering: the current-repo header sits above its staircase, which sits
    // above the other-repo ledger at the bottom.
    let row = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle}"))
    };
    assert!(
        row("bazel-mono services/payments") < row("feat/use-proto"),
        "current-repo header is above its staircases"
    );
    assert!(
        row("feat/use-proto") < row("services/auth"),
        "other repos sit below the current repo's staircases"
    );
}

#[test]
fn arrowing_into_recents_and_activating_requests_a_switch() {
    use stacksaw_ui::command::Action;

    let mut app = App::new(fixture_snapshot()); // one staircase
    app.focused = ColumnKind::Stacks;
    app.set_recents(RecentsView {
        rows: vec![
            RecentRowView {
                path: "/repos/bazel-mono/services/payments".into(),
                parent: Some("bazel-mono".into()),
                label: "services/payments".into(),
                branch: Some("main".into()),
                current: true,
            },
            RecentRowView {
                path: "/repos/bazel-mono/services/auth".into(),
                parent: Some("bazel-mono".into()),
                label: "services/auth".into(),
                branch: Some("main".into()),
                current: false,
            },
        ],
    });

    // Activating while the cursor is still on a staircase does nothing.
    app.apply(Action::Activate);
    assert_eq!(app.pending_switch, None, "staircases don't switch repos");

    // Arrow down past the lone staircase drops into the first recent repo,
    // and activating it requests a switch to that repo's workdir.
    app.apply(Action::MoveDown);
    app.apply(Action::Activate);
    assert_eq!(
        app.pending_switch.as_deref(),
        Some(std::path::Path::new("/repos/bazel-mono/services/auth")),
    );
}

#[test]
fn clicking_a_recent_selects_first_then_switches_on_second_click() {
    let mut app = App::new(fixture_snapshot()); // one staircase
    app.focused = ColumnKind::Stacks;
    app.set_recents(RecentsView {
        rows: vec![
            RecentRowView {
                path: "/repos/bazel-mono/services/payments".into(),
                parent: Some("bazel-mono".into()),
                label: "services/payments".into(),
                branch: Some("main".into()),
                current: true,
            },
            RecentRowView {
                path: "/repos/bazel-mono/services/auth".into(),
                parent: Some("bazel-mono".into()),
                label: "services/auth".into(),
                branch: Some("main".into()),
                current: false,
            },
        ],
    });
    // Render populates the hit map; find the screen row of the recent repo.
    let lines = render_to_lines(&app, 220, 60);
    let y = lines
        .iter()
        .position(|l| l.contains("services/auth"))
        .expect("recent repo row rendered") as u16;

    // First click only selects — it must not switch out from under the user.
    app.on_click(2, y);
    assert_eq!(app.pending_switch, None, "first click selects, does not switch");

    // Second click on the now-selected row opens it.
    app.on_click(2, y);
    assert_eq!(
        app.pending_switch.as_deref(),
        Some(std::path::Path::new("/repos/bazel-mono/services/auth")),
    );
}

#[test]
fn columns_show_a_glyph_legend_at_the_bottom() {
    let app = App::new(fixture_snapshot());
    let joined = render_to_lines(&app, 220, 60).join("\n");
    // Stacks explains its counters + the dirty marker (fixture is dirty).
    assert!(joined.contains("↑ ahead"), "Stacks legend: ahead");
    assert!(joined.contains("↓ behind"), "Stacks legend: behind");
    // The Stacks dirty marker is a "*" (like the run tab), not the pencil.
    assert!(joined.contains("* uncommitted"), "Stacks legend: dirty");
    // Commits explains its structural + status glyphs actually shown.
    assert!(joined.contains("╭┴ branch"), "Commits legend: branch");
    assert!(joined.contains("✓ clean"), "Commits legend: clean");
}

#[test]
fn diff_pane_is_full_width_below_the_columns() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(
        oid.clone(),
        vec![FileEntry { status: "A".into(), path: "wide.txt".into(), ..Default::default() }],
    );
    app.selected_file = 1;
    // A line far wider than any single top column (each ~1/3 of 220): if it
    // renders in full, the pane must span the whole width beneath the columns.
    let wide = "X".repeat(180);
    app.set_diff(oid, "wide.txt".into(), &wide, true);
    let lines = render_to_lines(&app, 220, 60);
    let stacks_row = lines.iter().position(|l| l.contains("Stacks")).expect("Stacks");
    let commits_row = lines.iter().position(|l| l.contains("Commits")).expect("Commits");
    let diff_row = lines.iter().position(|l| l.contains("Diff")).expect("Diff");
    // Stacks/Commits share the top band; the Diff tab sits on a lower row.
    assert_eq!(stacks_row, commits_row, "master columns share the top band");
    assert!(diff_row > stacks_row, "Diff pane is below the columns");
    // The wide line renders below the tab bar and spans well past one column.
    let wide_row = lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("XXXXXXXXXX"))
        .expect("wide diff content renders");
    assert!(wide_row.0 as usize > diff_row, "content is below the tab bar");
    assert!(
        wide_row.1.chars().count() >= 180,
        "Diff pane should be full width, got {}",
        wide_row.1.chars().count()
    );
}

#[test]
fn deck_mode_below_100_cols() {
    let app = App::new(fixture_snapshot());
    let lines = render_to_lines(&app, 90, 24);
    let joined = lines.join("\n");
    // Deck mode shows a breadcrumb.
    assert!(joined.contains("Stacks ▸"), "expected breadcrumb, got:\n{joined}");
}

#[test]
fn renders_at_minimum_size_80x24() {
    let app = App::new(fixture_snapshot());
    let lines = render_to_lines(&app, 80, 24);
    assert_eq!(lines.len(), 24);
}

/// A snapshot with two staircases so stack-row clicks are observable.
fn two_stair_snapshot() -> Snapshot {
    let mut snap = fixture_snapshot();
    let mut second = snap.staircases[0].clone();
    second.name = "feat/other".into();
    second.segments = vec![Segment {
        branch: "feat/other".into(),
        parent: None,
        commits: snap.staircases[0].segments[0].commits.clone(),
    }];
    snap.staircases.push(second);
    snap
}

#[test]
fn commit_subject_uses_available_column_width() {
    let long = "Refactor the staircase model to support rootless segments and \
                fallback upstream resolution across every configured tracking ref";
    let mut snap = fixture_snapshot();
    snap.staircases[0].segments[0].commits[0].subject = long.into();

    // Wide terminal: the Commits column is large and should show much more of
    // the subject than a fixed 48-char cap ever would.
    let wide = render_to_lines(&App::new(snap.clone()), 260, 20).join("\n");
    assert!(
        wide.contains("rootless segments"),
        "wide column should reveal more of the subject:\n{wide}"
    );

    // Narrow (deck) mode: the same long subject must be truncated with an
    // ellipsis rather than overflowing.
    let narrow = render_to_lines(&App::new(snap), 90, 20).join("\n");
    assert!(narrow.contains('…'), "narrow view should ellipsize:\n{narrow}");
}

#[test]
fn click_selects_stack_row() {
    let mut app = App::new(two_stair_snapshot());
    // Populate the hit map for a wide layout (Stacks is the leftmost column).
    let _ = render_to_lines(&app, 220, 60);
    // Stacks column has a border, so its first row is at inner y = 1; the
    // second staircase renders on the next row.
    app.on_click(2, 2);
    assert_eq!(app.selected_stair, 1);
    assert_eq!(app.selected_commit, 0);
}

#[test]
fn files_column_renders_loaded_files() {
    let mut app = App::new(fixture_snapshot());
    app.set_files(
        app.selected_commit_oid().unwrap(),
        vec![
            FileEntry { status: "A".into(), path: "src/codec.rs".into(), ..Default::default() },
            FileEntry { status: "M".into(), path: "src/lib.rs".into(), ..Default::default() },
        ],
    );
    let joined = render_to_lines(&app, 220, 60).join("\n");
    // Filename first, directory shown separately afterwards.
    assert!(joined.contains("codec.rs"), "file name should render");
    assert!(joined.contains("lib.rs"));
    assert!(joined.contains("src"), "directory should still be shown");
}

#[test]
fn files_needing_load_tracks_selection() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().expect("a selected commit");
    // Nothing loaded yet → needs the selected commit's files.
    assert_eq!(app.files_needing_load().as_deref(), Some(oid.as_str()));
    app.set_files(oid, vec![]);
    assert_eq!(app.files_needing_load(), None, "up to date after load");
    // Moving the selection off the tip (the default) makes it stale again.
    app.selected_commit = 0;
    assert!(app.files_needing_load().is_some());
}

#[test]
fn selected_commit_shows_marker() {
    let app = App::new(fixture_snapshot());
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(joined.contains('▶'), "selected commit should show a marker");
}

#[test]
fn scroll_over_focused_files_moves_file_selection() {
    let mut app = App::new(fixture_snapshot());
    app.set_files(
        app.selected_commit_oid().unwrap(),
        vec![
            FileEntry { status: "A".into(), path: "one.rs".into(), ..Default::default() },
            FileEntry { status: "M".into(), path: "two.rs".into(), ..Default::default() },
        ],
    );
    app.focused = stacksaw_ui::layout::ColumnKind::Files;
    let _ = render_to_lines(&app, 220, 60);
    // Rows: [commit message, one.rs, two.rs]. Scroll off-screen falls back to
    // the focused Files column.
    app.on_scroll(0, 500, true);
    assert_eq!(app.selected_file, 1);
    app.on_scroll(0, 500, true);
    assert_eq!(app.selected_file, 2);
    app.on_scroll(0, 500, true); // clamps at last
    assert_eq!(app.selected_file, 2);
}

#[test]
fn focused_column_drives_navigation() {
    let mut app = App::new(two_stair_snapshot());
    // With Stacks focused, j moves between stacks.
    app.focused = stacksaw_ui::layout::ColumnKind::Stacks;
    assert_eq!(app.selected_stair, 0);
    app.move_selection(true);
    assert_eq!(app.selected_stair, 1);
    app.move_selection(false);
    assert_eq!(app.selected_stair, 0);

    // With Files focused, j moves between files.
    app.set_files(
        app.selected_commit_oid().unwrap(),
        vec![
            FileEntry { status: "A".into(), path: "a".into(), ..Default::default() },
            FileEntry { status: "A".into(), path: "b".into(), ..Default::default() },
        ],
    );
    // Rows: [commit message, a, b].
    app.focused = stacksaw_ui::layout::ColumnKind::Files;
    app.move_selection(true);
    assert_eq!(app.selected_file, 1);
    app.move_selection(true);
    assert_eq!(app.selected_file, 2);
    app.move_selection(true); // clamps
    assert_eq!(app.selected_file, 2);
}

#[test]
fn diff_column_renders_loaded_diff() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(oid.clone(), vec![FileEntry { status: "M".into(), path: "src/lib.rs".into(), ..Default::default() }]);
    let patch = "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1,2 @@\n context\n+added line\n-removed line\n";
    app.set_diff(oid, "src/lib.rs".into(), patch, false);
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(joined.contains("added line"), "diff body should render");
    assert!(joined.contains("removed line"));
}

#[test]
fn modified_file_diff_shows_whole_file_with_line_backgrounds() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(oid.clone(), vec![FileEntry { status: "M".into(), path: "src/lib.rs".into(), ..Default::default() }]);
    app.selected_file = 1; // row 0 is the commit-message entry
    let patch = "diff --git a/src/lib.rs b/src/lib.rs\n\
                 index 1111111..2222222 100644\n\
                 --- a/src/lib.rs\n+++ b/src/lib.rs\n\
                 @@ -1,3 +1,3 @@\n keep one\n-old line\n+new line\n keep two\n";
    app.set_diff(oid, "src/lib.rs".into(), patch, false);
    let joined = render_to_lines(&app, 220, 60).join("\n");
    // Whole file is shown, including unchanged context lines.
    assert!(joined.contains("keep one") && joined.contains("keep two"), "context shown:\n{joined}");
    assert!(joined.contains("new line") && joined.contains("old line"));
    // Structural rows are hidden and markers are stripped from the body.
    assert!(!joined.contains("@@"), "hunk header hidden");
    assert!(!joined.contains("diff --git"), "git header hidden");
    assert!(!joined.contains("+new line"), "leading marker stripped");
}

#[test]
fn diff_rows_carry_before_after_line_numbers() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(oid.clone(), vec![FileEntry { status: "M".into(), path: "src/lib.rs".into(), ..Default::default() }]);
    app.selected_file = 1; // row 0 is the commit-message entry
    // Hunk starts at line 10 on both sides: keep(10/10), del(11/–), add(–/11),
    // keep(12/12) — each side's counter advances independently.
    let patch = "diff --git a/src/lib.rs b/src/lib.rs\n\
                 @@ -10,3 +10,3 @@\n keep one\n-old line\n+new line\n keep two\n";
    app.set_diff(oid, "src/lib.rs".into(), patch, false);
    let lines = render_to_lines(&app, 220, 60);
    let row = |needle: &str| {
        lines.iter().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("row {needle} not found"))
    };
    // Context row shows both numbers; the added row numbers only the new side,
    // the deleted row only the old side.
    assert!(row("keep one").contains("10 10"), "context numbers both sides");
    assert!(row("keep two").contains("12 12"), "context numbers advance per side");
    assert!(row("new line").contains("11"), "added row shows its new-side number");
    assert!(row("old line").contains("11"), "deleted row shows its old-side number");
}

#[test]
fn modified_file_diff_opens_scrolled_to_first_change() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(oid.clone(), vec![FileEntry { status: "M".into(), path: "f.rs".into(), ..Default::default() }]);
    app.selected_file = 1;
    // Ten unchanged lines, then a deletion/addition far below the top.
    let mut patch = String::from(
        "diff --git a/f.rs b/f.rs\nindex 1..2 100644\n--- a/f.rs\n+++ b/f.rs\n@@ -1,11 +1,11 @@\n",
    );
    for i in 0..10 {
        patch.push_str(&format!(" ctx line {i}\n"));
    }
    patch.push_str("-old\n+new\n");
    app.set_diff(oid, "f.rs".into(), &patch, false);
    // First change is body row 10; keep 3 context rows above → scroll to 7.
    assert_eq!(app.diff_scroll(), 7);
}

#[test]
fn added_file_shows_content() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(oid.clone(), vec![FileEntry { status: "A".into(), path: "new.rs".into(), ..Default::default() }]);
    // Row 0 is the pinned commit-message entry; the added file is row 1.
    app.selected_file = 1;
    assert!(app.selected_file_is_added());
    // Raw content (no diff prefixes) renders verbatim.
    let content = "fn main() {\n    println!(\"hi\");\n}\n";
    app.set_diff(oid, "new.rs".into(), content, true);
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(joined.contains("fn main()"), "content should render");
    assert!(joined.contains("println!"));
}

#[test]
fn commit_message_row_pinned_and_shows_in_diff() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(oid.clone(), vec![FileEntry { status: "M".into(), path: "src/lib.rs".into(), ..Default::default() }]);
    // The virtual row is pinned at the top and selected by default.
    assert_eq!(app.selected_file, 0);
    assert!(app.selected_file_is_message());
    assert!(!app.selected_file_is_added());
    // Its diff key is the message path, and the host renders raw message text.
    let (load_oid, path) = app.diff_needing_load().expect("message needs loading");
    assert_eq!(load_oid, oid);
    app.set_diff(oid, path, "Add codec\n\nWire the proto codec end to end.\n", true);
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(joined.contains("commit message"), "labelled row should render");
    assert!(joined.contains("Wire the proto codec"), "message body in Diff");
}

#[test]
fn diff_needing_load_tracks_file_selection() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(
        oid.clone(),
        vec![
            FileEntry { status: "M".into(), path: "a.rs".into(), ..Default::default() },
            FileEntry { status: "M".into(), path: "b.rs".into(), ..Default::default() },
        ],
    );
    // Row 0 is the commit message; the first real file is row 1.
    app.selected_file = 1;
    assert_eq!(
        app.diff_needing_load(),
        Some((oid.clone(), "a.rs".to_string()))
    );
    app.set_diff(oid.clone(), "a.rs".into(), "diff", false);
    assert_eq!(app.diff_needing_load(), None, "up to date after load");
    // Selecting the second file makes the diff stale for the new path.
    app.selected_file = 2;
    assert_eq!(app.diff_needing_load(), Some((oid, "b.rs".to_string())));
}

#[test]
fn scroll_moves_commit_selection() {
    let mut app = App::new(fixture_snapshot());
    let _ = render_to_lines(&app, 220, 60);
    // The default selection opens on the stack tip (ToT); the fixture has two
    // commits, so that is index 1.
    assert_eq!(app.selected_commit, 1);
    // Scroll below the scene (no column under the pointer) falls back to the
    // focused Commits column; already at the tip, so it clamps.
    app.on_scroll(0, 500, true);
    assert_eq!(app.selected_commit, 1);
    // Scrolling up steps toward the base, then clamps there.
    app.on_scroll(0, 500, false);
    assert_eq!(app.selected_commit, 0);
    app.on_scroll(0, 500, false);
    assert_eq!(app.selected_commit, 0);
}

#[test]
fn virtual_worktree_commit_renders_as_uncommitted_changes() {
    let mut snap = fixture_snapshot();
    // Append the virtual worktree commit to the tip segment, as the snapshot
    // builder does when the tree is dirty.
    let tip = snap.staircases[0].segments.last_mut().unwrap();
    tip.commits.push(CommitSummary {
        oid: WORKTREE_OID.into(),
        short: WORKTREE_OID.into(),
        subject: "Uncommitted changes".into(),
        author: String::new(),
        author_time: 0,
        parents: vec![],
        change_id: None,
        finding_counts: FindingCounts::default(),
        twins: vec![],
        added: 4,
        deleted: 1,
    });
    let app = App::new(snap);
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(
        joined.contains("✎ Uncommitted changes"),
        "worktree commit renders with its label"
    );
    // The sentinel oid itself must never be shown as a hash.
    assert!(!joined.contains(WORKTREE_OID), "sentinel oid is not displayed");
}

#[test]
fn commits_and_files_show_churn_annotation() {
    // A commit and a file both carry `-N +M` line-churn counts.
    let mut snap = fixture_snapshot();
    snap.staircases[0].segments[0].commits[0].added = 12;
    snap.staircases[0].segments[0].commits[0].deleted = 3;
    let mut app = App::new(snap);
    app.set_files(
        app.selected_commit_oid().unwrap(),
        vec![
            FileEntry {
                status: "M".into(),
                path: "src/lib.rs".into(),
                added: 7,
                deleted: 2,
            },
            // A pure addition: the `-0` half must be suppressed.
            FileEntry {
                status: "A".into(),
                path: "new.rs".into(),
                added: 5,
                deleted: 0,
            },
        ],
    );
    let joined = render_to_lines(&app, 220, 60).join("\n");
    // Commit churn.
    assert!(joined.contains("-3"), "commit deletions should render");
    assert!(joined.contains("+12"), "commit additions should render");
    // File churn.
    assert!(joined.contains("-2"), "file deletions should render");
    assert!(joined.contains("+7"), "file additions should render");
    // Zero halves are suppressed entirely.
    assert!(joined.contains("+5"), "added-only file shows its additions");
    assert!(!joined.contains("-0"), "no `-0` churn text anywhere");
    assert!(!joined.contains("+0"), "no `+0` churn text anywhere");
}

#[test]
fn zooming_a_top_column_keeps_the_diff_pane() {
    let mut app = App::new(fixture_snapshot());
    // Load a diff so the Diff pane has recognizable content.
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(oid.clone(), vec![]);
    app.set_diff(oid, "src/lib.rs".into(), "diff --git a b\n zoomed-diff-body\n", false);
    // Focus and zoom the Commits column.
    app.apply(Action::Focus(ColumnKind::Commits));
    app.apply(Action::ToggleZoom);
    let joined = render_to_lines(&app, 160, 40).join("\n");
    assert!(joined.contains("Diff"), "Diff pane stays visible when a top column is zoomed");
    assert!(joined.contains("zoomed-diff-body"), "Diff content still renders");
}

#[test]
fn adjacent_top_columns_share_a_single_divider() {
    // Zooming a top column collapses its siblings to spines; the spine's right
    // border must be the *only* divider (no doubled "││").
    let mut app = App::new(fixture_snapshot());
    app.apply(Action::Focus(ColumnKind::Files));
    app.apply(Action::ToggleZoom);
    let lines = render_to_lines(&app, 160, 40);
    for line in &lines {
        assert!(
            !line.contains("││"),
            "doubled vertical divider in row: {line:?}"
        );
    }
    // The band's top border stitches dividers into `┬` tees (and `┴` on the
    // bottom border) rather than leaving disconnected corners.
    assert!(lines[0].contains("┬"), "top border has tee junctions: {:?}", lines[0]);
    assert!(
        lines.iter().any(|l| l.contains("┴")),
        "bottom border has tee junctions"
    );
}

#[test]
fn archiving_a_stack_queues_all_its_branch_names() {
    use stacksaw_ui::command::Action;
    let mut app = App::new(fixture_snapshot());
    app.focused = ColumnKind::Stacks;
    // `a` in the Stacks column archives the selected stack.
    app.apply(Action::ArchiveStack);
    // The intent carries every segment branch, so the host can park the stack.
    assert_eq!(
        app.take_pending_archive(),
        Some(vec!["feat/wire-proto".to_string(), "feat/use-proto".to_string()])
    );
    // Consumed once.
    assert_eq!(app.take_pending_archive(), None);
}

#[test]
fn archive_is_bound_to_a_only_in_the_stacks_column() {
    use crossterm::event::{KeyCode, KeyEvent};
    use stacksaw_ui::command::{self, Action};
    let a = KeyEvent::from(KeyCode::Char('a'));
    assert_eq!(command::lookup(&a, ColumnKind::Stacks), Some(Action::ArchiveStack));
    assert_eq!(command::lookup(&a, ColumnKind::Commits), None);
}

#[test]
fn hint_bar_shows_registry_keys() {
    let app = App::new(fixture_snapshot());
    let lines = render_to_lines(&app, 120, 30);
    // The hint bar is the bottom row and projects the command registry.
    let bar = lines.last().unwrap();
    assert!(bar.contains("Move down"), "hint bar advertises navigation");
    assert!(bar.contains("Command palette"), "hint bar advertises the palette");
    assert!(bar.contains("Help"), "hint bar advertises help");
}

#[test]
fn help_overlay_lists_commands_by_category() {
    let mut app = App::new(fixture_snapshot());
    app.apply(Action::OpenHelp);
    let joined = render_to_lines(&app, 120, 40).join("\n");
    assert!(joined.contains("Help — keys"), "help overlay is titled");
    assert!(joined.contains("Navigate"), "category headings render");
    assert!(joined.contains("Quit"), "commands are listed");
}

#[test]
fn palette_opens_filters_and_confirms() {
    let mut app = App::new(fixture_snapshot());
    app.apply(Action::OpenPalette);
    // Type a fuzzy query for "zoom".
    for c in "zoom".chars() {
        app.palette_input(c);
    }
    let joined = render_to_lines(&app, 120, 40).join("\n");
    assert!(joined.contains("Command palette"), "palette overlay is titled");
    assert!(joined.contains("Zoom column"), "fuzzy query surfaces the command");
    // Confirming the top result returns its action and closes the palette.
    let action = app.palette_confirm();
    assert_eq!(action, Some(Action::ToggleZoom));
}

#[test]
fn exec_target_resolves_to_selected_commit() {
    let app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    let target = app.exec_target();
    assert_eq!(target.oid.as_deref(), Some(oid.as_str()));
    assert_eq!(target.label, oid.chars().take(7).collect::<String>());
}

#[test]
fn viewport_tab_bar_shows_diff_tab_with_close() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(
        oid.clone(),
        vec![FileEntry { status: "M".into(), path: "src/lib.rs".into(), ..Default::default() }],
    );
    app.selected_file = 1;
    app.set_diff(
        oid,
        "src/lib.rs".into(),
        "diff --git a b\n@@ -1 +1 @@\n-old\n+new\n",
        false,
    );
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(joined.contains("Diff"), "Diff tab is labelled");
    assert!(joined.contains('x'), "a close control is present on the tab bar");
    assert!(joined.contains("new"), "the diff body renders under the tab");
}

#[test]
fn run_tab_emulates_ansi_output() {
    let mut app = App::new(fixture_snapshot());
    app.focused = ColumnKind::Diff;
    // Open a command terminal tab (as the host does after spawning a PTY) and
    // feed it a byte stream, including a carriage return + newline.
    app.open_run(
        1,
        "echo hi".into(),
        "abc1234".into(),
        Some("abc1234ff".into()),
        RunContext::default(),
        20,
        80,
    );
    app.push_pty_output(1, b"hello \x1b[32mworld\x1b[0m\r\n");
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(joined.contains("hello world"), "vt100 renders the terminal cells");
    assert!(joined.contains("abc1234"), "the run tab carries its label");
}

#[test]
fn run_tab_shows_a_context_header() {
    let mut app = App::new(fixture_snapshot());
    app.focused = ColumnKind::Diff;
    app.open_run(
        7,
        "cargo test".into(),
        "abc1234".into(),
        Some("abc1234ff".into()),
        RunContext { repo_root: "~/proj".into(), git_dir: ".git".into() },
        20,
        80,
    );
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(
        joined.contains("cargo test   ~/proj (.git) @ abc1234"),
        "the header names the command and the repo/git/target context:\n{joined}"
    );
    // Once the command exits, the header reports the code textually (not by the
    // tab-badge color alone), per P6.
    app.finish_run(7, 2);
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(joined.contains("exited 2"), "the header reports the exit code:\n{joined}");
}

#[test]
fn stacks_selection_targets_the_stack_tip_by_name() {
    let mut app = App::new(fixture_snapshot());
    // Selecting a stack in Stacks means "this whole stack": the target is the
    // stack's tip, named by the staircase (its tip branch), regardless of where
    // the Commits cursor sits.
    app.focused = ColumnKind::Stacks;
    app.selected_commit = 0;
    let target = app.exec_target();
    assert_eq!(target.label, "feat/use-proto");
    // The tip is the last commit of the last segment (feat/use-proto → 22ab),
    // not the base segment the cursor defaults into.
    assert_eq!(target.oid.as_deref(), Some("22ab0000000000000000000000000000000000"));
    // A specific commit chosen in Commits/Files is named by its short oid.
    app.focused = ColumnKind::Commits;
    assert_eq!(app.exec_target().label, "8c1f000");
}

#[test]
fn stacks_selection_targets_the_branch_tip_not_the_commit_cursor() {
    let mut snap = fixture_snapshot();
    // Dirty tip segment: append the virtual worktree commit (the live on-disk
    // state), as the snapshot builder does for the checked-out branch.
    let seg = snap.staircases[0].segments.last_mut().unwrap();
    let branch = seg.branch.clone();
    seg.commits.push(CommitSummary {
        oid: WORKTREE_OID.into(),
        short: WORKTREE_OID.into(),
        subject: "Uncommitted changes".into(),
        author: String::new(),
        author_time: 0,
        parents: vec![],
        change_id: None,
        finding_counts: FindingCounts::default(),
        twins: vec![],
        added: 4,
        deleted: 1,
    });
    let mut app = App::new(snap);
    app.focused = ColumnKind::Stacks;
    // Park the commit cursor on an ancestor within the tip segment (index 1 is
    // the original tip commit, not the worktree row).
    app.selected_commit = 1;
    let target = app.exec_target();
    // A Stacks run targets the branch tip — here the live working tree — so it
    // stays in the physical checkout instead of isolating the ancestor commit.
    assert_eq!(target.oid.as_deref(), Some(WORKTREE_OID));
    assert_eq!(target.label, branch);
}

#[test]
fn worktree_target_is_named_after_the_branch() {
    let mut snap = fixture_snapshot();
    // Append the virtual worktree commit to the tip segment, as the snapshot
    // builder does when the tree is dirty.
    let tip = snap.staircases[0].segments.last_mut().unwrap();
    let branch = tip.branch.clone();
    tip.commits.push(CommitSummary {
        oid: WORKTREE_OID.into(),
        short: WORKTREE_OID.into(),
        subject: "Uncommitted changes".into(),
        author: String::new(),
        author_time: 0,
        parents: vec![],
        change_id: None,
        finding_counts: FindingCounts::default(),
        twins: vec![],
        added: 4,
        deleted: 1,
    });
    let total: usize = snap.staircases[0]
        .segments
        .iter()
        .map(|s| s.commits.len())
        .sum();
    let mut app = App::new(snap);
    // Select the worktree row (last commit in the flattened staircase).
    app.selected_commit = total - 1;
    let target = app.exec_target();
    assert_eq!(target.oid.as_deref(), Some(WORKTREE_OID));
    // The live on-disk checkout is named after its branch (not the bare word
    // "worktree"); the `*` dirty marker is a live, render-time decoration.
    assert_eq!(target.label, branch);
}

#[test]
fn worktree_run_tab_shows_a_live_dirty_marker() {
    let mut snap = fixture_snapshot();
    let branch = snap.staircases[0].segments.last().unwrap().branch.clone();
    snap.staircases[0].dirty = true;
    let mut app = App::new(snap);
    app.focused = ColumnKind::Diff;
    // A run against the working tree (WORKTREE_OID) tracks live dirtiness.
    app.open_run(
        11,
        "zsh -i".into(),
        branch.clone(),
        Some(WORKTREE_OID.into()),
        RunContext::default(),
        20,
        80,
    );
    // Dirty tree: the tab/header show the branch with a live `*`.
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(
        joined.contains(&format!("{branch}*")),
        "a dirty worktree run shows the live * marker:\n{joined}"
    );
    // Clean the tree: the `*` disappears (it is not baked into the label).
    app.snapshot.staircases[0].dirty = false;
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(
        !joined.contains(&format!("{branch}*")),
        "a clean worktree drops the * marker:\n{joined}"
    );
    assert!(
        joined.contains(&branch),
        "the branch name still shows when clean:\n{joined}"
    );
}

#[test]
fn run_header_pins_commit_when_target_is_a_branch() {
    let mut app = App::new(fixture_snapshot());
    app.focused = ColumnKind::Diff;
    app.open_run(
        3,
        "cargo test".into(),
        "fix-tui-mouse-lag".into(),
        Some("c63c0f66aabbccddee".into()),
        RunContext { repo_root: "~/p".into(), git_dir: ".git".into() },
        20,
        80,
    );
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(
        joined.contains("@ fix-tui-mouse-lag · c63c0f6"),
        "a branch target still pins the exact commit:\n{joined}"
    );
}

#[test]
fn finished_run_shows_action_buttons_and_close_works() {
    let mut app = App::new(fixture_snapshot());
    app.focused = ColumnKind::Diff;
    app.open_run(9, "echo hi".into(), "run".into(), None, RunContext::default(), 20, 80);
    app.push_pty_output(9, b"done\r\n");
    app.finish_run(9, 0);
    let (w, h) = (220u16, 60u16);
    let lines = render_to_lines(&app, w, h);
    let joined = lines.join("\n");
    assert!(joined.contains("Run Again"), "finished run offers Run Again:\n{joined}");
    assert!(joined.contains("Close Tab"), "finished run offers Close Tab:\n{joined}");

    // Clicking "Close Tab" closes the run tab (falling back to the Diff tab).
    let (y, line) = lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("Close Tab"))
        .map(|(i, l)| (i as u16, l.clone()))
        .expect("close button row");
    let col = col_of(&line, "Close Tab").unwrap();
    app.on_click(col, y);
    let after = render_to_lines(&app, w, h).join("\n");
    assert!(!after.contains("Run Again"), "the run tab closed:\n{after}");
}

#[test]
fn run_tab_reopens_diff_on_file_select() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(
        oid.clone(),
        vec![FileEntry { status: "M".into(), path: "src/lib.rs".into(), ..Default::default() }],
    );
    app.selected_file = 1;
    app.set_diff(oid, "src/lib.rs".into(), "diff --git a b\n@@ -1 +1 @@\n-old\n+new\n", false);
    // Open a run tab and switch to it, then close the Diff tab.
    app.open_run(1, "ls".into(), "run".into(), None, RunContext::default(), 20, 80);
    app.apply(Action::Focus(ColumnKind::Diff));
    // Selecting a new file should re-load the diff and reopen its tab.
    app.selected_file = 0;
    if let Some((o, p)) = app.diff_needing_load() {
        app.set_diff(o, p, "message body\n", true);
    }
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(joined.contains("Diff"), "Diff tab reappears after a file selection");
    assert!(joined.contains("run"), "the command tab is still present");
}

/// Visual column (cell index) where `needle` first appears in a rendered line.
/// Cells are one char each in `render_to_lines`, so the char index is the x.
fn col_of(line: &str, needle: &str) -> Option<u16> {
    let chars: Vec<char> = line.chars().collect();
    let n: Vec<char> = needle.chars().collect();
    if n.is_empty() || chars.len() < n.len() {
        return None;
    }
    (0..=chars.len() - n.len())
        .find(|&i| chars[i..i + n.len()] == n[..])
        .map(|i| i as u16)
}

#[test]
fn clicking_viewport_tabs_selects_and_closes_them() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(
        oid.clone(),
        vec![FileEntry { status: "M".into(), path: "src/lib.rs".into(), ..Default::default() }],
    );
    app.selected_file = 1;
    app.set_diff(oid, "src/lib.rs".into(), "diff --git a b\n@@ -1 +1 @@\n-old\n+new\n", false);
    // Open a command tab; it becomes active, so the diff body is hidden.
    app.open_run(1, "ls".into(), "runjob".into(), None, RunContext::default(), 20, 80);

    let (w, h) = (220u16, 60u16);
    let lines = render_to_lines(&app, w, h);
    let (y, line) = lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("Diff") && l.contains("runjob"))
        .map(|(i, l)| (i as u16, l.clone()))
        .expect("viewport tab bar row present");

    // Clicking the Diff tab makes it active — its body (containing "new") shows.
    let diff_col = col_of(&line, "Diff").expect("Diff tab label");
    app.on_click(diff_col, y);
    assert!(
        render_to_lines(&app, w, h).join("\n").contains("new"),
        "clicking the Diff tab selects it"
    );

    // Clicking the close 'x' of the runjob tab removes it.
    let after_label = col_of(&line, "runjob").expect("run tab label") + "runjob".chars().count() as u16;
    let x_col = (after_label..w)
        .find(|&c| line.chars().nth(c as usize) == Some('x'))
        .expect("close control after the run tab label");
    app.on_click(x_col, y);
    assert!(
        !render_to_lines(&app, w, h).join("\n").contains("runjob"),
        "clicking the italic x closes the tab"
    );
}

#[test]
fn closing_the_only_tab_renders_an_empty_viewport_without_panicking() {
    let mut app = App::new(fixture_snapshot());
    let (w, h) = (220u16, 60u16);
    let lines = render_to_lines(&app, w, h);
    let (y, line) = lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("Diff"))
        .map(|(i, l)| (i as u16, l.clone()))
        .expect("viewport tab bar row present");
    // The close 'x' sits just after the sole Diff tab's label.
    let after_label = col_of(&line, "Diff").unwrap() + "Diff".chars().count() as u16;
    let x_col = (after_label..w)
        .find(|&c| line.chars().nth(c as usize) == Some('x'))
        .expect("close control after the Diff tab label");
    app.on_click(x_col, y);
    // Rendering the now-tabless viewport must not panic and shows a hint.
    let out = render_to_lines(&app, w, h).join("\n");
    assert!(out.contains("no tabs"), "empty viewport shows a hint, got:\n{out}");
}

#[test]
fn lookup_resolves_keys_to_actions() {
    use crossterm::event::{KeyCode, KeyEvent};
    let ev = |code| KeyEvent::from(code);
    assert_eq!(
        command::lookup(&ev(KeyCode::Char('j')), ColumnKind::Commits),
        Some(Action::MoveDown)
    );
    assert_eq!(
        command::lookup(&ev(KeyCode::Char('>')), ColumnKind::Commits),
        Some(Action::OpenRunPrompt)
    );
    assert_eq!(
        command::lookup(&ev(KeyCode::Char(':')), ColumnKind::Commits),
        Some(Action::OpenPalette)
    );
    assert_eq!(
        command::lookup(&ev(KeyCode::Char('?')), ColumnKind::Commits),
        Some(Action::OpenHelp)
    );
    assert_eq!(
        command::lookup(&ev(KeyCode::Char('x')), ColumnKind::Commits),
        None
    );
}

#[test]
fn top_branch_name_is_not_elided_unconditionally() {
    let mut app = App::new(fixture_snapshot());
    app.focused = ColumnKind::Stacks;
    app.zoom = true;
    app.set_recents(RecentsView {
        rows: vec![
            RecentRowView {
                path: "/repos/bazel-mono/services/payments".into(),
                parent: Some("bazel-mono".into()),
                label: "services/payments".into(),
                branch: Some("feat/payment-gateway-setup-integration-test".into()),
                current: true,
            },
            RecentRowView {
                path: "/repos/dotfiles".into(),
                parent: None,
                label: "dotfiles".into(),
                branch: None,
                current: false,
            },
        ],
    });
    // Width 220 has plenty of space.
    let lines = render_to_lines(&app, 220, 60);
    let joined = lines.join("\n");
    // Assert that the full branch name is present in the output.
    assert!(
        joined.contains("⎇ feat/payment-gateway-setup-integration-test"),
        "top branch name should render in full without elision since there is plenty of room. Output was:\n{}",
        joined
    );
}
