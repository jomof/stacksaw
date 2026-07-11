//! Agent Client Protocol (ACP) client (§9.1).
//!
//! stacksaw is the ACP *client*; agents are subprocesses speaking JSON-RPC 2.0
//! over newline-delimited stdio. We reuse the JSON-RPC message types from
//! `stacksaw-ssp` (the shapes are identical) but frame with line delimiters as
//! ACP requires, rather than `Content-Length`.

use futures::StreamExt;
use std::env;
use std::io;
use std::path::Path;
use std::process::Stdio;

use bytes::{BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stacksaw_ssp::client::{Incoming as RpcIncoming, JsonRpcClient};
use stacksaw_ssp::message::{Message, Notification, Request, RequestId};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::codec::{Decoder, Encoder, Framed};

/// The ACP protocol version stacksaw implements against.
pub const ACP_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("spawn failed: {0}")]
    Spawn(io::Error),
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("agent closed the connection")]
    Closed,
    #[error("agent returned error {code}: {message}")]
    Rpc { code: i64, message: String },
}

impl From<stacksaw_ssp::client::ClientError> for AcpError {
    fn from(e: stacksaw_ssp::client::ClientError) -> Self {
        match e {
            stacksaw_ssp::client::ClientError::Closed => AcpError::Closed,
            stacksaw_ssp::client::ClientError::Json(e) => AcpError::Json(e),
            stacksaw_ssp::client::ClientError::Rpc { code, message } => {
                AcpError::Rpc { code, message }
            }
        }
    }
}

/// Something the agent sent us that requires attention: either a streamed
/// session update (notification) or a server→client request (e.g. a permission
/// prompt) which MUST be answered with [`AcpClient::respond`].
#[derive(Debug)]
pub enum Incoming {
    Notification(Notification),
    ServerRequest(Request),
}

/// An ACP client bound to one agent subprocess.
pub struct AcpClient {
    child: Child,
    rpc: JsonRpcClient,
    /// Server→client traffic for the caller to service.
    pub incoming: mpsc::UnboundedReceiver<Incoming>,
}

impl AcpClient {
    /// Spawn an ACP agent (§9.2 configuration points at a command).
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: &Path,
    ) -> Result<Self, AcpError> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        // Env is allowlisted, never inherited wholesale (§13).
        cmd.env_clear();
        if let Ok(path) = env::var("PATH") {
            cmd.env("PATH", path);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(AcpError::Spawn)?;
        let stdin = child.stdin.take().ok_or(AcpError::Closed)?;
        let stdout = child.stdout.take().ok_or(AcpError::Closed)?;

        let framed = Framed::new(AcpStream { stdin, stdout }, AcpCodec);
        let (sink, stream) = framed.split();

        let (rpc, mut rpc_in) = JsonRpcClient::new(sink, stream);
        let (in_tx, in_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Some(msg) = rpc_in.recv().await {
                match msg {
                    RpcIncoming::Notification(n) => {
                        let _ = in_tx.send(Incoming::Notification(n));
                    }
                    RpcIncoming::Request(r) => {
                        let _ = in_tx.send(Incoming::ServerRequest(r));
                    }
                }
            }
        });

        Ok(AcpClient {
            child,
            rpc,
            incoming: in_rx,
        })
    }

    /// Issue a request and await the correlated response.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        Ok(self.rpc.request(method, Some(params)).await?)
    }

    /// Send a notification (no response expected).
    pub fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        Ok(self.rpc.notify(method, Some(params))?)
    }

    /// Answer a server→client request (e.g. a permission prompt).
    pub fn respond(&self, id: RequestId, result: Value) -> Result<(), AcpError> {
        Ok(self.rpc.respond(id, result)?)
    }

    // --- Typed ACP method helpers (§9.1) ---

    pub async fn initialize(&self) -> Result<InitializeResult, AcpError> {
        let params = serde_json::json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "clientCapabilities": { "fs": { "readTextFile": true, "writeTextFile": true } }
        });
        let v = self.request("initialize", params).await?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn new_session(&self, cwd: &Path) -> Result<String, AcpError> {
        let params = serde_json::json!({ "cwd": cwd, "mcpServers": [] });
        let v = self.request("session/new", params).await?;
        let sid = v
            .get("sessionId")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(sid)
    }

    /// Send a prompt turn. Returns the stop reason. Streamed `session/update`
    /// notifications arrive on [`AcpClient::incoming`] concurrently.
    pub async fn prompt(&self, session_id: &str, text: &str) -> Result<String, AcpError> {
        let params = serde_json::json!({
            "sessionId": session_id,
            "prompt": [ { "type": "text", "text": text } ]
        });
        let v = self.request("session/prompt", params).await?;
        Ok(v.get("stopReason")
            .and_then(|s| s.as_str())
            .unwrap_or("end_turn")
            .to_string())
    }

    /// Terminate the agent.
    pub async fn shutdown(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    #[serde(default)]
    pub protocol_version: u32,
    #[serde(default)]
    pub agent_capabilities: Value,
}

/// Build the `_stacksaw/workflowContext` extension payload, which degrades to
/// embedded prompt text if the agent ignores it (§9.1).
pub fn workflow_context(workflow: &str, description: &str) -> Value {
    serde_json::json!({
        "_stacksaw/workflowContext": {
            "workflow": workflow,
            "description": description,
        }
    })
}

struct AcpStream {
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
}

impl tokio::io::AsyncRead for AcpStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for AcpStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

struct AcpCodec;

impl Decoder for AcpCodec {
    type Item = Message;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if let Some(pos) = src.iter().position(|&b| b == b'\n') {
            let line = src.split_to(pos + 1);
            let line = &line[..line.len() - 1];
            if line.is_empty() || line.iter().all(|b| b.is_ascii_whitespace()) {
                return self.decode(src);
            }
            let msg = serde_json::from_slice(line)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }
}

impl Encoder<Message> for AcpCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let json =
            serde_json::to_vec(&item).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        dst.extend_from_slice(&json);
        dst.put_u8(b'\n');
        Ok(())
    }
}
