use stacksaw_git::edit;
use stacksaw_git::executor::GitExecutor;
use stacksaw_git::Repo;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_edit_finish_leak_on_failure() {
    let tmp = tempdir().unwrap();
    let repo_dir = tmp.path();

    // ARRANGE: Create a repo with a commit and a branch
    GitExecutor::new(repo_dir)
        .args(["init", "-q", "-b", "main"])
        .status()
        .unwrap();

    let commit = |msg: &str, content: &str| {
        fs::write(repo_dir.join("file.txt"), content).unwrap();
        GitExecutor::new(repo_dir)
            .args(["add", "file.txt"])
            .status()
            .unwrap();
        GitExecutor::new(repo_dir)
            .args(["commit", "-m", msg])
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap();
    };

    commit("initial", "line 1\n");
    let oid_a = GitExecutor::new(repo_dir)
        .args(["rev-parse", "HEAD"])
        .run_captured()
        .unwrap();

    commit("second", "line 1\nline 2\n");
    GitExecutor::new(repo_dir)
        .args(["branch", "feat-a", "main"])
        .status()
        .unwrap();

    let repo = Repo::open(repo_dir).unwrap();

    // Start edit session on initial commit
    let begin = edit::begin(&repo, &oid_a).unwrap();
    let token = begin.session.token.clone();
    let worktree_path = begin.session.worktree.clone();

    // ACT: Induce conflict by changing line 1 in the scratch worktree
    fs::write(worktree_path.join("file.txt"), "line 1 conflicted\n").unwrap();

    // Finish should fail during rebase of "second" onto amended "initial"
    let result = edit::finish(&repo, &token, None);

    // ASSERT: finish should fail
    assert!(result.is_err(), "finish should have failed due to conflict");

    // ASSERT: the worktree and session file should have been cleaned up, but they are LEAKED
    assert!(
        worktree_path.exists(),
        "BUG: Worktree should still exist (leak) but was cleaned up"
    );

    let session_file = repo_dir
        .join(".git/stacksaw/edit")
        .join(format!("{}.json", token));
    assert!(
        session_file.exists(),
        "Session file should still exist (leak)"
    );
}
