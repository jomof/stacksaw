//! The linter model shared by built-in, external, and (future) WASM tiers
//! (§7.1–§7.4).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use stacksaw_ssp::types::Finding;

/// Lint profile (§7.5): `local` is lenient, `upload` is the strict pre-flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    #[default]
    Local,
    Upload,
}

impl std::str::FromStr for Profile {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "local" => Ok(Profile::Local),
            "upload" => Ok(Profile::Upload),
            other => Err(format!("unknown profile {other:?}")),
        }
    }
}

/// A file changed by a commit, as presented to linters (§7.4 external contract).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_oid: Option<String>,
    /// Inclusive 1-based line ranges changed by this commit.
    #[serde(default)]
    pub changed_ranges: Vec<(u32, u32)>,
    /// New file content (post-commit), when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Whether the file was newly added in this commit.
    #[serde(default)]
    pub added: bool,
}

impl FileChange {
    /// The set of changed lines, for diff-scoped linters.
    pub fn changed_lines(&self) -> std::collections::HashSet<u32> {
        let mut set = std::collections::HashSet::new();
        for (a, b) in &self.changed_ranges {
            for l in *a..=*b {
                set.insert(l);
            }
        }
        set
    }
}

/// A per-commit lint job (§7.2, §7.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LintJob {
    /// Abbreviated commit oid.
    pub commit: String,
    /// Author year, for `{year}` validation in the copyright linter.
    pub author_year: i32,
    /// Full commit message.
    pub message: String,
    pub files: Vec<FileChange>,
    pub repo_root: PathBuf,
    /// A read-only checkout path when `mode = "tree"` (§7.4).
    pub worktree: PathBuf,
    #[serde(default)]
    pub profile: Profile,
}

#[derive(Debug, thiserror::Error)]
pub enum LintError {
    #[error("linter {0} failed: {1}")]
    Failed(String, String),
    #[error("external linter i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("external linter produced invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("external linter timed out after {0:?}")]
    Timeout(std::time::Duration),
}

/// The common linter interface (§7.1).
pub trait Linter: Send + Sync {
    fn id(&self) -> &str;
    fn version(&self) -> &str;

    /// When true, results depend only on commit *content*, so a restack that
    /// preserves the patch-id MAY reuse cached results (§7.2).
    fn content_pure(&self) -> bool {
        false
    }

    fn run(&self, job: &LintJob) -> Result<Vec<Finding>, LintError>;
}
