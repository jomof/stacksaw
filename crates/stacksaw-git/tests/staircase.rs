//! End-to-end staircase model tests against real fixture repos.

use std::fs;
use std::path::Path;

use stacksaw_git::executor::GitExecutor;
use stacksaw_git::model::ModelOptions;
use stacksaw_git::Repo;
use stacksaw_ssp::types::WORKTREE_OID;

fn git(dir: &Path, args: &[&str]) {
    let status = GitExecutor::new(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn commit(dir: &Path, file: &str, contents: &str, msg: &str) {
    let path = dir.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn opts() -> ModelOptions {
    ModelOptions {
        default_upstream: Some("refs/heads/main".to_string()),
    }
}

/// A three-branch family: `feature-1` forks from `main`, `feature-2` from
/// `feature-1`, and `feature` from `feature-2`.
fn staircase_family(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base", "seed");
    git(dir, &["checkout", "-q", "-b", "feature-1"]);
    commit(dir, "c1.txt", "c1", "c1");
    git(dir, &["checkout", "-q", "-b", "feature-2"]);
    commit(dir, "c2.txt", "c2", "c2");
    git(dir, &["checkout", "-q", "-b", "feature"]);
    commit(dir, "c3.txt", "c3", "c3");
    for b in ["feature-1", "feature-2", "feature"] {
        git(dir, &["branch", "--set-upstream-to=main", b]);
    }
}

#[test]
fn build_snapshot_regroups_family_into_one_staircase() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    staircase_family(dir);

    let repo = Repo::discover(dir).unwrap();
    let snap = stacksaw_git::build_snapshot(&repo, 0, &opts()).unwrap();

    // The family regrouped into one staircase named after the tip.
    assert_eq!(snap.staircases.len(), 1);
    let s = &snap.staircases[0];
    assert_eq!(s.name, "feature");

    // It has three segments in the expected order.
    assert_eq!(s.segments.len(), 3);
    assert_eq!(s.segments[0].branch.short(), "feature-1");
    assert_eq!(s.segments[1].branch.short(), "feature-2");
    assert_eq!(s.segments[2].branch.short(), "feature");
}

#[test]
fn snapshot_detects_uncommitted_changes_as_a_virtual_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    staircase_family(dir);

    // Dirty the tree.
    fs::write(dir.join("dirty.txt"), "dirty\n").unwrap();

    let repo = Repo::discover(dir).unwrap();
    let snap = stacksaw_git::build_snapshot(&repo, 0, &opts()).unwrap();

    let s = &snap.staircases[0];
    assert_eq!(s.segments.len(), 3, "expected 3 real segments");

    let tip_seg = s.segments.last().unwrap();
    assert_eq!(tip_seg.branch.leaf(), "feature");

    // The tip segment should have its original commit plus the virtual worktree commit.
    assert_eq!(
        tip_seg.commits.len(),
        2,
        "expected 1 original + 1 virtual commit"
    );
    let virtual_commit = tip_seg.commits.last().unwrap();
    assert_eq!(virtual_commit.oid, WORKTREE_OID);
}

#[test]
fn fully_landed_implicit_branch_is_not_an_active_staircase() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base", "seed");

    git(dir, &["checkout", "-q", "-b", "spec-implement-review-identity"]);
    commit(dir, "c1.txt", "c1", "refactor: unify CLI presentation");
    commit(dir, "c2.txt", "c2", "fix(git): fix get_tree_id memoization");
    commit(dir, "c3.txt", "c3", "Implement Review identity kind");

    // Merge spec-implement-review-identity into main
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["merge", "-q", "--no-ff", "-m", "Merge spec-implement-review-identity", "spec-implement-review-identity"]);

    // Stay on spec-implement-review-identity
    git(dir, &["checkout", "-q", "spec-implement-review-identity"]);
    git(dir, &["branch", "--set-upstream-to=main", "spec-implement-review-identity"]);

    let repo = Repo::discover(dir).unwrap();
    let snap = stacksaw_git::build_snapshot(&repo, 0, &opts()).unwrap();

    assert!(snap
        .staircases
        .iter()
        .all(|s| s.name != "spec-implement-review-identity"));
}

#[test]
fn test_resolve_staircase_by_structural_key() {
    use stacksaw_git::model::{build_staircases, resolve_staircase_by_structural_key};
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    staircase_family(dir);

    let repo = Repo::discover(dir).unwrap();
    let staircases = build_staircases(&repo, &opts()).unwrap();
    assert!(!staircases.is_empty(), "expected staircases from discovery");

    let stair = &staircases[0];
    let structural_key = stair.id.as_ref().expect("staircase must carry a structural key");
    assert!(
        structural_key.starts_with("implicit@") || !structural_key.is_empty(),
        "expected structural key, got {structural_key}"
    );

    // Compute staircase contents for the commits window by structural key
    let resolved = resolve_staircase_by_structural_key(&repo, structural_key, &opts())
        .unwrap()
        .expect("should resolve staircase by structural key");

    assert_eq!(resolved.id, stair.id);
    assert_eq!(resolved.name, stair.name);
    assert_eq!(resolved.segments.len(), stair.segments.len());
}

#[test]
fn remote_integration_context_is_not_replaced_by_local_main() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base", "seed");

    // Simulate origin/main ref at initial seed commit
    git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);

    git(dir, &["checkout", "-q", "-b", "spec-implement-review-identity"]);
    commit(dir, "c1.txt", "c1", "refactor: unify CLI presentation");
    git(dir, &["config", "branch.spec-implement-review-identity.remote", "origin"]);
    git(dir, &["config", "branch.spec-implement-review-identity.merge", "refs/heads/main"]);

    // Merge spec-implement-review-identity into local main (origin/main stays at seed)
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["merge", "-q", "--no-ff", "-m", "Merge spec-implement-review-identity", "spec-implement-review-identity"]);

    // Stay checked out on spec-implement-review-identity
    git(dir, &["checkout", "-q", "spec-implement-review-identity"]);

    let repo = Repo::discover(dir).unwrap();
    let snap = stacksaw_git::build_snapshot(&repo, 0, &ModelOptions {
        default_upstream: Some("refs/remotes/origin/main".to_string()),
    }).unwrap();

    let s = snap
        .staircases
        .iter()
        .find(|s| s.segments.iter().any(|seg| seg.branch.leaf() == "spec-implement-review-identity"))
        .expect("branch staircase present");

    let seg = s.segments.iter().find(|seg| seg.branch.leaf() == "spec-implement-review-identity").unwrap();
    assert_eq!(s.integration.target, "refs/remotes/origin/main");
    assert_eq!(
        seg.commits.len(),
        1,
        "commits are decomposed against the remote integration anchor, not local main"
    );
}
