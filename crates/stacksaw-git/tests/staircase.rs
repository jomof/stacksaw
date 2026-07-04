//! End-to-end test of the staircase model against a real fixture repo.

use std::path::Path;
use std::process::Command;

use stacksaw_git::model::ModelOptions;
use stacksaw_git::{build_staircases, Repo};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "2026-07-01T12:00:00")
        .env("GIT_COMMITTER_DATE", "2026-07-01T12:00:00")
        .output()
        .expect("run git");
    assert!(
        status.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&status.stderr)
    );
}

fn commit(dir: &Path, file: &str, contents: &str, msg: &str) {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", msg]);
}

/// Build: main (base) → feat1 (1 commit) → feat2 (1 commit) → feat3 (1 commit).
fn build_fixture(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base\n", "Initial commit");

    git(dir, &["checkout", "-q", "-b", "feat1"]);
    commit(dir, "a.txt", "a\n", "Add a");

    git(dir, &["checkout", "-q", "-b", "feat2"]);
    commit(dir, "b.txt", "b\n", "Add b");

    git(dir, &["checkout", "-q", "-b", "feat3"]);
    commit(dir, "c.txt", "c\n", "Add c");
}

#[test]
fn builds_three_step_staircase() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    build_fixture(dir);

    let repo = Repo::discover(dir).unwrap();
    let opts = ModelOptions {
        default_upstream: Some("refs/heads/main".to_string()),
    };
    let staircases = build_staircases(&repo, &opts).unwrap();

    // main itself has no upstream distinct from itself → not a staircase root
    // with commits, but feat1/feat2/feat3 form one staircase off main.
    let stair = staircases
        .iter()
        .find(|s| s.segments.iter().any(|seg| seg.branch == "feat3"))
        .expect("staircase containing feat3");

    assert_eq!(stair.name, "feat3", "named after tip-most branch");
    assert_eq!(stair.upstream, "refs/heads/main");

    // Three segments (feat1, feat2, feat3), each one commit, in order.
    let branches: Vec<&str> = stair.segments.iter().map(|s| s.branch.as_str()).collect();
    assert_eq!(branches, vec!["feat1", "feat2", "feat3"]);
    for seg in &stair.segments {
        assert_eq!(seg.commits.len(), 1, "one commit per step");
    }

    // Parent links encode the ladder: feat2's parent is feat1's segment, etc.
    assert_eq!(stair.segments[0].parent, None);
    assert_eq!(stair.segments[1].parent, Some(0));
    assert_eq!(stair.segments[2].parent, Some(1));

    assert_eq!(stair.ahead, 3);
    assert_eq!(stair.behind, 0);
}

#[test]
fn detects_forked_segment_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base\n", "Initial commit");

    git(dir, &["checkout", "-q", "-b", "trunk"]);
    commit(dir, "t.txt", "t\n", "Add trunk");

    // Two branches fork off trunk.
    git(dir, &["checkout", "-q", "-b", "left"]);
    commit(dir, "l.txt", "l\n", "Add left");
    git(dir, &["checkout", "-q", "trunk"]);
    git(dir, &["checkout", "-q", "-b", "right"]);
    commit(dir, "r.txt", "r\n", "Add right");

    let repo = Repo::discover(dir).unwrap();
    let opts = ModelOptions {
        default_upstream: Some("refs/heads/main".to_string()),
    };
    let staircases = build_staircases(&repo, &opts).unwrap();

    let tree = staircases
        .iter()
        .find(|s| s.segments.iter().any(|seg| seg.branch == "trunk"))
        .expect("staircase containing trunk");

    // trunk root, then left and right both children of trunk's segment.
    let trunk_idx = tree
        .segments
        .iter()
        .position(|s| s.branch == "trunk")
        .unwrap();
    let children: Vec<&str> = tree
        .segments
        .iter()
        .filter(|s| s.parent == Some(trunk_idx))
        .map(|s| s.branch.as_str())
        .collect();
    assert!(children.contains(&"left"));
    assert!(children.contains(&"right"));
}
