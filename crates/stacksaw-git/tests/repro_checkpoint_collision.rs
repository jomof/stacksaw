use stacksaw_git::executor::GitExecutor;
use stacksaw_git::refs::write_checkpoint;
use stacksaw_ssp::git_ref::GitRef;
use tempfile::tempdir;

#[test]
fn test_write_checkpoint_collisions() {
    let tmp = tempdir().unwrap();
    let repo_dir = tmp.path();
    GitExecutor::new(repo_dir)
        .args(["init", "-q", "-b", "main"])
        .status()
        .unwrap();

    // Helper to commit
    let commit = |msg: &str| {
        GitExecutor::new(repo_dir)
            .args(["commit", "--allow-empty", "-m", msg])
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap();
    };

    commit("initial");

    // Create a branch and a tag with the same name
    GitExecutor::new(repo_dir)
        .args(["branch", "collision"])
        .status()
        .unwrap();
    GitExecutor::new(repo_dir)
        .args(["tag", "collision"])
        .status()
        .unwrap();

    // Attempt to write a checkpoint for both
    let res = write_checkpoint(
        repo_dir,
        &[
            GitRef::new("refs/heads/collision"),
            GitRef::new("refs/tags/collision"),
        ],
    );

    // EXPECTATION: Should succeed. BUG: Fails due to name collision in checkpoints.
    assert!(
        res.is_ok(),
        "Checkpoint failed due to leaf-name collision: {:?}",
        res.err()
    );
}
