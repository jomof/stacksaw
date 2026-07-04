//! Built-in `copyright` linter (§7.5).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use stacksaw_ssp::types::{Edit, Finding, Location, Severity, Suggestion, SCHEMA_VERSION};

use crate::linter::{FileChange, LintError, LintJob, Linter};

/// Which files the linter applies to (§7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CopyrightMode {
    /// Only files added in the commit (default).
    Added,
    /// Files added or modified.
    Touched,
    /// Every file in the commit's diff.
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyrightConfig {
    /// Header template; `{year}` and `{holder}` are substituted.
    #[serde(default = "d_template")]
    pub template: String,
    #[serde(default = "d_holder")]
    pub holder: String,
    #[serde(default = "d_mode")]
    pub mode: CopyrightMode,
    /// Grace window (years) for `{year}` validation against author year.
    #[serde(default = "d_grace")]
    pub year_grace: i32,
    /// Comment style per file extension, e.g. `kt = "//"`.
    #[serde(default = "d_styles")]
    pub comment_styles: HashMap<String, String>,
}

fn d_template() -> String {
    "Copyright (c) {year} {holder}".to_string()
}
fn d_holder() -> String {
    "The Authors".to_string()
}
fn d_mode() -> CopyrightMode {
    CopyrightMode::Added
}
fn d_grace() -> i32 {
    1
}
fn d_styles() -> HashMap<String, String> {
    let mut m = HashMap::new();
    for ext in ["rs", "kt", "kts", "java", "js", "ts", "go", "c", "cpp", "h"] {
        m.insert(ext.to_string(), "//".to_string());
    }
    for ext in ["py", "sh", "toml", "yaml", "yml"] {
        m.insert(ext.to_string(), "#".to_string());
    }
    m
}

impl Default for CopyrightConfig {
    fn default() -> Self {
        CopyrightConfig {
            template: d_template(),
            holder: d_holder(),
            mode: d_mode(),
            year_grace: d_grace(),
            comment_styles: d_styles(),
        }
    }
}

pub struct CopyrightLinter {
    config: CopyrightConfig,
}

impl CopyrightLinter {
    pub fn new(config: CopyrightConfig) -> Self {
        CopyrightLinter { config }
    }

    fn applies(&self, file: &FileChange) -> bool {
        match self.config.mode {
            CopyrightMode::Added => file.added,
            CopyrightMode::Touched => true,
            CopyrightMode::All => true,
        }
    }

    fn comment_prefix(&self, path: &str) -> Option<&str> {
        let ext = path.rsplit('.').next()?;
        self.config.comment_styles.get(ext).map(String::as_str)
    }
}

impl Linter for CopyrightLinter {
    fn id(&self) -> &str {
        "copyright"
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
            if !self.applies(file) {
                continue;
            }
            let Some(prefix) = self.comment_prefix(&file.path) else {
                continue; // unknown file type: no policy
            };
            let Some(content) = &file.content else {
                continue;
            };

            let expected_body = self
                .config
                .template
                .replace("{year}", &job.author_year.to_string())
                .replace("{holder}", &self.config.holder);
            let header_line = format!("{prefix} {expected_body}");

            // Look for any existing copyright header in the first few lines.
            let head: Vec<&str> = content.lines().take(10).collect();
            let existing = head.iter().find(|l| l.to_lowercase().contains("copyright"));

            match existing {
                Some(line) => {
                    // Validate the year is within the grace window.
                    if let Some(year) = extract_year(line) {
                        let diff = (job.author_year - year).abs();
                        if diff > self.config.year_grace {
                            findings.push(finding(
                                &job.commit,
                                &file.path,
                                Severity::Warning,
                                "stale-year",
                                format!(
                                    "Copyright year {year} differs from author year {} by more than {}",
                                    job.author_year, self.config.year_grace
                                ),
                                None,
                            ));
                        }
                    }
                }
                None => {
                    // Insert after shebang / license-guard lines (§7.5).
                    let insert_after = header_insert_line(content, prefix);
                    let suggestion = Suggestion {
                        edits: vec![Edit {
                            file: file.path.clone(),
                            range: None,
                            insert_after_line: Some(insert_after),
                            new_text: header_line.clone(),
                        }],
                    };
                    findings.push(finding(
                        &job.commit,
                        &file.path,
                        Severity::Warning,
                        "missing-header",
                        format!("Missing copyright header: {header_line:?}"),
                        Some(suggestion),
                    ));
                }
            }
        }
        Ok(findings)
    }
}

