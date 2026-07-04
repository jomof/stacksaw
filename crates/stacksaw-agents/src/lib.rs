//! `stacksaw-agents` — the ACP client, workflow orchestration, and the restack
//! state machine (§9).

pub mod acp;
pub mod policy;
pub mod restack;
pub mod workflow;

pub use acp::{AcpClient, AcpError, Incoming};
pub use policy::{Decision, Policy};
pub use restack::{RestackError, RestackOutcome, Restacker, StopKind};
pub use workflow::{ConflictPolicy, FixPolicy, RestackParams, ReviewParams, Workflow};
