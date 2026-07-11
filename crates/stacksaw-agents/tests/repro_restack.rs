use stacksaw_agents::restack::{RestackOutcome, Restacker};
use stacksaw_agents::workflow::{ConflictPolicy, FixPolicy, RestackParams};
use stacksaw_git::executor::GitExecutor;
use stacksaw_git::Repo;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn git(dir: &Path, args: &[&str]) {
    let out = GitExecutor::new(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .unwrap();
    assert!(out.success());
}

#[test]
fn test_restack_should_complete_with_dirty_worktree() {
    let tmp = tempdir().unwrap();
    let repo_dir = tmp.path();
    git(repo_dir, &["init", "-q", "-b", "main"]);

    // Create base commit
    fs::write(repo_dir.join("file.txt"), "line 1\n").unwrap();
    git(repo_dir, &["add", "file.txt"]);
    git(repo_dir, &["commit", "-m", "base"]);

    // Create feature branch
    git(repo_dir, &["checkout", "-b", "feat"]);
    fs::write(repo_dir.join("file.txt"), "line 1\nline 2\n").unwrap();
    git(repo_dir, &["add", "file.txt"]);
    git(repo_dir, &["commit", "-m", "feat"]);

    // Back to main and create a new base
    git(repo_dir, &["checkout", "main"]);
    fs::write(repo_dir.join("other.txt"), "other\n").unwrap();
    git(repo_dir, &["add", "other.txt"]);
    git(repo_dir, &["commit", "-m", "new base"]);

    // Now stay on feat and make it DIRTY (tracked change)
    git(repo_dir, &["checkout", "feat"]);
    fs::write(repo_dir.join("file.txt"), "line 1\nline 2\nDIRTY\n").unwrap();

    let repo = Repo::open(repo_dir).unwrap();
    let params = RestackParams {
        staircase: vec!["feat".to_string()],
        onto: "main".to_string(),
        fix_policy: FixPolicy::default(),
        conflict_policy: ConflictPolicy::Stop,
        max_attempts: 3,
    };

    let restacker = Restacker::new(&repo, params);

    // This SHOULD succeed if it used a scratch worktree (Protocol P4: never surprising).
    // It currently fails because it rebases in the main worktree.
    let result = restacker.run().expect("Restacker failed");

    assert!(matches!(result, RestackOutcome::Completed { .. }), "Restack should COMPLETE even if main worktree is dirty (by using a scratch worktree), but it PAUSED or failed: {:?}", result);
}
