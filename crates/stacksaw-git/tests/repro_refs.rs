#![allow(unused_imports)]
use tempfile::tempdir;
use stacksaw_git::refs::{write_checkpoint, restore_checkpoint};
use stacksaw_ssp::git_ref::GitRef;
use stacksaw_git::executor::GitExecutor;

#[test]
fn test_restore_checkpoint_heads_corruption() {
    let tmp = tempdir().unwrap();
    let repo_dir = tmp.path();
    GitExecutor::new(repo_dir).args(["init", "-q", "-b", "main"]).status().unwrap();
    
    // Initial commit
    GitExecutor::new(repo_dir)
        .args(["commit", "--allow-empty", "-m", "initial"])
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .unwrap();

    // Checkpoint HEAD
    let cp = write_checkpoint(repo_dir, &[GitRef::new("HEAD")]).unwrap();
    
    // Restore
    restore_checkpoint(repo_dir, &cp.id).unwrap();
    
    // Check if refs/heads/HEAD exists
    let out = GitExecutor::new(repo_dir).args(["rev-parse", "--verify", "refs/heads/HEAD"]).run_captured();
    if out.is_ok() {
        panic!("BUG REPRODUCED: refs/heads/HEAD was created!");
    }
}
