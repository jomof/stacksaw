//! Golden-frame rendering tests via ratatui `TestBackend` (§14).

use stacksaw_ssp::types::{
    CommitSummary, FindingCounts, Segment, Snapshot, Staircase, SCHEMA_VERSION,
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
