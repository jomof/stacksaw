use stacksaw_git::executor::GitExecutor;
use stacksaw_git::refs::{restore_checkpoint, write_checkpoint};
use stacksaw_ssp::git_ref::GitRef;
use tempfile::tempdir;

#[test]
fn test_restore_checkpoint_corrupts_tags() {
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

    // Create a tag
    GitExecutor::new(repo_dir)
        .args(["tag", "v1.0"])
        .status()
        .unwrap();

    // Write checkpoint for the tag
    let cp = write_checkpoint(repo_dir, &[GitRef::new("refs/tags/v1.0")]).unwrap();

    // Delete the tag so we can see it restored
    GitExecutor::new(repo_dir)
        .args(["tag", "-d", "v1.0"])
        .status()
        .unwrap();

    // Restore checkpoint
    restore_checkpoint(repo_dir, &cp.id).unwrap();

    // Check where v1.0 is now
    let out = GitExecutor::new(repo_dir)
        .args(["rev-parse", "--symbolic-full-name", "v1.0"])
        .run_captured()
        .unwrap();

    // BUG: It should be refs/tags/v1.0, but it will be refs/heads/v1.0
    assert_eq!(out, "refs/tags/v1.0", "Tag was restored as a branch!");
}
