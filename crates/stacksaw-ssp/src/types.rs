//! Shared, schema-bearing domain types (the protocol vocabulary).
//!
//! These are the DTOs that cross every seam: SSP notifications/results (§5.3),
//! CLI `--output=json` payloads (§10), and agent findings (§7.1). Keeping them
//! in the lowest shared crate gives us **one source of truth** for the wire
//! schema, from which `stacksaw schema <name>` prints JSON Schema.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The current schema version stamped onto every envelope. Evolution is
/// additive; unknown fields MUST be ignored by readers (§5.2, §10).
pub const SCHEMA_VERSION: u32 = 1;

fn schema_version_default() -> u32 {
    SCHEMA_VERSION
}

// ---------------------------------------------------------------------------
// Findings (§7.1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    /// Glyph carrier so severity is never conveyed by color alone (§8.3).
    pub fn glyph(self) -> char {
        match self {
            Severity::Info => 'ℹ',
            Severity::Warning => '⚠',
            Severity::Error => '✗',
        }
    }
}

/// A line/column position. Lines and columns are 1-based, matching editors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// Where a finding is anchored. Either a file range, or (for commit-message
/// linters) a line of the commit message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    /// 1-based line within the commit message, for message-level findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_line: Option<u32>,
}

impl Location {
    pub fn message_line(line: u32) -> Self {
        Location {
            file: None,
            range: None,
            message_line: Some(line),
        }
    }

    pub fn file_range(file: impl Into<String>, range: Range) -> Self {
        Location {
            file: Some(file.into()),
            range: Some(range),
            message_line: None,
        }
    }
}

/// A single edit within a [`Suggestion`]. Either replaces a range or inserts a
/// line after `insert_after_line`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_after_line: Option<u32>,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Suggestion {
    pub edits: Vec<Edit>,
}

/// A structured issue attached to a commit/file/range (§7.1). Produced by
/// linters and agents alike, applied through one code path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    #[serde(default = "schema_version_default")]
    pub schema_version: u32,
    /// e.g. `linter:ktfqn`, `agent:antigravity`, `note:me`.
    pub source: String,
    /// e.g. `ktfqn/avoid-fqn`.
    pub code: String,
    pub severity: Severity,
    /// Abbreviated commit oid the finding is attached to.
    pub commit: String,
    pub location: Location,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<Suggestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl Finding {
    pub fn is_autofixable(&self) -> bool {
        self.suggestion.is_some() && self.tags.iter().any(|t| t == "autofixable")
    }
}

// ---------------------------------------------------------------------------
// Staircases and snapshots (§2, §5.3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommitSummary {
    pub oid: String,
    pub short: String,
    pub subject: String,
    pub author: String,
    /// Author time as a unix timestamp (seconds).
    pub author_time: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    /// `Change-Id` trailer, if present (used for twin detection, §2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
    #[serde(default)]
    pub finding_counts: FindingCounts,
    /// Twin links: oids of duplicate commits on other branches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub twins: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FindingCounts {
    pub error: u32,
    pub warning: u32,
    pub info: u32,
}

impl FindingCounts {
    pub fn total(&self) -> u32 {
        self.error + self.warning + self.info
    }

    pub fn add(&mut self, sev: Severity) {
        match sev {
            Severity::Error => self.error += 1,
            Severity::Warning => self.warning += 1,
            Severity::Info => self.info += 1,
        }
    }
}

/// A staircase step's contribution: `Seg(Bᵢ) = (tip(Bᵢ₋₁), tip(Bᵢ)]` (§2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    /// The branch ref name this segment belongs to.
    pub branch: String,
    /// Index of the parent segment in the enclosing [`Staircase::segments`],
    /// or `None` for the root segment. Encodes the segment *tree* (§2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    /// Ordered commits contributed by this step, child-most last.
    pub commits: Vec<CommitSummary>,
}

/// An ordered branch sequence sharing an upstream (§2). May be a tree when
/// branches fork mid-staircase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Staircase {
    /// The tip-most branch name; used as the staircase's display name.
    pub name: String,
    /// The upstream ref this staircase is reviewed against.
    pub upstream: String,
    /// Commits ahead of upstream (sum across segments).
    pub ahead: u32,
    /// Commits the upstream has that this staircase lacks.
    pub behind: u32,
    /// Whether the checked-out worktree is dirty on this staircase's tip.
    pub dirty: bool,
    /// Segment tree, root first (topological, then ref name).
    pub segments: Vec<Segment>,
}

/// An immutable, generation-numbered view of repo state (§2, §5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(default = "schema_version_default")]
    pub schema_version: u32,
    /// Monotonic generation; bumped on every invalidation (§6).
    pub generation: u64,
    pub head: Option<String>,
    pub detached: bool,
    pub staircases: Vec<Staircase>,
}

// ---------------------------------------------------------------------------
// CLI-facing envelopes (§10)
// ---------------------------------------------------------------------------

/// The `stacksaw edit begin` result (§10.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EditBegin {
    #[serde(default = "schema_version_default")]
    pub schema_version: u32,
    pub token: String,
    pub worktree: String,
    pub commit: String,
    pub descendants: u32,
}

/// A single old→new rewrite in an edit/restack result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rewrite {
    pub old: String,
    pub new: String,
}

/// The `stacksaw edit finish` result (§10.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EditFinish {
    #[serde(default = "schema_version_default")]
    pub schema_version: u32,
    pub rewrites: Vec<Rewrite>,
    pub updated_refs: Vec<String>,
    pub checkpoint: String,
}

/// Structured error envelope for `--output=json` on stderr (§10).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl ErrorEnvelope {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        ErrorEnvelope {
            error: ErrorBody {
                code: code.into(),
                message: message.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_autofix_detection() {
        let mut f = Finding {
            schema_version: SCHEMA_VERSION,
            source: "linter:ktfqn".into(),
            code: "ktfqn/avoid-fqn".into(),
            severity: Severity::Warning,
            commit: "8c1f".into(),
            location: Location::message_line(1),
            message: "x".into(),
            suggestion: None,
            tags: vec![],
        };
        assert!(!f.is_autofixable());
        f.suggestion = Some(Suggestion { edits: vec![] });
        f.tags.push("autofixable".into());
        assert!(f.is_autofixable());
    }

    #[test]
    fn severity_orders_by_intensity() {
        assert!(Severity::Error > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
    }
}
