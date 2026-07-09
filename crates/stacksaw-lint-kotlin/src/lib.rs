//! `ktfqn` — the reference tree-sitter Kotlin linter (§7.5).
//!
//! Flags Kotlin code that references a type or member by fully-qualified name
//! inline instead of importing it and using the short name. Parses with the
//! pinned `tree-sitter-kotlin` grammar; node names are validated by the
//! compile-time query check in `xtask lint-queries`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use stacksaw_ssp::types::{
    Edit, Finding, Location, Position, Range, Severity, Suggestion, SCHEMA_VERSION,
};

/// Default well-known-root set (§7.5 scope guard).
pub const DEFAULT_ROOTS: &[&str] = &[
    "com", "org", "net", "io", "java", "javax", "jakarta", "kotlin", "kotlinx", "android",
    "androidx", "dev", "edu", "gov",
];

/// `[lint.ktfqn]` configuration (§7.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KtfqnConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_roots")]
    pub roots: Vec<String>,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default = "default_severity")]
    pub severity: Severity,
    #[serde(default)]
    pub exclude: Vec<String>,
}

fn default_true() -> bool {
    true
}
fn default_roots() -> Vec<String> {
    DEFAULT_ROOTS.iter().map(|s| s.to_string()).collect()
}
fn default_scope() -> String {
    "diff".to_string()
}
fn default_severity() -> Severity {
    Severity::Warning
}

impl Default for KtfqnConfig {
    fn default() -> Self {
        KtfqnConfig {
            enabled: true,
            roots: default_roots(),
            scope: default_scope(),
            severity: Severity::Warning,
            exclude: Vec::new(),
        }
    }
}

/// The pinned Kotlin grammar.
pub fn language() -> tree_sitter::Language {
    tree_sitter_kotlin::language()
}

#[derive(Debug, thiserror::Error)]
pub enum KtfqnError {
    #[error("failed to set tree-sitter language: {0}")]
    Language(String),
    #[error("failed to parse source")]
    Parse,
}

/// Analyze one Kotlin source file and return findings (§7.5).
///
/// * `commit` — abbreviated commit oid to stamp on findings.
/// * `changed_lines` — when `Some` and `scope == "diff"`, only findings whose
///   start line is in the set are reported.
pub fn analyze(
    source: &str,
    commit: &str,
    file: &str,
    config: &KtfqnConfig,
    changed_lines: Option<&HashSet<u32>>,
) -> Result<Vec<Finding>, KtfqnError> {
    if !config.enabled || is_excluded(file, &config.exclude) {
        return Ok(vec![]);
    }

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(language())
        .map_err(|e| KtfqnError::Language(e.to_string()))?;
    let tree = parser.parse(source, None).ok_or(KtfqnError::Parse)?;
    let root = tree.root_node();
    let bytes = source.as_bytes();

    let roots: HashSet<&str> = config.roots.iter().map(String::as_str).collect();

    // Gather import/package context: imported simple-name → fqn (§7.5 fix rules).
    let mut imported: Vec<(String, String)> = Vec::new(); // (short, fqn)
    let mut declared_types: HashSet<String> = HashSet::new();
    let mut last_import_line: Option<u32> = None;
    let mut package_line: Option<u32> = None;

    collect_context(
        root,
        bytes,
        &mut imported,
        &mut declared_types,
        &mut last_import_line,
        &mut package_line,
    );

    let mut findings = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    collect_candidates(root, bytes, &mut candidates);

    for cand in candidates {
        let Some(flag) = evaluate(&cand, &roots) else {
            continue;
        };

        // scope = "diff": restrict to changed lines when provided.
        if config.scope == "diff" {
            if let Some(changed) = changed_lines {
                if !changed.contains(&(flag.range.start.line)) {
                    continue;
                }
            }
        }

        // Disambiguation downgrade (§7.5): the short name is already imported
        // from a *different* package, or declared in-file → info, no autofix.
        let short = &flag.short;
        let conflict = imported
            .iter()
            .any(|(s, fqn)| s == short && fqn != &flag.fqn)
            || declared_types.contains(short);

        let (severity, suggestion, tags) = if conflict {
            (Severity::Info, None, vec![])
        } else {
            let import_after = last_import_line.or(package_line).unwrap_or(0);
            let suggestion = Suggestion {
                edits: vec![
                    Edit {
                        file: file.to_string(),
                        range: Some(flag.range),
                        insert_after_line: None,
                        new_text: short.clone(),
                    },
                    Edit {
                        file: file.to_string(),
                        range: None,
                        insert_after_line: Some(import_after),
                        new_text: format!("import {}", flag.fqn),
                    },
                ],
            };
            (
                config.severity,
                Some(suggestion),
                vec!["autofixable".to_string()],
            )
        };

        findings.push(Finding {
            schema_version: SCHEMA_VERSION,
            source: "linter:ktfqn".to_string(),
            code: "ktfqn/avoid-fqn".to_string(),
            severity,
            commit: commit.to_string(),
            location: Location::file_range(file, flag.range),
            message: format!("Use an import for {}", flag.fqn),
            suggestion,
            tags,
        });
    }

    Ok(findings)
}

