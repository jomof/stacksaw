//! End-to-end test of the staircase model against a real fixture repo.

use std::path::Path;
use std::process::Command;

use stacksaw_git::model::ModelOptions;
use stacksaw_git::{build_snapshot, build_staircases, changed_files, file_content, file_diff, Repo};
use stacksaw_ssp::types::WORKTREE_OID;

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

    assert_eq!(stair.name, "feat", "named after the branches' common prefix");
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
fn changed_files_lists_commit_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base\n", "Initial commit");
    // A non-root commit that adds and modifies files.
    std::fs::write(dir.join("base.txt"), "base changed\n").unwrap();
    std::fs::write(dir.join("added.txt"), "new\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "Change base, add file"]);

    // Root commit lists its file as added.
    let root = changed_files(dir, "HEAD^").unwrap();
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].status, "A");
    assert_eq!(root[0].path, "base.txt");

    // HEAD shows the modify + add.
    let head = changed_files(dir, "HEAD").unwrap();
    let mut pairs: Vec<(String, String)> =
        head.into_iter().map(|f| (f.status, f.path)).collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("A".to_string(), "added.txt".to_string()),
            ("M".to_string(), "base.txt".to_string()),
        ]
    );
}

#[test]
fn file_diff_shows_single_file_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "keep.txt", "keep\n", "Initial commit");
    std::fs::write(dir.join("keep.txt"), "keep\nmore\n").unwrap();
    std::fs::write(dir.join("other.txt"), "other\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "Edit keep, add other"]);

    let diff = file_diff(dir, "HEAD", "keep.txt").unwrap();
    assert!(diff.contains("keep.txt"), "diff should mention the path");
    assert!(diff.contains("+more"), "diff should show the added line");
    // Scoped to the pathspec: other.txt must not appear.
    assert!(!diff.contains("other.txt"), "diff is scoped to keep.txt");
}

#[test]
fn file_content_returns_full_text_at_rev() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "hello.txt", "line one\nline two\n", "Initial commit");

    let content = file_content(dir, "HEAD", "hello.txt").unwrap();
    assert_eq!(content, "line one\nline two\n");
}

#[test]
fn dirty_worktree_appears_as_a_virtual_tip_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "keep.txt", "one\ntwo\n", "Initial commit");

    // Clean tree: no virtual commit.
    let repo = Repo::discover(dir).unwrap();
    let opts = ModelOptions {
        default_upstream: None,
    };
    let clean = build_snapshot(&repo, 0, &opts).unwrap();
    assert!(
        !has_worktree_commit(&clean),
        "clean tree has no virtual commit"
    );

    // Dirty the tree: modify a tracked file and add an untracked one.
    std::fs::write(dir.join("keep.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(dir.join("new.txt"), "brand new\n").unwrap();

    let repo = Repo::discover(dir).unwrap();
    let snap = build_snapshot(&repo, 1, &opts).unwrap();

    // The staircase containing main is dirty and ends in the virtual commit.
    let stair = snap
        .staircases
        .iter()
        .find(|s| s.segments.iter().any(|seg| seg.branch == "main"))
        .expect("staircase for main");
    assert!(stair.dirty, "staircase flagged dirty");
    let wip = stair
        .segments
        .iter()
        .flat_map(|seg| seg.commits.iter())
        .find(|c| c.oid == WORKTREE_OID)
        .expect("virtual worktree commit present");
    assert_eq!(wip.subject, "Uncommitted changes");
    assert_eq!(wip.added, 1, "one added line vs HEAD (tracked)");
    assert_eq!(wip.deleted, 0);

    // Its files include the modified tracked file and the untracked addition.
    let files = changed_files(dir, WORKTREE_OID).unwrap();
    let mut pairs: Vec<(String, String)> =
        files.into_iter().map(|f| (f.status, f.path)).collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("A".to_string(), "new.txt".to_string()),
            ("M".to_string(), "keep.txt".to_string()),
        ]
    );

    // Diff of the tracked file is vs HEAD; untracked content is read from disk.
    let diff = file_diff(dir, WORKTREE_OID, "keep.txt").unwrap();
    assert!(diff.contains("+three"), "worktree diff shows the new line");
    let content = file_content(dir, WORKTREE_OID, "new.txt").unwrap();
    assert_eq!(content, "brand new\n");
}

