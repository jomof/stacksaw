//! SSP server ↔ client conformance: initialize handshake, subscribe, and a
//! full snapshot over the socket (§5.3 AC, subset).

use std::path::Path;
use std::process::Command;

use stacksaw_core::config::Config;
use stacksaw_core::server::{self, ClientCounter};
use stacksaw_core::{Service, SspClient};

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_over_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);
    git(repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "Add a"]);

    // Point feat's fallback upstream at main so a staircase forms.
    let mut config = Config::default();
    config.upstream.default = "refs/heads/main".into();

    let git_dir = repo.join(".git");
    let service = Service::new(repo.to_path_buf(), git_dir.clone(), config);

    // Serve on a socket in the temp dir.
    let socket = tmp.path().join("core.sock");
    let counter = ClientCounter::default();
    let serve_socket = socket.clone();
    let serve_service = service.clone();
    let serve = tokio::spawn(async move {
        let _ = server::serve(serve_service, &serve_socket, counter).await;
    });

    // Write a discovery file so the client can attach.
    let info = stacksaw_core::DaemonInfo {
        pid: std::process::id(),
        endpoint: format!("unix:{}", socket.display()),
        protocol_version: "1.0".into(),
        binary_version: "0.1.0".into(),
        started_at: "now".into(),
    };
    stacksaw_core::discovery::write(&git_dir, &info).unwrap();

    // Give the listener a moment to bind.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut client = SspClient::attach(&git_dir, "cli")
        .await
        .expect("attach to core");
    client.subscribe(&["snapshot", "refs"]).await.unwrap();
    let snap = client.snapshot().await.unwrap();

    let staircases = snap["snapshot"]["staircases"].as_array().unwrap();
    assert!(
        staircases.iter().any(|s| s["name"] == "feat"),
        "expected a 'feat' staircase in {snap}"
    );

    serve.abort();
}
