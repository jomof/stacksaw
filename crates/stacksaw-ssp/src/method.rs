//! The SSP method surface (§5.3). Method names are string constants used by
//! both client and server; params/results are the DTOs in [`crate::types`]
//! plus a handful of small request/notification shapes defined here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::Snapshot;

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
pub const MUTATE_APPLY: &str = "mutate/apply";
pub const MUTATE_UNDO: &str = "mutate/undo";

pub const CHECKPOINTS_LIST: &str = "checkpoints/list";
pub const UI_LINK: &str = "ui/link";
pub const UI_DID_FOCUS: &str = "ui/didFocus";

/// Server → client
pub const REFS_DID_CHANGE: &str = "refs/didChange";
pub const WORKTREE_DID_CHANGE: &str = "worktree/didChange";
pub const SNAPSHOT_DID_ADVANCE: &str = "snapshot/didAdvance";

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
    /// One of: `refs`, `worktree`, `snapshot`.
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