#[test]
fn detached_head_shows_a_staircase_with_its_uncommitted_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "keep.txt", "one\ntwo\n", "Initial commit");
    commit(dir, "more.txt", "aaa\n", "Second commit");

    // Detach HEAD at the tip, then dirty the worktree.
    git(dir, &["checkout", "-q", "--detach"]);
    std::fs::write(dir.join("keep.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(dir.join("new.txt"), "brand new\n").unwrap();

    let repo = Repo::discover(dir).unwrap();
    let opts = ModelOptions {
        default_upstream: None,
    };
    let snap = build_snapshot(&repo, 0, &opts).unwrap();

    // A detached HEAD still surfaces as a staircase (keyed by its short oid),
    // rather than leaving Stacks empty.
    assert!(snap.detached, "snapshot flags detached HEAD");
    let head_oid = snap.head.clone().expect("HEAD resolves to a commit");
    let short = &head_oid[..7];
    let stair = snap
        .staircases
        .iter()
        .find(|s| s.name == short)
        .expect("a staircase for the detached HEAD");

    // Its uncommitted changes appear as the virtual worktree commit — and that
    // is the *only* row: HEAD is its own upstream, so no history is listed.
    assert!(stair.dirty, "detached staircase flagged dirty");
    assert_eq!(stair.ahead, 0, "detached HEAD has nothing ahead of itself");
    let commits: Vec<&stacksaw_ssp::types::CommitSummary> =
        stair.segments.iter().flat_map(|seg| seg.commits.iter()).collect();
    assert_eq!(commits.len(), 1, "only the worktree row shows, no history");
    let wip = commits
        .iter()
        .find(|c| c.oid == WORKTREE_OID)
        .expect("virtual worktree commit on the detached HEAD");
    assert_eq!(wip.subject, "Uncommitted changes");

    // And those changes are browsable via the worktree sentinel.
    let files = changed_files(dir, WORKTREE_OID).unwrap();
    let mut pairs: Vec<(String, String)> =
        files.into_iter().map(|f| (f.status, f.path)).collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("A".to_string(), "new.txt".to_string()),
            ("M".to_string(), "keep.txt".to_string()),
        ]
    );
}

fn has_worktree_commit(snap: &stacksaw_ssp::types::Snapshot) -> bool {
    snap.staircases
        .iter()
        .flat_map(|s| s.segments.iter())
        .flat_map(|seg| seg.commits.iter())
        .any(|c| c.oid == WORKTREE_OID)
}

#[test]
fn detects_forked_segment_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base\n", "Initial commit");

    // The forked branches share a `feat/` prefix so they form one staircase (a
    // segment tree) rather than a prefix-less "bunch of branches" (§2).
    git(dir, &["checkout", "-q", "-b", "feat/trunk"]);
    commit(dir, "t.txt", "t\n", "Add trunk");

    // Two branches fork off feat/trunk.
    git(dir, &["checkout", "-q", "-b", "feat/left"]);
    commit(dir, "l.txt", "l\n", "Add left");
    git(dir, &["checkout", "-q", "feat/trunk"]);
    git(dir, &["checkout", "-q", "-b", "feat/right"]);
    commit(dir, "r.txt", "r\n", "Add right");

    let repo = Repo::discover(dir).unwrap();
    let opts = ModelOptions {
        default_upstream: Some("refs/heads/main".to_string()),
    };
    let staircases = build_staircases(&repo, &opts).unwrap();

    let tree = staircases
        .iter()
        .find(|s| s.segments.iter().any(|seg| seg.branch == "feat/trunk"))
        .expect("staircase containing feat/trunk");

    // trunk root, then left and right both children of trunk's segment.
    let trunk_idx = tree
        .segments
        .iter()
        .position(|s| s.branch == "feat/trunk")
        .unwrap();
    let children: Vec<&str> = tree
        .segments
        .iter()
        .filter(|s| s.parent == Some(trunk_idx))
        .map(|s| s.branch.as_str())
        .collect();
    assert!(children.contains(&"feat/left"));
    assert!(children.contains(&"feat/right"));
}

#[test]
fn branches_without_a_common_prefix_are_not_a_staircase() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base\n", "Initial commit");

    // `bob` is stacked on `alice`, both tracking main, but their names share no
    // common prefix: a "bunch of branches", not a staircase (§2). Each must
    // surface on its own single-branch stack rather than being fused into one.
    git(dir, &["checkout", "-q", "-b", "alice"]);
    commit(dir, "a.txt", "a\n", "Add a");
    git(dir, &["checkout", "-q", "-b", "bob"]);
    commit(dir, "b.txt", "b\n", "Add b");

    let repo = Repo::discover(dir).unwrap();
    let opts = ModelOptions {
        default_upstream: Some("refs/heads/main".to_string()),
    };
    let staircases = build_staircases(&repo, &opts).unwrap();

    let alice = staircases
        .iter()
        .find(|s| s.name == "alice")
        .expect("a standalone alice stack");
    let bob = staircases
        .iter()
        .find(|s| s.name == "bob")
        .expect("a standalone bob stack");
    assert_eq!(alice.segments.len(), 1, "alice stays a lone branch");
    assert_eq!(bob.segments.len(), 1, "bob stays a lone branch");
}

