//! Golden-frame rendering tests via ratatui `TestBackend` (§14).

use stacksaw_ssp::types::{
    CommitSummary, FileEntry, FindingCounts, Segment, Snapshot, Staircase, SCHEMA_VERSION,
};
use stacksaw_ui::{render_to_lines, App};

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
            FileEntry { status: "A".into(), path: "src/codec.rs".into() },
            FileEntry { status: "M".into(), path: "src/lib.rs".into() },
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
    // Moving the selection makes it stale again.
    app.selected_commit = 1;
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
            FileEntry { status: "A".into(), path: "one.rs".into() },
            FileEntry { status: "M".into(), path: "two.rs".into() },
        ],
    );
    app.focused = stacksaw_ui::layout::ColumnKind::Files;
    let _ = render_to_lines(&app, 220, 60);
    // Scroll off-screen falls back to the focused Files column.
    app.on_scroll(0, 500, true);
    assert_eq!(app.selected_file, 1);
    app.on_scroll(0, 500, true); // clamps at last
    assert_eq!(app.selected_file, 1);
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
            FileEntry { status: "A".into(), path: "a".into() },
            FileEntry { status: "A".into(), path: "b".into() },
        ],
    );
    app.focused = stacksaw_ui::layout::ColumnKind::Files;
    app.move_selection(true);
    assert_eq!(app.selected_file, 1);
    app.move_selection(true); // clamps
    assert_eq!(app.selected_file, 1);
}

#[test]
fn diff_column_renders_loaded_diff() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(oid.clone(), vec![FileEntry { status: "M".into(), path: "src/lib.rs".into() }]);
    let patch = "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1,2 @@\n context\n+added line\n-removed line\n";
    app.set_diff(oid, "src/lib.rs".into(), patch, false);
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(joined.contains("added line"), "diff body should render");
    assert!(joined.contains("removed line"));
}

#[test]
fn added_file_shows_content() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(oid.clone(), vec![FileEntry { status: "A".into(), path: "new.rs".into() }]);
    assert!(app.selected_file_is_added());
    // Raw content (no diff prefixes) renders verbatim.
    let content = "fn main() {\n    println!(\"hi\");\n}\n";
    app.set_diff(oid, "new.rs".into(), content, true);
    let joined = render_to_lines(&app, 220, 60).join("\n");
    assert!(joined.contains("fn main()"), "content should render");
    assert!(joined.contains("println!"));
}

#[test]
fn diff_needing_load_tracks_file_selection() {
    let mut app = App::new(fixture_snapshot());
    let oid = app.selected_commit_oid().unwrap();
    app.set_files(
        oid.clone(),
        vec![
            FileEntry { status: "M".into(), path: "a.rs".into() },
            FileEntry { status: "M".into(), path: "b.rs".into() },
        ],
    );
    // First file needs a diff load.
    assert_eq!(
        app.diff_needing_load(),
        Some((oid.clone(), "a.rs".to_string()))
    );
    app.set_diff(oid.clone(), "a.rs".into(), "diff", false);
    assert_eq!(app.diff_needing_load(), None, "up to date after load");
    // Selecting the second file makes the diff stale for the new path.
    app.selected_file = 1;
    assert_eq!(app.diff_needing_load(), Some((oid, "b.rs".to_string())));
}

#[test]
fn scroll_moves_commit_selection() {
    let mut app = App::new(fixture_snapshot());
    let _ = render_to_lines(&app, 220, 60);
    assert_eq!(app.selected_commit, 0);
    // Scroll below the scene (no column under the pointer) falls back to the
    // focused Commits column; the fixture has two commits, so it steps to 1.
    app.on_scroll(0, 500, true);
    assert_eq!(app.selected_commit, 1);
    // Clamped at the last commit.
    app.on_scroll(0, 500, true);
    assert_eq!(app.selected_commit, 1);
    app.on_scroll(0, 500, false);
    assert_eq!(app.selected_commit, 0);
}
