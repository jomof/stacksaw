//! Shared, schema-bearing domain types (the protocol vocabulary).
//!
//! These are the DTOs that cross every seam: SSP notifications/results (§5.3),
//! CLI --output=json payloads (§10), and agent findings (§7.1). Keeping them
//! in the lowest shared crate gives us **one source of truth** for the wire
//! schema, from which `stacksaw schema <name>` prints JSON Schema.

use crate::git_ref::GitRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The current schema version stamped onto every envelope. Evolution is
/// additive; unknown fields MUST be ignored by readers (§5.2, §10).
pub const SCHEMA_VERSION: u32 = 3;

fn schema_version_default() -> u32 {
    SCHEMA_VERSION
}

// ---------------------------------------------------------------------------
// Findings (§7.1)
// ---------------------------------------------------------------------------

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
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

/// Sentinel `oid` for the virtual "uncommitted changes" commit shown at the tip
/// of the current branch when the worktree is dirty (§8.3). It is not a real git
/// object; the host resolves its files/diffs against the working tree instead of
/// `git show`.
pub const WORKTREE_OID: &str = "working-tree";

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_id: Option<String>,
    #[serde(default)]
    pub finding_counts: FindingCounts,
    /// Twin links: oids of duplicate commits on other branches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub twins: Vec<String>,
    /// Total lines added by this commit vs its first parent (0 if unknown).
    #[serde(default)]
    pub added: u32,
    /// Total lines deleted by this commit vs its first parent (0 if unknown).
    #[serde(default)]
    pub deleted: u32,
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
    pub branch: GitRef,
    /// Index of the parent segment in the enclosing [`Staircase::segments`],
    /// or `None` for the root segment. Encodes the segment *tree* (§2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    /// True when this segment's link to its parent is *stale*: the parent branch
    /// was amended/rebased so this segment no longer descends from the parent's
    /// current tip (its base is a *former* tip recovered from the parent's
    /// reflog). Such a segment needs a **restack** onto the parent's new tip
    /// before the stack is coherent again (§4).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    /// Ordered commits contributed by this step, child-most last.
    pub commits: Vec<CommitSummary>,
}

/// Whether rebasing a staircase onto its upstream is indicated, and if so
/// whether it would apply cleanly. Determined by simulating the rebase in an
/// isolated scratch worktree (never touching real refs). Only meaningful when
/// the staircase is `behind > 0`; `Unknown` when not evaluated (e.g. in sync,
/// or the probe could not run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RebaseStatus {
    /// Not evaluated — no rebase indicated (in sync, or probe unavailable).
    #[default]
    Unknown,
    /// A rebase onto upstream would replay cleanly (a "free" rebase — safe to
    /// offer as a one-click action).
    Clean,
    /// A rebase onto upstream would hit conflicts (a manual/assisted rebase is
    /// required).
    Conflict,
}

/// An ordered branch sequence sharing an upstream (§2). May be a tree when
/// branches fork mid-staircase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Staircase {
    /// Display name: the non-empty common prefix its branches share when it is a
    /// true (multi-branch) staircase, or the lone branch's own name otherwise
    /// (§2). A group of ancestry-linked branches with no shared prefix is not a
    /// staircase but "a bunch of branches", split into one entry each.
    pub name: String,
    /// The upstream ref this staircase is reviewed against.
    pub upstream: GitRef,
    /// Commits ahead of upstream (sum across segments).
    pub ahead: u32,
    /// Commits the upstream has that this staircase lacks.
    pub behind: u32,
    /// Whether the checked-out worktree is dirty on this staircase's tip.
    pub dirty: bool,
    /// Whether a rebase onto upstream is indicated and whether it would be
    /// clean (§4 preview). Additive; readers may ignore it.
    #[serde(default)]
    pub rebase: RebaseStatus,
    /// When `rebase` is `Conflict`, *where* the reflow first breaks: the commit
    /// and files that conflict. `None` for a clean/unknown verdict. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<ConflictInfo>,
    /// Segment tree, root first (topological, then ref name).
    pub segments: Vec<Segment>,
}

