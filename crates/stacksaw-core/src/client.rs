//! A minimal SSP client used by the UI and by CLI commands that attach to a
//! running core (§3.1, §5). Falls back to in-process when no daemon exists.

use std::path::Path;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use stacksaw_ssp::message::{Message, Notification, Request};
use stacksaw_ssp::{method, ContentLengthCodec, PROTOCOL_VERSION};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

use crate::discovery;

/// A connected SSP client.
pub struct SspClient {
    framed: Framed<UnixStream, ContentLengthCodec>,
    next_id: i64,
}

impl SspClient {
    /// Attach to the running core for the repo at `git_common_dir`, validating
    /// liveness with a full `initialize` handshake (§3.1). Returns `None` if no
    /// healthy daemon is present.
    pub async fn attach(git_common_dir: &Path, client_kind: &str) -> Option<Self> {
        let info = discovery::read(git_common_dir)?;
        if !discovery::pid_alive(info.pid) {
            discovery::remove(git_common_dir);
            return None;
        }
        let socket = info.socket_path()?;
        let stream = UnixStream::connect(&socket).await.ok()?;
        let mut client = SspClient {
            framed: Framed::new(stream, ContentLengthCodec::new()),
            next_id: 1,
        };
        client.initialize(client_kind).await.ok()?;
        Some(client)
    }

    async fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request::new(id, method, Some(params));
        self.framed.send(Message::Request(req)).await?;
        while let Some(msg) = self.framed.next().await {
            if let Message::Response(resp) = msg? {
                if let Some(err) = resp.error {
                    anyhow::bail!("rpc error {}: {}", err.code, err.message);
                }
                return Ok(resp.result.unwrap_or(Value::Null));
            }
        }
        anyhow::bail!("connection closed before response")
    }

    async fn initialize(&mut self, client_kind: &str) -> anyhow::Result<Value> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientKind": client_kind,
            "capabilities": {}
        });
        let r = self.request(method::INITIALIZE, params).await?;
        // Complete the handshake.
        self.framed
            .send(Message::Notification(Notification::new(
                "initialized",
                None,
            )))
            .await?;
        Ok(r)
    }

    pub async fn subscribe(&mut self, topics: &[&str]) -> anyhow::Result<()> {
        self.request(method::SUBSCRIBE, json!({ "topics": topics }))
            .await?;
        Ok(())
    }

    pub async fn snapshot(&mut self) -> anyhow::Result<Value> {
        self.request(method::WORKSPACE_SNAPSHOT, json!({})).await
    }
}
