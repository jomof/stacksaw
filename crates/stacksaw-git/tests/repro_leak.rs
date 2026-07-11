use stacksaw_git::executor::GitExecutor;
use stacksaw_git::Repo;
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

fn commit(dir: &Path, file: &str, msg: &str) {
    fs::write(dir.join(file), format!("{file}\n")).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", msg]);
}

#[test]
fn test_patch_ids_zombie_leak() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base");
    let oid = GitExecutor::new(dir)
        .args(["rev-parse", "HEAD"])
        .run_captured()
        .unwrap();

    let repo = Repo::discover(dir).unwrap();
    // Trigger the leak multiple times
    for _ in 0..5 {
        repo.patch_ids(&[oid.clone()]).unwrap();
    }

    // Check for zombie children of the current process (state 'Z').
    let output = std::process::Command::new("ps")
        .args(["-o", "state", "--ppid", &std::process::id().to_string()])
        .output()
        .expect("run ps");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let zombie_count = stdout.lines().filter(|l| l.trim() == "Z").count();
    assert!(
        zombie_count > 0,
        "Expected zombie processes, found {}",
        zombie_count
    );
}
