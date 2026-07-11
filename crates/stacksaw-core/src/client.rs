//! A minimal SSP client used by the UI and CLI (§3.1, §5).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use serde_json::{json, Value};
use stacksaw_lint::Profile;
use stacksaw_ssp::client::{Incoming, JsonRpcClient};
use stacksaw_ssp::message::{Notification, RequestId};
use stacksaw_ssp::method::{self};
use stacksaw_ssp::types::{
    ChangeView, CommitDetail, CommitRecord, EditBegin, EditFinish, Finding, MutatePlan,
    MutateResult, ReviewNote, Snapshot,
};
use stacksaw_ssp::{ContentLengthCodec, PROTOCOL_VERSION};
use tokio::net::UnixStream;
use tokio::sync::broadcast;
use tokio_util::codec::Framed;

use crate::discovery;
use crate::service::ChangeEvent;

/// A connected SSP client.
pub struct SspClient {
    rpc: JsonRpcClient,
    events: broadcast::Sender<ChangeEvent>,
    subscribed: AtomicBool,
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
        let framed = Framed::new(stream, ContentLengthCodec::new());
        let (sink, stream) = framed.split();

        let (rpc, mut rpc_in) = JsonRpcClient::new(sink, stream);
        let (events, _) = broadcast::channel(256);

        let events_pumper = events.clone();
        tokio::spawn(async move {
            while let Some(msg) = rpc_in.recv().await {
                if let Incoming::Notification(note) = msg {
                    if let Some(ev) = notification_to_event(&note) {
                        let _ = events_pumper.send(ev);
                    }
                }
            }
        });

