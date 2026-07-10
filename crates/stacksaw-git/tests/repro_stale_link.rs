use stacksaw_git::executor::GitExecutor;
use stacksaw_git::model::{build_staircases, ModelOptions};
use stacksaw_git::repo::Repo;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_stale_link_recovery_when_child_is_exactly_at_former_tip() {
    let tmp = tempdir().unwrap();
    let repo_dir = tmp.path();
    let git = |args: &[&str]| {
        GitExecutor::new(repo_dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap();
    };

    git(&["init", "-q", "-b", "main"]);
    fs::write(repo_dir.join("file"), "1").unwrap();
    git(&["add", "file"]);
    git(&["commit", "-m", "c1"]);

    git(&["checkout", "-b", "feat/a"]);
    fs::write(repo_dir.join("file"), "2").unwrap();
    git(&["add", "file"]);
    git(&["commit", "-m", "c2"]);

    git(&["checkout", "-b", "feat/b"]); // feat/b is now at c2

    git(&["checkout", "feat/a"]);
    fs::write(repo_dir.join("file"), "2-amended").unwrap();
    git(&["add", "file"]);
    git(&["commit", "--amend", "-m", "c2-amended"]);

    let repo = Repo::open(repo_dir).unwrap();
    let opts = ModelOptions::default();
    let staircases = build_staircases(&repo, &opts).unwrap();

    let feat_b_seg = staircases
        .iter()
        .flat_map(|s| &s.segments)
        .find(|seg| seg.branch.leaf() == "feat/b")
        .expect("Should find feat/b");

    assert!(
        feat_b_seg.stale,
        "feat/b should be marked stale even if it is exactly at the former tip"
    );
}