/// Where a rebase/restack first conflicts (§4 preview). Derived from the probe:
/// the replay halts at the first offending commit, so this pins the conflict to
/// that commit and its files — the actionable "where", not every downstream
/// clash.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConflictInfo {
    /// Oid of the first commit whose replay conflicts. Matches a
    /// [`CommitSummary::oid`] in the staircase's segments. May be empty if git
    /// reported a conflict without naming the commit.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
    /// Repo-relative paths left conflicted at that commit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum FileStatus {
    #[serde(rename = "A")]
    Added,
    #[serde(rename = "M")]
    Modified,
    #[serde(rename = "D")]
    Deleted,
    #[serde(rename = "R")]
    Renamed,
    #[serde(rename = "C")]
    Copied,
    #[serde(rename = "U")]
    Unmerged,
    #[serde(rename = "?")]
    #[default]
    Untracked,
    #[serde(rename = "!")]
    Ignored,
    #[serde(rename = "✉")]
    Message,
}

impl FileStatus {
    pub fn as_char(self) -> char {
        match self {
            FileStatus::Added => 'A',
            FileStatus::Modified => 'M',
            FileStatus::Deleted => 'D',
            FileStatus::Renamed => 'R',
            FileStatus::Copied => 'C',
            FileStatus::Unmerged => 'U',
            FileStatus::Untracked => '?',
            FileStatus::Ignored => '!',
            FileStatus::Message => '✉',
        }
    }
}

impl fmt::Display for FileStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_char())
    }
}

impl From<char> for FileStatus {
    fn from(c: char) -> Self {
        match c {
            'A' => FileStatus::Added,
            'M' => FileStatus::Modified,
            'D' => FileStatus::Deleted,
            'R' => FileStatus::Renamed,
            'C' => FileStatus::Copied,
            'U' => FileStatus::Unmerged,
            '?' => FileStatus::Untracked,
            '!' => FileStatus::Ignored,
            '✉' => FileStatus::Message,
            _ => FileStatus::Untracked,
        }
    }
}

/// A file changed by a commit (§8.1 Files column). `status` is the git
/// name-status letter: `A`dded, `M`odified, `D`eleted, `R`enamed, etc.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub status: FileStatus,
    pub path: String,
    /// Lines added by this file's change (0 for binary/unknown).
    #[serde(default)]
    pub added: u32,
    /// Lines deleted by this file's change (0 for binary/unknown).
    #[serde(default)]
    pub deleted: u32,
}

/// Full commit metadata for `show` / review headers (§5.3 `commit/get`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommitRecord {
    pub oid: String,
    pub short: String,
    pub subject: String,
    pub body: String,
    pub author: String,
    pub author_email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
    pub parents: Vec<String>,
}

/// Files changed by a commit, enriched for review (§8.1). Wraps [`FileEntry`] with
/// room for per-file finding counts and note anchors as the review surface grows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommitDetail {
    pub oid: String,
    pub generation: u64,
    pub files: Vec<FileEntry>,
}

/// A single change under review in the viewport: commit message, added file
/// content, or a unified diff for a modified path (§8.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ChangeView {
    Message { text: String },
    AddedFile { path: String, content: String },
    ModifiedDiff { path: String, diff: String },
}

/// Domain intent submitted to `mutate/apply` (§4, §5.3). Plans express *what*
/// should happen to the stack, never raw git verbs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum MutatePlan {
    Reshape {
        target_oid: String,
        /// `indent` or `unindent`.
        op: String,
    },
    Archive {
        branches: Vec<String>,
    },
}

/// Outcome of a successful mutation (§4). Carries the new generation and the
/// checkpoint id written before refs moved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MutateResult {
    pub generation: u64,
    pub checkpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<Snapshot>,
}



/// An immutable, generation-numbered view of repo state (§2, §5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(default = "schema_version_default")]
    pub schema_version: u32,
    /// Monotonic generation; bumped on every invalidation (§6).
    pub generation: u64,
    pub head: Option<GitRef>,
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
    pub updated_refs: Vec<GitRef>,
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

    #[test]
    fn file_status_roundtrip() {
        let statuses = vec![
            (FileStatus::Added, "A"),
            (FileStatus::Modified, "M"),
            (FileStatus::Deleted, "D"),
            (FileStatus::Renamed, "R"),
            (FileStatus::Copied, "C"),
            (FileStatus::Unmerged, "U"),
            (FileStatus::Untracked, "?"),
            (FileStatus::Ignored, "!"),
            (FileStatus::Message, "✉"),
        ];

        for (status, expected) in statuses {
            let serialized = serde_json::to_string(&status).unwrap();
            assert_eq!(serialized, format!("\"{}\"", expected));

            let deserialized: FileStatus =
                serde_json::from_str(&format!("\"{}\"", expected)).unwrap();
            assert_eq!(deserialized, status);
        }
    }
}