struct Candidate {
    /// Ordered dotted segments with their byte offsets and start/end points.
    segments: Vec<Segment>,
}

struct Segment {
    text: String,
    start: tree_sitter::Point,
    end: tree_sitter::Point,
}

struct Flagged {
    fqn: String,
    short: String,
    range: Range,
}

/// Apply the scope guard and compute the fix target (§7.5).
fn evaluate(cand: &Candidate, roots: &HashSet<&str>) -> Option<Flagged> {
    let segs = &cand.segments;
    if segs.len() < 2 {
        return None;
    }
    // First segment must be a well-known root.
    if !roots.contains(segs[0].text.as_str()) {
        return None;
    }
    // Some segment must begin with an uppercase letter (heuristic "reaches a
    // type"). The first such segment is the type name.
    let type_index = segs
        .iter()
        .position(|s| s.text.chars().next().is_some_and(|c| c.is_uppercase()))?;
    if type_index == 0 {
        return None; // a root can't be a type; guards ordinary chains
    }

    let fqn = segs[..=type_index]
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(".");
    let short = segs[type_index].text.clone();
    let range = Range {
        start: point_to_pos(segs[0].start),
        end: point_to_pos(segs[type_index].end),
    };
    Some(Flagged { fqn, short, range })
}

fn point_to_pos(p: tree_sitter::Point) -> Position {
    Position {
        line: (p.row as u32) + 1,
        col: (p.column as u32) + 1,
    }
}

/// Walk the tree collecting import/package context.
fn collect_context(
    node: tree_sitter::Node,
    bytes: &[u8],
    imported: &mut Vec<(String, String)>,
    declared: &mut HashSet<String>,
    last_import_line: &mut Option<u32>,
    package_line: &mut Option<u32>,
) {
    match node.kind() {
        "import_header" => {
            if let Some(id) = child_of_kind(node, "identifier") {
                let fqn = text_of(id, bytes);
                let short = fqn.rsplit('.').next().unwrap_or(&fqn).to_string();
                imported.push((short, fqn));
            }
            let line = (node.start_position().row as u32) + 1;
            *last_import_line = Some(last_import_line.map_or(line, |l| l.max(line)));
            return;
        }
        "package_header" => {
            *package_line = Some((node.start_position().row as u32) + 1);
            return;
        }
        "class_declaration" | "object_declaration" | "interface_declaration" => {
            if let Some(id) = child_of_kind(node, "type_identifier")
                .or_else(|| child_of_kind(node, "simple_identifier"))
            {
                declared.insert(text_of(id, bytes));
            }
        }
        _ => {}
    }
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        collect_context(
            ch,
            bytes,
            imported,
            declared,
            last_import_line,
            package_line,
        );
    }
}

/// Walk the tree collecting root-most candidate dotted chains.
fn collect_candidates(node: tree_sitter::Node, bytes: &[u8], out: &mut Vec<Candidate>) {
    // Skip anything inside imports, packages, or comments (§7.5 immunity).
    let kind = node.kind();
    if kind == "import_header" || kind == "package_header" || kind.contains("comment") {
        return;
    }

    match kind {
        "navigation_expression" => {
            // Only the root-most navigation_expression (parent is not one).
            let is_root = node
                .parent()
                .map(|p| p.kind() != "navigation_expression")
                .unwrap_or(true);
            if is_root {
                if let Some(cand) = nav_segments(node, bytes) {
                    out.push(cand);
                }
            }
            // Do not descend: nested nav-expressions are part of this chain.
            return;
        }
        "user_type" => {
            if let Some(cand) = user_type_segments(node, bytes) {
                out.push(cand);
            }
            // Descend into type_arguments for nested user_types (e.g. Map<a.b.C>).
        }
        _ => {}
    }

    let mut c = node.walk();
    for ch in node.children(&mut c) {
        collect_candidates(ch, bytes, out);
    }
}

/// Reconstruct a dotted chain from an expression `navigation_expression`.
fn nav_segments(node: tree_sitter::Node, bytes: &[u8]) -> Option<Candidate> {
    // Leftmost descent yields a base simple_identifier; navigation_suffix nodes
    // contribute the trailing segments.
    let mut segments = Vec::new();
    build_nav(node, bytes, &mut segments);
    if segments.is_empty() {
        None
    } else {
        Some(Candidate { segments })
    }
}

fn build_nav(node: tree_sitter::Node, bytes: &[u8], segs: &mut Vec<Segment>) {
    match node.kind() {
        "navigation_expression" => {
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                build_nav(ch, bytes, segs);
            }
        }
        "navigation_suffix" => {
            if let Some(id) = child_of_kind(node, "simple_identifier") {
                push_seg(id, bytes, segs);
            } else {
                // Non-identifier suffix (e.g. `.class`, index) → invalidate.
                segs.clear();
            }
        }
        "simple_identifier" => push_seg(node, bytes, segs),
        // A base that isn't a plain identifier chain (call, literal) → not FQN.
        "." => {}
        _ => {
            segs.clear();
        }
    }
}

