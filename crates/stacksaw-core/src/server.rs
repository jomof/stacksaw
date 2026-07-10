//! The SSP server: a JSON-RPC 2.0 endpoint over the local socket (§5).

use std::collections::HashSet;
use std::fs::{self, Permissions};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use stacksaw_ssp::message::{ErrorCode, Message, Notification, Request, Response, ResponseError};
use stacksaw_ssp::method;
use stacksaw_ssp::ContentLengthCodec;
use stacksaw_ssp::{is_compatible, PROTOCOL_VERSION};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::codec::Framed;

use crate::service::{ChangeEvent, Service};

/// Count of currently-connected clients, for idle shutdown (§3.1).
#[derive(Clone, Default)]
pub struct ClientCounter(Arc<AtomicUsize>);

impl ClientCounter {
    pub fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
    fn enter(&self) -> ClientGuard {
        self.0.fetch_add(1, Ordering::SeqCst);
        ClientGuard(self.0.clone())
    }
}

struct ClientGuard(Arc<AtomicUsize>);
impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Serve on a Unix socket until `shutdown` fires. Returns the listener's client
/// counter so the caller can implement idle shutdown.
pub async fn serve(service: Service, socket: &Path, counter: ClientCounter) -> anyhow::Result<()> {
    // Fresh socket; the caller has already resolved staleness.
    let _ = fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(socket, Permissions::from_mode(0o600));
    }

    loop {
        let (stream, _addr) = listener.accept().await?;
        // Peer-UID check (§3.1): the socket is 0600 in a 0700 dir, but verify.
        if !peer_uid_ok(&stream) {
            tracing::warn!("rejecting connection from a different uid");
            continue;
        }
        let service = service.clone();
        let guard = counter.enter();
        tokio::spawn(async move {
            let _guard = guard;
            if let Err(e) = handle_connection(service, stream).await {
                tracing::debug!("client connection ended: {e}");
            }
        });
    }
}

#[cfg(unix)]
fn peer_uid_ok(stream: &UnixStream) -> bool {
    match stream.peer_cred() {
        Ok(cred) => {
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let ours = unsafe { getuid() };
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let ours = unsafe { getuid() };
            cred.uid() == ours
        }
        Err(_) => true,
    }
}

#[cfg(not(unix))]
fn peer_uid_ok(_stream: &UnixStream) -> bool {
    true
}

#[cfg(unix)]
extern "C" {
    fn getuid() -> u32;
}

