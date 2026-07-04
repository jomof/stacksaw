//! Golden-frame rendering tests via ratatui `TestBackend` (§14).

use stacksaw_ssp::types::{
    CommitSummary, FileEntry, FindingCounts, Segment, Snapshot, Staircase, SCHEMA_VERSION,
    WORKTREE_OID,
};
use stacksaw_ui::command::{self, Action};
use stacksaw_ui::layout::ColumnKind;
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
fn diff_pane_is_full_width_below_the_columns() {
    let app = App::new(fixture_snapshot());
    let lines = render_to_lines(&app, 220, 60);
    let stacks_row = lines.iter().position(|l| l.contains("Stacks")).expect("Stacks");
    let commits_row = lines.iter().position(|l| l.contains("Commits")).expect("Commits");
    let diff_row = lines.iter().position(|l| l.contains("Diff")).expect("Diff");
    // Stacks/Commits share the top band; Diff sits on a lower row.
    assert_eq!(stacks_row, commits_row, "master columns share the top band");
    assert!(diff_row > stacks_row, "Diff pane is below the columns");
    // Its top border spans (essentially) the whole terminal width.
    assert!(
        lines[diff_row].chars().count() >= 200,
        "Diff pane should be full width, got {}",
        lines[diff_row].chars().count()
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
fn lookup_resolves_keys_to_actions() {
    use crossterm::event::{KeyCode, KeyEvent};
    let ev = |code| KeyEvent::from(code);
    assert_eq!(
        command::lookup(&ev(KeyCode::Char('j')), ColumnKind::Commits),
        Some(Action::MoveDown)
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
