use stacksaw_git::edit::begin;
use stacksaw_git::executor::GitExecutor;
use stacksaw_git::refs::remove_worktree;
use stacksaw_git::Repo;
use std::fs;
use std::path::Path;

fn git(dir: &Path, args: &[&str]) {
    GitExecutor::new(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .unwrap();
}

fn commit(dir: &Path, file: &str, msg: &str) {
    fs::write(dir.join(file), format!("{file}\n")).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", msg]);
}

#[test]
fn test_begin_edit_creates_scratch_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base");

    let c1_oid = GitExecutor::new(dir)
        .args(["rev-parse", "HEAD"])
        .run_captured()
        .unwrap()
        .trim()
        .to_string();

    let repo = Repo::open(dir).unwrap();
    let res = begin(&repo, &c1_oid).unwrap();

    assert!(res.session.worktree.exists());
    assert!(res.session.worktree.join(".git").exists());
    assert!(res.session.worktree.join("base.txt").exists());

    // Cleanup
    let _ = remove_worktree(dir, &res.session.worktree);
}