/// Reconstruct a dotted chain from a type-position `user_type`.
fn user_type_segments(node: tree_sitter::Node, bytes: &[u8]) -> Option<Candidate> {
    let mut segments = Vec::new();
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        match ch.kind() {
            "type_identifier" => push_seg(ch, bytes, &mut segments),
            "." => {}
            // Stop at type arguments / the end of the qualified name.
            "type_arguments" => break,
            _ => {}
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(Candidate { segments })
    }
}

fn push_seg(node: tree_sitter::Node, bytes: &[u8], segs: &mut Vec<Segment>) {
    segs.push(Segment {
        text: text_of(node, bytes),
        start: node.start_position(),
        end: node.end_position(),
    });
}

fn child_of_kind<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
    let mut c = node.walk();
    let found = node.children(&mut c).find(|ch| ch.kind() == kind);
    found
}

fn text_of(node: tree_sitter::Node, bytes: &[u8]) -> String {
    node.utf8_text(bytes).unwrap_or("").to_string()
}

/// Minimal glob exclusion supporting the common `**/dir/**` and `*.ext` forms.
fn is_excluded(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pat| glob_match(pat, path))
}

fn glob_match(pattern: &str, path: &str) -> bool {
    if let Some(mid) = pattern
        .strip_prefix("**/")
        .and_then(|p| p.strip_suffix("/**"))
    {
        return path.contains(&format!("/{mid}/")) || path.starts_with(&format!("{mid}/"));
    }
    if let Some(ext) = pattern.strip_prefix("*.") {
        return path.ends_with(&format!(".{ext}"));
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path.starts_with(&format!("{prefix}/")) || path == prefix;
    }
    pattern == path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(source: &str) -> Vec<Finding> {
        analyze(source, "abc123", "Main.kt", &KtfqnConfig::default(), None).unwrap()
    }

    #[test]
    fn flags_fqn_in_call_chain() {
        let f = one("fun x() { com.foo.Bar.baz() }");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, "ktfqn/avoid-fqn");
        assert!(f[0].message.contains("com.foo.Bar"));
        assert!(f[0].is_autofixable());
    }

    #[test]
    fn flags_fqn_in_type_position() {
        let f = one("val m: java.util.concurrent.ConcurrentHashMap<String, Int>? = null");
        assert_eq!(f.len(), 1, "one finding for the type-position FQN");
        assert!(f[0]
            .message
            .contains("java.util.concurrent.ConcurrentHashMap"));
        let sug = f[0].suggestion.as_ref().unwrap();
        assert_eq!(sug.edits.len(), 2);
        assert_eq!(sug.edits[0].new_text, "ConcurrentHashMap");
        assert_eq!(
            sug.edits[1].new_text,
            "import java.util.concurrent.ConcurrentHashMap"
        );
    }

    #[test]
    fn import_header_is_immune() {
        let f = one("import java.util.List\nclass A");
        assert!(f.is_empty(), "imports must never be flagged");
    }

    #[test]
    fn package_header_is_immune() {
        let f = one("package com.example.Foo");
        assert!(f.is_empty());
    }

    #[test]
    fn receiver_chain_is_immune() {
        // lowercase-only, non-root first segment: an ordinary receiver chain.
        let f = one("fun x() { config.build.flavor }");
        assert!(f.is_empty(), "lowercase receiver chains never fire");
    }

    #[test]
    fn root_first_but_no_type_is_immune() {
        // com is a root but no uppercase segment → not reaching a type.
        let f = one("fun x() { com.foo.bar() }");
        assert!(f.is_empty());
    }

    #[test]
    fn kdoc_is_immune() {
        let f = one("/** See [com.foo.Bar] */\nclass A");
        assert!(f.is_empty(), "KDoc references are immune");
    }

    #[test]
    fn conflict_downgrades_to_info() {
        // Bar already imported from a different package → info, no autofix.
        let src = "import other.Bar\nfun x() { com.foo.Bar.baz() }";
        let f = one(src);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Info);
        assert!(f[0].suggestion.is_none());
    }

    #[test]
    fn autofix_is_idempotent() {
        // After applying the fix, the short-name form must not re-flag.
        let fixed = "import java.util.concurrent.ConcurrentHashMap\nval m: ConcurrentHashMap<String, Int>? = null";
        let f = one(fixed);
        assert!(f.is_empty(), "fixed code must not re-trigger");
    }

    #[test]
    fn diff_scope_restricts_to_changed_lines() {
        let src = "fun x() { com.foo.Bar.baz() }";
        let changed: HashSet<u32> = HashSet::new(); // nothing changed
        let f = analyze(src, "c", "Main.kt", &KtfqnConfig::default(), Some(&changed)).unwrap();
        assert!(
            f.is_empty(),
            "unchanged lines are not reported in diff scope"
        );
    }

    #[test]
    fn excluded_glob_skips_file() {
        let cfg = KtfqnConfig {
            exclude: vec!["**/generated/**".into()],
            ..Default::default()
        };
        let f = analyze(
            "fun x() { com.foo.Bar.baz() }",
            "c",
            "app/generated/Api.kt",
            &cfg,
            None,
        )
        .unwrap();
        assert!(f.is_empty());
    }
}