fn finding(
    commit: &str,
    path: &str,
    sev: Severity,
    code: &str,
    msg: String,
    suggestion: Option<Suggestion>,
) -> Finding {
    let autofix = suggestion.is_some();
    Finding {
        schema_version: SCHEMA_VERSION,
        source: "linter:copyright".into(),
        code: format!("copyright/{code}"),
        severity: sev,
        commit: commit.into(),
        location: Location {
            file: Some(path.into()),
            range: None,
            message_line: None,
        },
        message: msg,
        suggestion,
        tags: if autofix {
            vec!["autofixable".into()]
        } else {
            vec![]
        },
    }
}

/// Find the line after which a header should be inserted: after a shebang or a
/// license guard `//!`/`/*` block start.
fn header_insert_line(content: &str, _prefix: &str) -> u32 {
    let mut after = 0u32;
    for (i, line) in content.lines().enumerate() {
        let t = line.trim_start();
        if i == 0 && t.starts_with("#!") {
            after = 1;
        } else if t.starts_with("@file:") {
            after = (i as u32) + 1;
        } else {
            break;
        }
    }
    after
}

fn extract_year(line: &str) -> Option<i32> {
    // First 4-digit substring in range. Scan runs of ASCII digits so we never
    // slice across a UTF-8 char boundary (lines may contain multi-byte chars).
    let mut run = String::new();
    for ch in line.chars().chain(std::iter::once('\0')) {
        if ch.is_ascii_digit() {
            run.push(ch);
            continue;
        }
        if run.len() >= 4 {
            for start in 0..=run.len() - 4 {
                if let Ok(y) = run[start..start + 4].parse::<i32>() {
                    if (1970..=2200).contains(&y) {
                        return Some(y);
                    }
                }
            }
        }
        run.clear();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn job(files: Vec<FileChange>, year: i32) -> LintJob {
        LintJob {
            commit: "abc".into(),
            author_year: year,
            message: String::new(),
            files,
            repo_root: PathBuf::from("/tmp"),
            worktree: PathBuf::from("/tmp"),
            profile: Default::default(),
        }
    }

    fn added(path: &str, content: &str) -> FileChange {
        FileChange {
            path: path.into(),
            old_oid: None,
            new_oid: Some("x".into()),
            changed_ranges: vec![],
            content: Some(content.into()),
            added: true,
        }
    }

    #[test]
    fn flags_missing_header_and_suggests_fix() {
        let f = CopyrightLinter::new(CopyrightConfig::default())
            .run(&job(vec![added("A.kt", "class A\n")], 2026))
            .unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, "copyright/missing-header");
        let s = f[0].suggestion.as_ref().unwrap();
        assert!(s.edits[0].new_text.contains("2026"));
        assert!(s.edits[0].new_text.starts_with("//"));
    }

    #[test]
    fn present_header_within_grace_is_ok() {
        let c = "// Copyright (c) 2026 The Authors\nclass A\n";
        let f = CopyrightLinter::new(CopyrightConfig::default())
            .run(&job(vec![added("A.kt", c)], 2026))
            .unwrap();
        assert!(f.is_empty());
    }

    #[test]
    fn stale_year_flagged() {
        let c = "// Copyright (c) 2010 The Authors\nclass A\n";
        let f = CopyrightLinter::new(CopyrightConfig::default())
            .run(&job(vec![added("A.kt", c)], 2026))
            .unwrap();
        assert!(f.iter().any(|f| f.code == "copyright/stale-year"));
    }

    #[test]
    fn only_added_files_by_default() {
        let mut fc = added("A.kt", "class A\n");
        fc.added = false; // modified, not added
        let f = CopyrightLinter::new(CopyrightConfig::default())
            .run(&job(vec![fc], 2026))
            .unwrap();
        assert!(f.is_empty(), "default mode only checks added files");
    }
}
