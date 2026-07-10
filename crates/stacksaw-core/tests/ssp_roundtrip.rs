use stacksaw_core::config::Config;
use stacksaw_core::service::Service;
use stacksaw_git::executor::GitExecutor;
use std::fs;
use std::path::Path;

fn git(dir: &Path, args: &[&str]) {
    let ok = GitExecutor::new(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .success()
        .unwrap();
    assert!(ok, "git {args:?}");
}

#[tokio::test]
async fn test_snapshot_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_path = tmp.path();
    git(repo_path, &["init", "-q", "-b", "main"]);

    fs::write(repo_path.join("file.txt"), "content\n").unwrap();
    git(repo_path, &["add", "."]);
    git(repo_path, &["commit", "-qm", "msg"]);

    let config = Config::default();
    let service = Service::new(repo_path.to_path_buf(), repo_path.join(".git"), config);
    let ssp = service.snapshot().await.unwrap();

    assert!(!ssp.staircases.is_empty());
}
