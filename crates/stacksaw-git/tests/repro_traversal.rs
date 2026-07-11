use stacksaw_git::executor::GitExecutor;
use stacksaw_git::snapshot::file_content;
use std::fs;
use std::path::Path;

fn git(dir: &Path, args: &[&str]) {
    let out = GitExecutor::new(dir)
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
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_worktree_path_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("repo");
    fs::create_dir(&repo_dir).unwrap();
    git(&repo_dir, &["init", "-q", "-b", "main"]);

    let secret_file = tmp.path().join("secret.txt");
    fs::write(&secret_file, "top secret content").unwrap();

    // Attempt traversal: read a file outside the repository worktree
    let content = file_content(&repo_dir, "working-tree", "../secret.txt").unwrap();
    assert_eq!(
        content, "top secret content",
        "Should have been able to traverse out of the worktree!"
    );
}
