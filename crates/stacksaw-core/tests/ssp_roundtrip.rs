use stacksaw_core::config::Config;
use stacksaw_core::service::Service;
use stacksaw_core::Core;
use stacksaw_git::executor::GitExecutor;
use stacksaw_ssp::method::ClientKind;
use stacksaw_ssp::types::MutatePlan;
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

fn fixture_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo_path = tmp.path().to_path_buf();
    git(&repo_path, &["init", "-q", "-b", "main"]);
    fs::write(repo_path.join("file.txt"), "content\n").unwrap();
    git(&repo_path, &["add", "."]);
    git(&repo_path, &["commit", "-qm", "msg"]);
    (tmp, repo_path)
}

#[tokio::test]
async fn test_snapshot_roundtrip() {
    let (_tmp, repo_path) = fixture_repo();
    let config = Config::default();
    let service = Service::new(repo_path.clone(), repo_path.join(".git"), config);
    let ssp = service.snapshot().await.unwrap();
    assert!(!ssp.staircases.is_empty());
}

#[tokio::test]
async fn core_in_process_snapshot() {
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
    let snap = core.snapshot().await.unwrap();
    assert_eq!(snap.generation, 1);
}

#[tokio::test]
async fn commit_get_and_show() {
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
    let show = core.commit_show("HEAD").await.unwrap();
    let detail = core.commit_detail(&show.oid).await.unwrap();
    assert_eq!(detail.oid, show.oid);
    assert!(!detail.files.is_empty());
}

#[tokio::test]
async fn diff_range_and_notes() {
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
    let diff = core.diff_range(&["show", "--stat", "HEAD"]).await.unwrap();
    assert!(!diff.is_empty());
    let note = core.note_add("file.txt", 1, "looks good").await.unwrap();
    let notes = core.note_list().await.unwrap();
    assert!(notes.iter().any(|n| n.id == note.id));
}

#[tokio::test]
async fn mutate_stale_generation_rejected() {
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
    let plan = MutatePlan::Reshape {
        target_oid: "HEAD".into(),
        op: "indent".into(),
    };
    assert!(core.mutate(plan, Some(0)).await.is_err());
}
