//! Built-in `ktfqn` linter — a thin adapter over `stacksaw-lint-kotlin` that
//! plugs the reference tree-sitter analyzer into the scheduler (§7.5).

use stacksaw_lint_kotlin::{analyze, KtfqnConfig};
use stacksaw_ssp::types::Finding;

use crate::linter::{LintError, LintJob, Linter};

pub struct KtfqnLinter {
    config: KtfqnConfig,
}

impl KtfqnLinter {
    pub fn new(config: KtfqnConfig) -> Self {
        KtfqnLinter { config }
    }
}

impl Linter for KtfqnLinter {
    fn id(&self) -> &str {
        "ktfqn"
    }
    fn version(&self) -> &str {
        "1"
    }
    fn content_pure(&self) -> bool {
        true
    }

    fn run(&self, job: &LintJob) -> Result<Vec<Finding>, LintError> {
        let mut findings = Vec::new();
        for file in &job.files {
            if !(file.path.ends_with(".kt") || file.path.ends_with(".kts")) {
                continue;
            }
            let Some(content) = &file.content else {
                continue;
            };
            let changed = file.changed_lines();
            let scoped = if self.config.scope == "diff" && !changed.is_empty() {
                Some(&changed)
            } else {
                None
            };
            let mut result = analyze(content, &job.commit, &file.path, &self.config, scoped)
                .map_err(|e| LintError::Failed("ktfqn".into(), e.to_string()))?;
            findings.append(&mut result);
        }
        Ok(findings)
    }
}
