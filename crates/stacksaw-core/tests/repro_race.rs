use futures::stream::{FuturesUnordered, StreamExt};
use stacksaw_core::config::Config;
use stacksaw_core::Core;
use stacksaw_git::executor::GitExecutor;
use stacksaw_ssp::method::ClientKind;
use stacksaw_ssp::types::MutatePlan;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn git(dir: &Path, args: &[&str]) {
    let ok = GitExecutor::new(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .success()
        .unwrap();
    assert!(ok, "git {:?}", args);
}

fn fixture_repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo_path = tmp.path().to_path_buf();
    git(&repo_path, &["init", "-q", "-b", "main"]);
    fs::write(repo_path.join("file.txt"), "content\n").unwrap();
    git(&repo_path, &["add", "."]);
    git(&repo_path, &["commit", "-qm", "initial"]);

    // Create a stack: main -> feature-1 -> feature
    git(&repo_path, &["checkout", "-qb", "feature-1"]);
    fs::write(repo_path.join("f1.txt"), "f1\n").unwrap();
    git(&repo_path, &["add", "."]);
    git(&repo_path, &["commit", "-qm", "f1"]);

    git(&repo_path, &["checkout", "-qb", "feature"]);
    fs::write(repo_path.join("f2.txt"), "f2\n").unwrap();
    git(&repo_path, &["add", "."]);
    git(&repo_path, &["commit", "-qm", "f2"]);

    (tmp, repo_path)
}

#[tokio::test]
async fn test_mutate_race() {
    let (_tmp, repo_path) = fixture_repo();
    let config = Config::default();
    let core = Core::attach_or_local(
        repo_path.clone(),
        repo_path.join(".git"),
        config,
        ClientKind::Cli,
    )
    .await
    .unwrap();

    let core = Arc::new(core);

    let snap = core.snapshot().await.unwrap();
    let target_oid = snap.staircases[0].segments[0].commits[0].oid.clone();

    let mut futs = FuturesUnordered::new();
    for _ in 0..10 {
        let core = core.clone();
        let target_oid = target_oid.clone();
        futs.push(async move {
            let plan = MutatePlan::Reshape {
                target_oid,
                op: "indent".into(),
            };
            core.mutate(plan, None).await
        });
    }

    let mut results = Vec::new();
    while let Some(res) = futs.next().await {
        results.push(res);
    }

    let errors: Vec<_> = results.iter().filter(|r| r.is_err()).collect();
    assert!(
        !errors.is_empty(),
        "Expected some errors due to concurrent mutations!"
    );
}
