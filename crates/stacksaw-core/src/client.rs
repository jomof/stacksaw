//! A minimal SSP client used by the UI and CLI (§3.1, §5).

use std::path::Path;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use stacksaw_lint::Profile;
use stacksaw_ssp::message::{Message, Notification, Request};
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
    framed: Framed<UnixStream, ContentLengthCodec>,
    next_id: i64,
    events: broadcast::Sender<ChangeEvent>,
    subscribed: bool,
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
        let (events, _) = broadcast::channel(256);
        let mut client = SspClient {
            framed: Framed::new(stream, ContentLengthCodec::new()),
            next_id: 1,
            events,
            subscribed: false,
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
            match msg? {
                Message::Response(resp) => {
                    if let Some(err) = resp.error {
                        anyhow::bail!("rpc error {}: {}", err.code, err.message);
                    }
                    return Ok(resp.result.unwrap_or(Value::Null));
                }
                Message::Notification(note) => {
                    if let Some(ev) = notification_to_event(&note) {
                        let _ = self.events.send(ev);
                    }
                }
                Message::Request(_) => {}
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
        self.framed
            .send(Message::Notification(Notification::new(
                "initialized",
                None,
            )))
            .await?;
        Ok(r)
    }

    async fn ensure_subscribed(&mut self) -> anyhow::Result<()> {
        if self.subscribed {
            return Ok(());
        }
        self.request(
            method::SUBSCRIBE,
            json!({ "topics": ["refs", "worktree", "snapshot"] }),
        )
        .await?;
        self.subscribed = true;
        Ok(())
    }

    pub async fn subscribe_events(&mut self) -> broadcast::Receiver<ChangeEvent> {
        let _ = self.ensure_subscribed().await;
        self.events.subscribe()
    }

    pub async fn generation(&mut self) -> anyhow::Result<u64> {
        let snap = self.snapshot().await?;
        Ok(snap.generation)
    }

    pub async fn snapshot(&mut self) -> anyhow::Result<Snapshot> {
        let r = self.request(method::WORKSPACE_SNAPSHOT, json!({})).await?;
        Ok(serde_json::from_value(r["snapshot"].clone())?)
    }

    pub async fn commit_detail(&mut self, oid: &str) -> anyhow::Result<CommitDetail> {
        let r = self
            .request(method::COMMIT_GET, json!({ "oid": oid }))
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn commit_show(&mut self, rev: &str) -> anyhow::Result<CommitRecord> {
        let r = self
            .request(method::COMMIT_GET, json!({ "rev": rev }))
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn change_view(&mut self, commit: &str, path: &str) -> anyhow::Result<ChangeView> {
        let r = self
            .request(
                method::DIFF_RANGE,
                json!({ "commit": commit, "path": path }),
            )
            .await?;
        Ok(serde_json::from_value(r["view"].clone())?)
    }

    pub async fn diff_range(&mut self, args: &[&str]) -> anyhow::Result<String> {
        let r = self
            .request(method::DIFF_RANGE, json!({ "args": args }))
            .await?;
        Ok(r["text"].as_str().unwrap_or("").to_string())
    }

    pub async fn diff_interdiff(&mut self, a: &str, b: &str) -> anyhow::Result<String> {
        let r = self
            .request(method::DIFF_INTERDIFF, json!({ "rangeA": a, "rangeB": b }))
            .await?;
        Ok(r["text"].as_str().unwrap_or("").to_string())
    }

    pub async fn mutate(
        &mut self,
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

    pub async fn undo(&mut self, checkpoint: Option<&str>) -> anyhow::Result<MutateResult> {
        let r = self
            .request(method::MUTATE_UNDO, json!({ "checkpoint": checkpoint }))
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn checkpoints_list(&mut self) -> anyhow::Result<Vec<String>> {
        let r = self.request(method::CHECKPOINTS_LIST, json!({})).await?;
        Ok(serde_json::from_value(r["checkpoints"].clone())?)
    }

    pub async fn worktree_dirty(&mut self) -> anyhow::Result<bool> {
        let r = self.request("status/worktree", json!({})).await?;
        Ok(r["dirty"].as_bool().unwrap_or(false))
    }

    pub async fn current_branch(&mut self) -> anyhow::Result<Option<String>> {
        let r = self.request("status/head", json!({})).await?;
        Ok(r["branch"].as_str().map(str::to_string))
    }

    pub async fn note_add(
        &mut self,
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

    pub async fn note_list(&mut self) -> anyhow::Result<Vec<ReviewNote>> {
        let r = self.request(method::NOTE_LIST, json!({})).await?;
        Ok(serde_json::from_value(r["notes"].clone())?)
    }

    pub async fn lint(
        &mut self,
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

    pub async fn edit_begin(&mut self, commit: &str) -> anyhow::Result<EditBegin> {
        let r = self
            .request("edit/begin", json!({ "commit": commit }))
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn edit_finish(
        &mut self,
        token: &str,
        message: Option<&str>,
    ) -> anyhow::Result<EditFinish> {
        let r = self
            .request("edit/finish", json!({ "token": token, "message": message }))
            .await?;
        Ok(serde_json::from_value(r)?)
    }

    pub async fn edit_abort(&mut self, token: &str) -> anyhow::Result<()> {
        self.request("edit/abort", json!({ "token": token }))
            .await?;
        Ok(())
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
