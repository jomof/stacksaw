use stacksaw_core::config::Config;
use stacksaw_core::Core;
use stacksaw_ssp::method::ClientKind;

#[tokio::test]
async fn test_core_is_send() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_path = tmp.path().to_path_buf();
    // Just enough to init
    std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(["init", "-q"])
        .status()
        .unwrap();

    let config = Config::default();
    let core = Core::attach_or_local(
        repo_path.clone(),
        repo_path.join(".git"),
        config,
        ClientKind::Cli,
    )
    .await
    .unwrap();

    let core = std::sync::Arc::new(core);

    // This will fail to compile if the future returned by snapshot() is not Send.
    tokio::spawn(async move {
        let _ = core.snapshot().await;
    })
    .await
    .unwrap();
}
