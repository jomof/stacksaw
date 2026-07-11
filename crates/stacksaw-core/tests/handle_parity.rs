use stacksaw_core::core::Core;
use stacksaw_core::service::Service;
use stacksaw_core::Config;
use stacksaw_git::executor::GitExecutor;
use stacksaw_ssp::method::ClientKind;
use tempfile::tempdir;

#[tokio::test]
async fn test_local_remote_parity() {
    let tmp = tempdir().unwrap();
    let repo_root = tmp.path().to_path_buf();
    let git_dir = repo_root.join(".git");

    // Init repo
    GitExecutor::new(&repo_root)
        .args(["init", "-q", "-b", "main"])
        .status()
        .unwrap();

    // Initial commit
    std::fs::write(repo_root.join("file.txt"), "hello").unwrap();
    GitExecutor::new(&repo_root)
        .args(["add", "."])
        .status()
        .unwrap();
    GitExecutor::new(&repo_root)
        .args(["commit", "-m", "initial"])
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .unwrap();

    let config = Config::default();

    // 1. Get local snapshot
    let local_core = Core::attach_or_local(
        repo_root.clone(),
        git_dir.clone(),
        config.clone(),
        ClientKind::Ui,
    )
    .await
    .unwrap();
    let local_snap = local_core.snapshot().await.unwrap();

    // 2. Start daemon
    let service = Service::new(repo_root.clone(), git_dir.clone(), config.clone());
    let socket = stacksaw_core::discovery::socket_path(&git_dir).unwrap();
    let counter = stacksaw_core::server::ClientCounter::default();
    let serve_socket = socket.clone();
    let serve_service = service.clone();
    let serve_counter = counter.clone();
    tokio::spawn(async move {
        let _ = stacksaw_core::server::serve(serve_service, &serve_socket, serve_counter).await;
    });

    // Write discovery file so Core can find it
    let info = stacksaw_core::discovery::DaemonInfo {
        pid: std::process::id(),
        endpoint: stacksaw_core::discovery::endpoint_for(&socket),
        protocol_version: stacksaw_ssp::PROTOCOL_VERSION.to_string(),
        binary_version: "0.1.0".into(),
        started_at: "now".into(),
    };
    stacksaw_core::discovery::write(&git_dir, &info).unwrap();

    // Give it a moment to start listening
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // 3. Get remote handle
    let remote_core = Core::attach_or_local(
        repo_root.clone(),
        git_dir.clone(),
        config.clone(),
        ClientKind::Ui,
    )
    .await
    .unwrap();

    let remote_snap = remote_core.snapshot().await.unwrap();

    // Compare
    assert_eq!(local_snap.staircases.len(), remote_snap.staircases.len());
    assert_eq!(local_snap.staircases, remote_snap.staircases);
    assert_eq!(local_snap.generation, remote_snap.generation);
}