async fn handle_connection(service: Service, stream: UnixStream) -> anyhow::Result<()> {
    let mut framed = Framed::new(stream, ContentLengthCodec::new());
    let mut initialized = false;
    let mut topics: HashSet<String> = HashSet::new();
    let mut events = service.subscribe();

    loop {
        tokio::select! {
            // Inbound requests from the client.
            msg = framed.next() => {
                let Some(msg) = msg else { break };
                let msg = msg?;
                match msg {
                    Message::Request(req) => {
                        let resp = dispatch(&service, &req, &mut initialized, &mut topics).await;
                        framed.send(Message::Response(resp)).await?;
                        if req.method == method::EXIT {
                            break;
                        }
                    }
                    Message::Notification(n) => {
                        if n.method == "initialized" {
                            initialized = true;
                        } else if n.method == method::EXIT {
                            break;
                        }
                    }
                    Message::Response(_) => { /* server-initiated request replies; unused here */ }
                }
            }
            // Outbound change notifications to subscribed clients.
            ev = events.recv() => {
                if let Ok(ev) = ev {
                    if let Some(note) = event_to_notification(&ev, &topics) {
                        framed.send(Message::Notification(note)).await?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn event_to_notification(ev: &ChangeEvent, topics: &HashSet<String>) -> Option<Notification> {
    match ev {
        ChangeEvent::SnapshotAdvanced { generation } if topics.contains("snapshot") => {
            Some(Notification::new(
                method::SNAPSHOT_DID_ADVANCE,
                Some(json!({ "generation": generation })),
            ))
        }
        ChangeEvent::RefsChanged if topics.contains("refs") => {
            Some(Notification::new(method::REFS_DID_CHANGE, None))
        }
        ChangeEvent::WorktreeChanged if topics.contains("worktree") => {
            Some(Notification::new(method::WORKTREE_DID_CHANGE, None))
        }
        _ => None,
    }
}

async fn dispatch(
    service: &Service,
    req: &Request,
    initialized: &mut bool,
    topics: &mut HashSet<String>,
) -> Response {
    let id = req.id.clone();
    match req.method.as_str() {
        method::INITIALIZE => handle_initialize(req, id, initialized).await,
        method::SHUTDOWN => handle_shutdown(id).await,
        method::EXIT => handle_exit(id).await,
        method::SUBSCRIBE => handle_subscribe(req, id, topics).await,
        method::WORKSPACE_SNAPSHOT => handle_workspace_snapshot(service, id).await,
        method::LINT_RUN => handle_lint_run(service, req, id).await,
        _ => Response::err(
            id,
            ResponseError::new(
                ErrorCode::MethodNotFound,
                format!("unknown method {}", req.method),
            ),
        ),
    }
}

async fn handle_initialize(
    req: &Request,
    id: stacksaw_ssp::RequestId,
    initialized: &mut bool,
) -> Response {
    let peer = req
        .params
        .as_ref()
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !is_compatible(peer) {
        return Response::err(
            id,
            ResponseError::new(
                ErrorCode::IncompatibleVersion,
                format!("client protocol {peer} incompatible with {PROTOCOL_VERSION}"),
            ),
        );
    }
    *initialized = true;
    Response::ok(
        id,
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "binaryVersion": env!("CARGO_PKG_VERSION"),
            "serverCapabilities": {
                "topics": ["refs", "worktree", "lint", "agents", "snapshot"],
                "workflows": ["review", "restack"]
            }
        }),
    )
}

async fn handle_shutdown(id: stacksaw_ssp::RequestId) -> Response {
    Response::ok(id, Value::Null)
}

async fn handle_exit(id: stacksaw_ssp::RequestId) -> Response {
    Response::ok(id, Value::Null)
}

async fn handle_subscribe(
    req: &Request,
    id: stacksaw_ssp::RequestId,
    topics: &mut HashSet<String>,
) -> Response {
    if let Some(list) = req
        .params
        .as_ref()
        .and_then(|p| p.get("topics"))
        .and_then(|t| t.as_array())
    {
        for t in list {
            if let Some(s) = t.as_str() {
                topics.insert(s.to_string());
            }
        }
    }
    Response::ok(id, json!({ "ok": true }))
}

async fn handle_workspace_snapshot(service: &Service, id: stacksaw_ssp::RequestId) -> Response {
    match service.snapshot().await {
        Ok(snap) => Response::ok(id, json!({ "snapshot": snap })),
        Err(e) => Response::err(
            id,
            ResponseError::new(ErrorCode::InternalError, e.to_string()),
        ),
    }
}

async fn handle_lint_run(
    service: &Service,
    req: &Request,
    id: stacksaw_ssp::RequestId,
) -> Response {
    // Resolve the scope to a set of commits. For simplicity, a `commit` scope.
    let commit = req
        .params
        .as_ref()
        .and_then(|p| p.get("scope"))
        .and_then(|s| s.get("commit"))
        .and_then(|c| c.as_str())
        .map(str::to_string);

    let commits = match commit {
        Some(c) => vec![c],
        None => {
            // Fall back to linting the whole first staircase's commits.
            match service.snapshot().await {
                Ok(snap) => snap
                    .staircases
                    .first()
                    .map(|s| {
                        s.segments
                            .iter()
                            .flat_map(|seg| seg.commits.iter().map(|c| c.oid.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
                Err(_) => vec![],
            }
        }
    };

    let scheduled = commits.len() as u32;
    match service.lint(commits, stacksaw_lint::Profile::Local).await {
        Ok(_findings) => {
            // In a full implementation we'd stream lint/didFinish; the run id lets
            // the client correlate. We return the count synchronously.
            Response::ok(
                id,
                json!({ "runId": format!("r{}", service.generation()), "scheduled": scheduled }),
            )
        }
        Err(e) => Response::err(
            id,
            ResponseError::new(ErrorCode::InternalError, e.to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use stacksaw_ssp::message::RequestId;
    use std::process::Command;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_dispatch_initialize() {
        let tmp = tempdir().unwrap();
        let service = Service::new(
            tmp.path().to_path_buf(),
            tmp.path().join(".git"),
            Config::default(),
        );
        let req = Request::new(
            RequestId::Number(1),
            method::INITIALIZE,
            Some(json!({ "protocolVersion": PROTOCOL_VERSION })),
        );
        let mut initialized = false;
        let mut topics = HashSet::new();

        let resp = dispatch(&service, &req, &mut initialized, &mut topics).await;

        assert!(initialized);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn test_dispatch_subscribe() {
        let tmp = tempdir().unwrap();
        let service = Service::new(
            tmp.path().to_path_buf(),
            tmp.path().join(".git"),
            Config::default(),
        );
        let req = Request::new(
            RequestId::Number(1),
            method::SUBSCRIBE,
            Some(json!({ "topics": ["refs", "worktree"] })),
        );
        let mut initialized = true;
        let mut topics = HashSet::new();

        let resp = dispatch(&service, &req, &mut initialized, &mut topics).await;

        assert!(resp.error.is_none());
        assert!(topics.contains("refs"));
        assert!(topics.contains("worktree"));
    }

    #[tokio::test]
    async fn test_dispatch_workspace_snapshot() {
        let tmp = tempdir().unwrap();
        // Init a real repo so snapshot works
        let repo_path = tmp.path();
        let status = Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(repo_path)
            .status()
            .unwrap();
        assert!(status.success());

        let service = Service::new(
            repo_path.to_path_buf(),
            repo_path.join(".git"),
            Config::default(),
        );
        let req = Request::new(RequestId::Number(1), method::WORKSPACE_SNAPSHOT, None);
        let mut initialized = true;
        let mut topics = HashSet::new();

        let resp = dispatch(&service, &req, &mut initialized, &mut topics).await;

        assert!(resp.error.is_none());
        assert!(resp.result.unwrap().get("snapshot").is_some());
    }

    #[tokio::test]
    async fn test_dispatch_unknown_method() {
        let tmp = tempdir().unwrap();
        let service = Service::new(
            tmp.path().to_path_buf(),
            tmp.path().join(".git"),
            Config::default(),
        );
        let req = Request::new(RequestId::Number(1), "unknown/method", None);
        let mut initialized = false;
        let mut topics = HashSet::new();

        let resp = dispatch(&service, &req, &mut initialized, &mut topics).await;

        assert_eq!(resp.error.unwrap().code, ErrorCode::MethodNotFound as i64);
    }
}
