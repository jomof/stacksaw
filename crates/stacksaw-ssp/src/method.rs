//! The SSP method surface (§5.3). Method names are string constants used by
//! both client and server; params/results are the DTOs in [`crate::types`]
//! plus a handful of small request/notification shapes defined here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{Finding, Snapshot};

/// Lifecycle
pub const INITIALIZE: &str = "initialize";
pub const SHUTDOWN: &str = "shutdown";
pub const EXIT: &str = "exit";
pub const CANCEL_REQUEST: &str = "$/cancelRequest";
pub const PROGRESS: &str = "$/progress";

/// Client → server
pub const SUBSCRIBE: &str = "subscribe";
pub const WORKSPACE_SNAPSHOT: &str = "workspace/snapshot";
pub const COMMIT_GET: &str = "commit/get";
pub const DIFF_RANGE: &str = "diff/range";
pub const DIFF_INTERDIFF: &str = "diff/interdiff";
pub const LINT_RUN: &str = "lint/run";
pub const LINT_CANCEL: &str = "lint/cancel";
pub const AGENT_LIST: &str = "agent/list";
pub const AGENT_START: &str = "agent/start";
pub const AGENT_PROMPT: &str = "agent/prompt";
pub const AGENT_CANCEL: &str = "agent/cancel";
pub const MUTATE_APPLY: &str = "mutate/apply";
pub const MUTATE_UNDO: &str = "mutate/undo";
pub const NOTE_ADD: &str = "note/add";
pub const NOTE_LIST: &str = "note/list";
pub const CHECKPOINTS_LIST: &str = "checkpoints/list";
pub const UI_LINK: &str = "ui/link";
pub const UI_DID_FOCUS: &str = "ui/didFocus";

/// Server → client
pub const AGENT_PERMISSION: &str = "agent/permission";
pub const REFS_DID_CHANGE: &str = "refs/didChange";
pub const WORKTREE_DID_CHANGE: &str = "worktree/didChange";
pub const SNAPSHOT_DID_ADVANCE: &str = "snapshot/didAdvance";
pub const LINT_DID_FINISH: &str = "lint/didFinish";
pub const AGENT_DID_UPDATE: &str = "agent/didUpdate";

/// What kind of client is connecting (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    Ui,
    Cli,
    Automation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    pub client_kind: ClientKind,
    #[serde(default)]
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// The client can render agent permission prompts (§5.3 `agent/permission`).
    #[serde(default)]
    pub agent_permissions: bool,
    /// The client wants delta snapshots rather than full ones.
    #[serde(default)]
    pub delta_snapshots: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub binary_version: String,
    pub server_capabilities: ServerCapabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    pub topics: Vec<String>,
    pub workflows: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeParams {
    /// One of: `refs`, `worktree`, `lint`, `agents`, `snapshot`.
    pub topics: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotParams {
    /// If given, request only a delta since this generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotResult {
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDidAdvance {
    pub generation: u64,
}

/// Scope for a lint run (§7.2). Exactly one field is normally set.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LintScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stair: Option<String>,
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LintRunParams {
    pub scope: LintScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linters: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LintRunResult {
    pub run_id: String,
    pub scheduled: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LintDidFinish {
    pub run_id: String,
    pub findings: Vec<Finding>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_params_use_camel_case() {
        let p = InitializeParams {
            protocol_version: "1.0".into(),
            client_kind: ClientKind::Cli,
            capabilities: Capabilities::default(),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("protocolVersion"));
        assert!(s.contains("\"cli\""));
    }
}