        let client = SspClient {
            rpc,
            events,
            subscribed: AtomicBool::new(false),
        };
        client.initialize(client_kind).await.ok()?;
        Some(client)
    }

    async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.rpc
            .request(method, Some(params))
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn initialize(&self, client_kind: &str) -> anyhow::Result<Value> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientKind": client_kind,
            "capabilities": {}
        });
        let r = self.request(method::INITIALIZE, params).await?;
        self.rpc
            .notify("initialized", None)
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(r)
    }

    async fn ensure_subscribed(&self) -> anyhow::Result<()> {
        if self.subscribed.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.request(
            method::SUBSCRIBE,
            json!({ "topics": ["refs", "worktree", "snapshot"] }),
        )
        .await?;
        self.subscribed.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub async fn subscribe_events(&self) -> broadcast::Receiver<ChangeEvent> {
        let _ = self.ensure_subscribed().await;
        self.events.subscribe()
    }

    pub async fn generation(&self) -> anyhow::Result<u64> {
        let snap = self.snapshot().await?;
        Ok(snap.generation)
    }

    pub async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let r = self.request(method::WORKSPACE_SNAPSHOT, json!({})).await?;
        Ok(serde_json::from_value(r["snapshot"].clone())?)
    }

    pub async fn commit_detail(&self, oid: &str) -> anyhow::Result<CommitDetail> {
        let r = self
            .request(method::COMMIT_GET, json!({ "oid": oid }))
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn commit_show(&self, rev: &str) -> anyhow::Result<CommitRecord> {
        let r = self
            .request(method::COMMIT_GET, json!({ "rev": rev }))
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn change_view(&self, commit: &str, path: &str) -> anyhow::Result<ChangeView> {
        let r = self
            .request(
                method::DIFF_RANGE,
                json!({ "commit": commit, "path": path }),
            )
            .await?;
        Ok(serde_json::from_value(r["view"].clone())?)
    }

    pub async fn diff_range(&self, args: &[&str]) -> anyhow::Result<String> {
        let r = self
            .request(method::DIFF_RANGE, json!({ "args": args }))
            .await?;
        Ok(r["text"].as_str().unwrap_or("").to_string())
    }

    pub async fn diff_interdiff(&self, a: &str, b: &str) -> anyhow::Result<String> {
        let r = self
            .request(method::DIFF_INTERDIFF, json!({ "rangeA": a, "rangeB": b }))
            .await?;
        Ok(r["text"].as_str().unwrap_or("").to_string())
    }

    pub async fn mutate(
        &self,
        plan: MutatePlan,
        if_generation: Option<u64>,
    ) -> anyhow::Result<MutateResult> {
        let r = self
            .request(
                method::MUTATE_APPLY,
                json!({ "plan": plan, "ifGeneration": if_generation }),
            )
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn undo(&self, checkpoint: Option<&str>) -> anyhow::Result<MutateResult> {
        let r = self
            .request(method::MUTATE_UNDO, json!({ "checkpoint": checkpoint }))
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn checkpoints_list(&self) -> anyhow::Result<Vec<String>> {
        let r = self.request(method::CHECKPOINTS_LIST, json!({})).await?;
        Ok(serde_json::from_value(r["checkpoints"].clone())?)
    }

    pub async fn worktree_dirty(&self) -> anyhow::Result<bool> {
        let r = self.request("status/worktree", json!({})).await?;
        Ok(r["dirty"].as_bool().unwrap_or(false))
    }

    pub async fn current_branch(&self) -> anyhow::Result<Option<String>> {
        let r = self.request("status/head", json!({})).await?;
        Ok(r["branch"].as_str().map(str::to_string))
    }

    pub async fn note_add(
        &self,
        file: &str,
        line: u32,
        text: &str,
    ) -> anyhow::Result<ReviewNote> {
        let r = self
            .request(
                method::NOTE_ADD,
                json!({ "file": file, "line": line, "text": text }),
            )
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn note_list(&self) -> anyhow::Result<Vec<ReviewNote>> {
        let r = self.request(method::NOTE_LIST, json!({})).await?;
        Ok(serde_json::from_value(r["notes"].clone())?)
    }

    pub async fn lint(
        &self,
        commits: Vec<String>,
        profile: Profile,
    ) -> anyhow::Result<Vec<Finding>> {
        let profile_str = match profile {
            Profile::Local => "local",
            Profile::Upload => "upload",
        };
        let r = self
            .request(
                method::LINT_RUN,
                json!({ "scope": { "commit": commits.first() }, "profile": profile_str }),
            )
            .await?;
        // Synchronous lint path returns findings inline for now.
        if let Some(findings) = r.get("findings") {
            return Ok(serde_json::from_value(findings.clone())?);
        }
        Ok(Vec::new())
    }

    pub async fn edit_begin(&self, commit: &str) -> anyhow::Result<EditBegin> {
        let r = self
            .request("edit/begin", json!({ "commit": commit }))
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn edit_finish(
        &self,
        token: &str,
        message: Option<&str>,
    ) -> anyhow::Result<EditFinish> {
        let r = self
            .request("edit/finish", json!({ "token": token, "message": message }))
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn edit_abort(&self, token: &str) -> anyhow::Result<()> {
        self.request("edit/abort", json!({ "token": token }))
            .await?;
        Ok(())
    }

    pub fn respond(&self, id: RequestId, result: Value) -> anyhow::Result<()> {
        self.rpc.respond(id, result).map_err(|e| anyhow::anyhow!(e))
    }
}

fn notification_to_event(note: &Notification) -> Option<ChangeEvent> {
    match note.method.as_str() {
        method::SNAPSHOT_DID_ADVANCE => {
            let g = note
                .params
                .as_ref()
                .and_then(|p| p.get("generation"))
                .and_then(|v| v.as_u64())?;
            Some(ChangeEvent::SnapshotAdvanced { generation: g })
        }
        method::REFS_DID_CHANGE => Some(ChangeEvent::RefsChanged),
        method::WORKTREE_DID_CHANGE => Some(ChangeEvent::WorktreeChanged),
        _ => None,
    }
}

pub use stacksaw_ssp::method::ClientKind as SspClientKind;
