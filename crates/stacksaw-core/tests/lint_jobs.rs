use stacksaw_core::service::build_lint_jobs;
use stacksaw_git::Repo;
use stacksaw_lint::Profile;
use std::fs;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn test_build_lint_jobs_content() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_path = tmp.path();
    git(repo_path, &["init", "-q", "-b", "main"]);

    fs::write(repo_path.join("file1.txt"), "content1\n").unwrap();
    fs::write(repo_path.join("file2.txt"), "content2\n").unwrap();
    git(repo_path, &["add", "."]);
    git(repo_path, &["commit", "-qm", "initial commit"]);

    let repo = Repo::open(repo_path).unwrap();
    let head = repo.head_oid().unwrap().unwrap().to_string();

    let jobs = build_lint_jobs(&repo, repo_path, &[head], Profile::default()).unwrap();

    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    assert_eq!(job.files.len(), 2);

    let f1 = job
        .files
        .iter()
        .find(|f| f.path == "file1.txt")
        .expect("file1.txt not found");
    assert_eq!(f1.content.as_deref(), Some("content1\n"));

    let f2 = job
        .files
        .iter()
        .find(|f| f.path == "file2.txt")
        .expect("file2.txt not found");
    assert_eq!(f2.content.as_deref(), Some("content2\n"));
}