#[test]
fn local_branch_upstream_fallback_works() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base\n", "Initial commit");

    // Create a branch `dry-feature` with no tracking configuration.
    git(dir, &["checkout", "-q", "-b", "dry-feature"]);
    commit(dir, "feat.txt", "feat\n", "Add feature");

    // Checkout main, so dry-feature is NOT the checked-out branch.
    git(dir, &["checkout", "-q", "main"]);

    let repo = Repo::discover(dir).unwrap();
    // Use default remote upstream which DOES NOT exist in this repo (e.g. refs/remotes/origin/main).
    let opts = ModelOptions {
        default_upstream: Some("refs/remotes/origin/main".to_string()),
    };
    let staircases = build_staircases(&repo, &opts).unwrap();

    // dry-feature should fallback to refs/heads/main and be shown.
    let dry_stair = staircases
        .iter()
        .find(|s| s.name == "dry-feature")
        .expect("should fall back to local main and show dry-feature stack");
    assert_eq!(dry_stair.upstream, "refs/heads/main");
    assert_eq!(dry_stair.ahead, 1);
}

#[test]
fn repox_dry_branches_scenario() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "root.txt", "root\n", "Initial root");

    git(dir, &["checkout", "-q", "-b", "dry-factor-code-review-prompts-into"]);
    commit(dir, "prompts.txt", "prompts\n", "Factor code review prompts");

    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["merge", "-q", "dry-factor-code-review-prompts-into"]);

    commit(dir, "main_1.txt", "main 1\n", "Main commit 1");
    commit(dir, "main_2.txt", "main 2\n", "Main commit 2");

    git(dir, &["checkout", "-q", "-b", "dry-consolidate-stack-logic"]);
    commit(dir, "stack.txt", "stack\n", "Consolidate stack logic");

    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["checkout", "-q", "-b", "dry-consolidate-git-helpers"]);
    commit(dir, "git_1.txt", "git 1\n", "Consolidate git helpers 1");
    commit(dir, "git_2.txt", "git 2\n", "Consolidate git helpers 2");

    let repo = Repo::discover(dir).unwrap();
    let opts = ModelOptions {
        default_upstream: Some("refs/heads/main".to_string()),
    };
    let staircases = build_staircases(&repo, &opts).unwrap();

    assert_eq!(staircases.len(), 2);

    let helper_stair = staircases
        .iter()
        .find(|s| s.name == "dry-consolidate-git-helpers")
        .expect("should find dry-consolidate-git-helpers staircase");
    assert_eq!(helper_stair.upstream, "refs/heads/main");
    assert_eq!(helper_stair.segments.len(), 1);
    assert_eq!(helper_stair.segments[0].branch, "dry-consolidate-git-helpers");
    assert_eq!(helper_stair.segments[0].commits.len(), 2); // git_1, git_2 (ahead of main)
    assert_eq!(helper_stair.ahead, 2);
    assert_eq!(helper_stair.behind, 0);

    let stack_stair = staircases
        .iter()
        .find(|s| s.name == "dry-consolidate-stack-logic")
        .expect("should find dry-consolidate-stack-logic staircase");
    assert_eq!(stack_stair.upstream, "refs/heads/main");
    assert_eq!(stack_stair.segments.len(), 1);
    assert_eq!(stack_stair.segments[0].branch, "dry-consolidate-stack-logic");
    assert_eq!(stack_stair.segments[0].commits.len(), 1); // stack (ahead of main)
    assert_eq!(stack_stair.ahead, 1);
    assert_eq!(stack_stair.behind, 0);
}

#[test]
fn playground_staircase_scenario() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "README.md", "README\n", "Initial commit");

    git(dir, &["checkout", "-q", "-b", "step-1"]);
    commit(dir, "f1.txt", "f1\n", "Add f1");
    commit(dir, "f2.txt", "f2\n", "Add f2");

    git(dir, &["checkout", "-q", "-b", "step-2"]);
    commit(dir, "f3.txt", "f3\n", "Add f3");
    commit(dir, "f4.txt", "f4\n", "Add f4");

    git(dir, &["checkout", "-q", "-b", "step-3"]);
    commit(dir, "f5.txt", "f5\n", "Add f5");
    commit(dir, "f6.txt", "f6\n", "Add f6");

    let repo = Repo::discover(dir).unwrap();
    let opts = ModelOptions {
        default_upstream: Some("refs/heads/main".to_string()),
    };
    let staircases = build_staircases(&repo, &opts).unwrap();

    let stair = staircases
        .iter()
        .find(|s| s.name == "step")
        .expect("should find step staircase");

    assert_eq!(stair.upstream, "refs/heads/main");
    assert_eq!(stair.segments.len(), 3);

    let branches: Vec<&str> = stair.segments.iter().map(|s| s.branch.as_str()).collect();
    assert_eq!(branches, vec!["step-1", "step-2", "step-3"]);

    assert_eq!(stair.segments[0].parent, None);
    assert_eq!(stair.segments[1].parent, Some(0));
    assert_eq!(stair.segments[2].parent, Some(1));

    assert_eq!(stair.segments[0].commits.len(), 2);
    assert_eq!(stair.segments[1].commits.len(), 2);
    assert_eq!(stair.segments[2].commits.len(), 2);

    assert_eq!(stair.ahead, 6);
    assert_eq!(stair.behind, 0);
}


