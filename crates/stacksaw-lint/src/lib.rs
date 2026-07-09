//! `stacksaw-lint` — the finding model, lint scheduler, built-in linters, and
//! the external-command extension tier (§7).

pub mod apply;
pub mod builtins;
pub mod external;
pub mod linter;
pub mod scheduler;

pub use apply::apply_suggestion;
pub use builtins::{
    CommitMsgConfig, CommitMsgLinter, CopyrightConfig, CopyrightLinter, KtfqnLinter,
};
pub use external::{ExecMode, ExternalLinter};
pub use linter::{FileChange, LintError, LintJob, Linter, Profile};
pub use scheduler::{cache_key, collect_findings, config_hash, run, LintOutcome};

// Re-export the ktfqn config so callers configure it in one place.
pub use stacksaw_lint_kotlin::KtfqnConfig;

/// Construct the default set of built-in linters (§7.4 tier 1).
pub fn default_builtins() -> Vec<Box<dyn Linter>> {
    vec![
        Box::new(CommitMsgLinter::new(CommitMsgConfig::default())),
        Box::new(CopyrightLinter::new(CopyrightConfig::default())),
        Box::new(KtfqnLinter::new(KtfqnConfig::default())),
    ]
}
