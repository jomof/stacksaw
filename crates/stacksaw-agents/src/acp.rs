//! Agent Client Protocol (ACP) client (§9.1).
//!
//! stacksaw is the ACP *client*; agents are subprocesses speaking JSON-RPC 2.0
//! over newline-delimited stdio. We reuse the JSON-RPC message types from
//! `stacksaw-ssp` (the shapes are identical) but frame with line delimiters as
//! ACP requires, rather than `Content-Length`.

use std::collections::HashMap;
use std::env;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use stacksaw_ssp::message::{Message, Notification, Request, RequestId, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

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
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Response>>>>,
    outbound: mpsc::UnboundedSender<String>,
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

        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Response>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<Incoming>();

        // Writer task.
        let mut stdin = stdin;
        tokio::spawn(async move {
            while let Some(line) = out_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // Reader task.
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<Message>(&line) else {
                    continue;
                };
                match msg {
                    Message::Response(resp) => {
                        if let RequestId::Number(id) = resp.id.clone() {
                            if let Some(tx) = pending_reader.lock().await.remove(&id) {
                                let _ = tx.send(resp);
                            }
                        }
                    }
                    Message::Request(req) => {
                        let _ = in_tx.send(Incoming::ServerRequest(req));
                    }
                    Message::Notification(n) => {
                        let _ = in_tx.send(Incoming::Notification(n));
                    }
                }
            }
        });

        Ok(AcpClient {
            child,
            next_id: AtomicI64::new(1),
            pending,
            outbound: out_tx,
            incoming: in_rx,
        })
    }

    /// Issue a request and await the correlated response.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let req = Request::new(id, method, Some(params));
        let line = serde_json::to_string(&Message::Request(req))?;
        self.outbound.send(line).map_err(|_| AcpError::Closed)?;

        let resp = rx.await.map_err(|_| AcpError::Closed)?;
        if let Some(err) = resp.error {
            return Err(AcpError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    /// Send a notification (no response expected).
    pub fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        let n = Notification::new(method, Some(params));
        let line = serde_json::to_string(&Message::Notification(n))?;
        self.outbound.send(line).map_err(|_| AcpError::Closed)
    }

    /// Answer a server→client request (e.g. a permission prompt).
    pub fn respond(&self, id: RequestId, result: Value) -> Result<(), AcpError> {
        let resp = Response::ok(id, result);
        let line = serde_json::to_string(&Message::Response(resp))?;
        self.outbound.send(line).map_err(|_| AcpError::Closed)
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
