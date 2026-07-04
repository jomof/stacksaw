//! Built-in `commitmsg` linter (§7.5).

use regex::Regex;
use serde::{Deserialize, Serialize};
use stacksaw_ssp::types::{Finding, Location, Severity, SCHEMA_VERSION};

use crate::linter::{LintError, LintJob, Linter, Profile};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitMsgConfig {
    #[serde(default = "d_subject_warn")]
    pub subject_max_warn: usize,
    #[serde(default = "d_subject_error")]
    pub subject_max_error: usize,
    #[serde(default = "d_true")]
    pub blank_line_after_subject: bool,
    #[serde(default = "d_body_wrap")]
    pub body_wrap: usize,
    /// Regex patterns that MUST each match some trailer line, e.g.
    /// `^Change-Id: I[0-9a-f]{40}$`.
    #[serde(default)]
    pub required_trailers: Vec<String>,
    /// Subject prefixes forbidden on the `upload` profile.
    #[serde(default = "d_forbidden")]
    pub forbidden_prefixes_upload: Vec<String>,
    /// Off by default (§7.5): imperative-mood heuristic.
    #[serde(default)]
    pub subject_mood: bool,
}

fn d_subject_warn() -> usize {
    50
}
fn d_subject_error() -> usize {
    72
}
fn d_true() -> bool {
    true
}
fn d_body_wrap() -> usize {
    72
}
fn d_forbidden() -> Vec<String> {
    vec!["WIP".into(), "fixup!".into(), "squash!".into()]
}

impl Default for CommitMsgConfig {
    fn default() -> Self {
        CommitMsgConfig {
            subject_max_warn: 50,
            subject_max_error: 72,
            blank_line_after_subject: true,
            body_wrap: 72,
            required_trailers: Vec::new(),
            forbidden_prefixes_upload: d_forbidden(),
            subject_mood: false,
        }
    }
}

pub struct CommitMsgLinter {
    config: CommitMsgConfig,
}

impl CommitMsgLinter {
    pub fn new(config: CommitMsgConfig) -> Self {
        CommitMsgLinter { config }
    }
}

impl Linter for CommitMsgLinter {
    fn id(&self) -> &str {
        "commitmsg"
    }
    fn version(&self) -> &str {
        "1"
    }
    fn content_pure(&self) -> bool {
        true
    }

    fn run(&self, job: &LintJob) -> Result<Vec<Finding>, LintError> {
        let cfg = &self.config;
        let mut findings = Vec::new();
        let lines: Vec<&str> = job.message.lines().collect();
        let commit = job.commit.clone();

        let mut push = |code: &str, sev: Severity, line: u32, msg: String| {
            findings.push(Finding {
                schema_version: SCHEMA_VERSION,
                source: "linter:commitmsg".into(),
                code: format!("commitmsg/{code}"),
                severity: sev,
                commit: commit.clone(),
                location: Location::message_line(line),
                message: msg,
                suggestion: None,
                tags: vec![],
            });
        };

        let subject = lines.first().copied().unwrap_or("");
        let slen = subject.chars().count();
        if slen > cfg.subject_max_error {
            push(
                "subject-too-long",
                Severity::Error,
                1,
                format!("Subject is {slen} chars (max {})", cfg.subject_max_error),
            );
        } else if slen > cfg.subject_max_warn {
            push(
                "subject-long",
                Severity::Warning,
                1,
                format!(
                    "Subject is {slen} chars (prefer ≤ {})",
                    cfg.subject_max_warn
                ),
            );
        }

        if cfg.blank_line_after_subject && lines.len() > 1 && !lines[1].trim().is_empty() {
            push(
                "no-blank-after-subject",
                Severity::Warning,
                2,
                "Leave a blank line after the subject".into(),
            );
        }

        for (i, line) in lines.iter().enumerate().skip(2) {
            let len = line.chars().count();
            if len > cfg.body_wrap && !is_trailer(line) && !line.contains("://") {
                push(
                    "body-too-wide",
                    Severity::Warning,
                    (i as u32) + 1,
                    format!("Body line is {len} chars (wrap at {})", cfg.body_wrap),
                );
            }
        }

        for pat in &cfg.required_trailers {
            let re = Regex::new(pat)
                .map_err(|e| LintError::Failed("commitmsg".into(), e.to_string()))?;
            let ok = lines.iter().any(|l| re.is_match(l));
            if !ok {
                push(
                    "missing-trailer",
                    Severity::Error,
                    lines.len().max(1) as u32,
                    format!("Required trailer matching /{pat}/ is missing"),
                );
            }
        }

        if job.profile == Profile::Upload {
            for prefix in &cfg.forbidden_prefixes_upload {
                if subject.starts_with(prefix.as_str()) {
                    push(
                        "forbidden-prefix",
                        Severity::Error,
                        1,
                        format!("Subject must not start with {prefix:?} on upload"),
                    );
                }
            }
        }

        Ok(findings)
    }
}

fn is_trailer(line: &str) -> bool {
    // A trailer looks like `Token: value` with a capitalized token.
    if let Some((key, _)) = line.split_once(": ") {
        !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            && key.chars().next().is_some_and(|c| c.is_ascii_uppercase())
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn job(msg: &str, profile: Profile) -> LintJob {
        LintJob {
            commit: "abc123".into(),
            author_year: 2026,
            message: msg.into(),
            files: vec![],
            repo_root: PathBuf::from("/tmp"),
            worktree: PathBuf::from("/tmp"),
            profile,
        }
    }

    #[test]
    fn flags_overlong_subject() {
        let long = "x".repeat(80);
        let f = CommitMsgLinter::new(CommitMsgConfig::default())
            .run(&job(&long, Profile::Local))
            .unwrap();
        assert!(f.iter().any(|f| f.code == "commitmsg/subject-too-long"
            && f.severity == Severity::Error));
    }

    #[test]
    fn flags_missing_blank_line() {
        let f = CommitMsgLinter::new(CommitMsgConfig::default())
            .run(&job("Subject here\nbody immediately", Profile::Local))
            .unwrap();
        assert!(f.iter().any(|f| f.code == "commitmsg/no-blank-after-subject"));
    }

    #[test]
    fn requires_change_id() {
        let cfg = CommitMsgConfig {
            required_trailers: vec!["^Change-Id: I[0-9a-f]{40}$".into()],
            ..Default::default()
        };
        let f = CommitMsgLinter::new(cfg.clone())
            .run(&job("Do a thing\n\nbody", Profile::Local))
            .unwrap();
        assert!(f.iter().any(|f| f.code == "commitmsg/missing-trailer"));

        let ok = CommitMsgLinter::new(cfg)
            .run(&job(
                &format!("Do a thing\n\nbody\n\nChange-Id: I{}", "a".repeat(40)),
                Profile::Local,
            ))
            .unwrap();
        assert!(!ok.iter().any(|f| f.code == "commitmsg/missing-trailer"));
    }

    #[test]
    fn forbidden_prefix_only_on_upload() {
        let local = CommitMsgLinter::new(CommitMsgConfig::default())
            .run(&job("WIP thing", Profile::Local))
            .unwrap();
        assert!(!local.iter().any(|f| f.code == "commitmsg/forbidden-prefix"));

        let upload = CommitMsgLinter::new(CommitMsgConfig::default())
            .run(&job("WIP thing", Profile::Upload))
            .unwrap();
        assert!(upload.iter().any(|f| f.code == "commitmsg/forbidden-prefix"));
    }
}
