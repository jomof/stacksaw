//! Workflow contracts (§9.4): structured context in, structured result out.

use serde::{Deserialize, Serialize};

/// How conflicts encountered during a restack are handled (§9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy {
    /// Delegate resolution to the agent.
    Agent,
    /// Stop and hand back to the human.
    Stop,
}

/// What lint failures the fix loop should act on (§9.4/§9.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixPolicy {
    #[serde(default)]
    pub linters: Vec<String>,
    /// `error` or `warning`.
    #[serde(default = "d_fail_on")]
    pub fail_on: String,
}

fn d_fail_on() -> String {
    "error".into()
}

impl Default for FixPolicy {
    fn default() -> Self {
        FixPolicy {
            linters: vec![],
            fail_on: d_fail_on(),
        }
    }
}

/// Parameters for the `restack` workflow (§9.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestackParams {
    /// The staircase's branch refs, root → tip order.
    pub staircase: Vec<String>,
    /// The ref to rebase onto (upstream).
    pub onto: String,
    #[serde(default)]
    pub fix_policy: FixPolicy,
    #[serde(default = "d_conflict")]
    pub conflict_policy: ConflictPolicy,
    /// Max agent attempts per stop before pausing for the human (§9.5 step 6).
    #[serde(default = "d_max_attempts")]
    pub max_attempts: u32,
}

fn d_conflict() -> ConflictPolicy {
    ConflictPolicy::Stop
}
fn d_max_attempts() -> u32 {
    3
}

/// Parameters for the `review` workflow (§9.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewParams {
    pub staircase: Vec<String>,
    #[serde(default)]
    pub guidelines: Option<String>,
}

/// A named workflow contract an agent may accept (§9.2 `workflows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workflow {
    Review,
    Restack,
}

impl Workflow {
    pub fn name(self) -> &'static str {
        match self {
            Workflow::Review => "review",
            Workflow::Restack => "restack",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "review" => Some(Workflow::Review),
            "restack" => Some(Workflow::Restack),
            _ => None,
        }
    }
}
