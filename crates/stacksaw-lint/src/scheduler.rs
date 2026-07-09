//! Lint scheduling and caching keys (§7.2).
//!
//! Linters run per commit in parent-before-child order, fanned out on the
//! rayon pool. This crate computes results and cache keys; the persistent redb
//! cache lives in `stacksaw-core`.

use rayon::prelude::*;
use stacksaw_ssp::types::Finding;

use crate::linter::{LintError, LintJob, Linter};

/// A blake3 cache key over `(commit-oid ‖ linter-id ‖ linter-version ‖
/// config-hash)` (§7.2, §4 hashing).
pub fn cache_key(
    commit_oid: &str,
    linter_id: &str,
    linter_version: &str,
    config_hash: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(commit_oid.as_bytes());
    hasher.update(b"\x00");
    hasher.update(linter_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(linter_version.as_bytes());
    hasher.update(b"\x00");
    hasher.update(config_hash.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Hash an arbitrary config value into a stable string for the cache key.
pub fn config_hash(value: &serde_json::Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_default();
    blake3::hash(text.as_bytes()).to_hex()[..16].to_string()
}

/// Result of running one linter over one commit.
pub struct LintOutcome {
    pub commit: String,
    pub linter_id: String,
    pub result: Result<Vec<Finding>, LintError>,
}

/// Run every linter over every job. `jobs` MUST already be ordered
/// parent-before-child (§7.2); the fan-out itself is order-independent.
pub fn run(jobs: &[LintJob], linters: &[Box<dyn Linter>]) -> Vec<LintOutcome> {
    // Build the (job, linter) work set, then fan out on rayon. We never run on
    // tokio worker threads (§4 parallelism).
    let work: Vec<(&LintJob, &Box<dyn Linter>)> = jobs
        .iter()
        .flat_map(|job| linters.iter().map(move |l| (job, l)))
        .collect();

    work.par_iter()
        .map(|(job, linter)| LintOutcome {
            commit: job.commit.clone(),
            linter_id: linter.id().to_string(),
            result: linter.run(job),
        })
        .collect()
}

/// Flatten outcomes into findings, dropping (but not hiding) linter errors.
pub fn collect_findings(outcomes: Vec<LintOutcome>) -> (Vec<Finding>, Vec<(String, String)>) {
    let mut findings = Vec::new();
    let mut errors = Vec::new();
    for o in outcomes {
        match o.result {
            Ok(mut fs) => findings.append(&mut fs),
            Err(e) => errors.push((o.linter_id, e.to_string())),
        }
    }
    findings.sort_by(|a, b| {
        a.commit
            .cmp(&b.commit)
            .then(b.severity.cmp(&a.severity))
            .then(a.code.cmp(&b.code))
    });
    (findings, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_stable_and_sensitive() {
        let a = cache_key("oid1", "ktfqn", "1", "cfg");
        let b = cache_key("oid1", "ktfqn", "1", "cfg");
        assert_eq!(a, b);
        assert_ne!(a, cache_key("oid2", "ktfqn", "1", "cfg"));
        assert_ne!(a, cache_key("oid1", "ktfqn", "2", "cfg"));
        assert_ne!(a, cache_key("oid1", "ktfqn", "1", "other"));
    }
}
