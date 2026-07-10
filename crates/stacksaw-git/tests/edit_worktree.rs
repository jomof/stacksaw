use stacksaw_git::edit::{begin, finish};
use stacksaw_git::repo::Repo;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_finish_needs_worktree() {
    let tmp = tempdir().unwrap();
    let repo_dir = tmp.path();

    // Init repo
    Command::new("git")
        .arg("init")
        .arg("-q")
        .arg("-b")
        .arg("main")
        .arg(repo_dir)
        .status()
        .unwrap();

    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap();
        assert!(status.success());
    };

    git(&["commit", "--allow-empty", "-m", "initial"]);
    let c1_oid = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let c1_oid = String::from_utf8(c1_oid.stdout).unwrap().trim().to_string();

    // C2: ahead of C1
    git(&["commit", "--allow-empty", "-m", "commit 2"]);
    // feat-a at C2
    git(&["branch", "feat-a"]);

    let repo = Repo::open(repo_dir).unwrap();

    // Begin edit on C1
    let res = begin(&repo, &c1_oid).unwrap();
    let token = res.session.token;

    // Finish edit. This should trigger rebase and fail if run in .git
    let finish_res = finish(&repo, &token, Some("amended"));
    assert!(finish_res.is_ok(), "finish failed: {:?}", finish_res.err());
}
