//! CLI command handlers (§10). All repo reads and writes go through the
//! semantic [`Core`] handle (in-process or attached daemon).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde_json::json;
use stacksaw_lint::{apply_suggestion, Profile};
use stacksaw_ssp::types::{Finding, Severity, Staircase, Suggestion};

use crate::cli::*;
use crate::context::Ctx;
use crate::output::{print_json, print_jsonl, Format};

pub fn ls(ctx: &Ctx, fmt: Format) -> anyhow::Result<i32> {
    let snap = ctx.block_on(ctx.core().snapshot())?;
    match fmt {
        Format::Json | Format::Jsonl => print_json(&json!({ "staircases": snap.staircases })),
        Format::Text => {
            if snap.staircases.is_empty() {
                println!("No staircases (no branches with an upstream).");
            }
            for s in &snap.staircases {
                let dirty = if s.dirty { " ✎" } else { "" };
                println!(
                    "● {}  ↑{} ↓{}{}  (upstream {})",
                    s.name, s.ahead, s.behind, dirty, s.upstream
                );
                for (i, seg) in s.segments.iter().enumerate() {
                    println!("  step {} ─ {}", i + 1, seg.branch);
                    for c in &seg.commits {
                        println!("     {} {}", c.short, c.subject);
                    }
                }
            }
        }
    }
    Ok(0)
}

pub fn status(ctx: &Ctx, fmt: Format) -> anyhow::Result<i32> {
    let snap = ctx.block_on(ctx.core().snapshot())?;
    let dirty = ctx.block_on(ctx.core().worktree_dirty())?;
    let payload = json!({
        "head": snap.head,
        "detached": snap.detached,
        "dirty": dirty,
        "staircases": snap.staircases.iter().map(|s| json!({
            "name": s.name, "ahead": s.ahead, "behind": s.behind, "dirty": s.dirty
        })).collect::<Vec<_>>()
    });
    match fmt {
        Format::Json | Format::Jsonl => print_json(&payload),
        Format::Text => {
            println!(
                "HEAD: {}{}",
                snap.head.as_deref().unwrap_or("(unborn)"),
                if snap.detached { " (detached)" } else { "" }
            );
            println!("worktree: {}", if dirty { "dirty" } else { "clean" });
            for s in &snap.staircases {
                println!("  {}  ↑{} ↓{}", s.name, s.ahead, s.behind);
            }
        }
    }
    Ok(0)
}

pub fn show(ctx: &Ctx, rev: &str, fmt: Format) -> anyhow::Result<i32> {
    let meta = ctx.block_on(ctx.core().commit_show(rev))?;
    let findings = ctx.block_on(ctx.core().lint(vec![meta.oid.clone()], Profile::Local))?;
    let payload = json!({
        "oid": meta.oid,
        "short": meta.short,
        "subject": meta.subject,
        "body": meta.body,
        "author": meta.author,
        "authorEmail": meta.author_email,
        "changeId": meta.change_id,
        "parents": meta.parents,
        "findings": findings,
    });
    match fmt {
        Format::Json | Format::Jsonl => print_json(&payload),
        Format::Text => {
            println!("commit {}", meta.oid);
            println!("Author: {} <{}>", meta.author, meta.author_email);
            println!("\n    {}\n", meta.subject);
            if !meta.body.is_empty() {
                for l in meta.body.lines() {
                    println!("    {l}");
                }
            }
            if !findings.is_empty() {
                println!("\nFindings:");
                for f in &findings {
                    println!("  {} [{}] {}", f.severity.glyph(), f.code, f.message);
                }
            }
        }
    }
    Ok(0)
}

pub fn diff(ctx: &Ctx, args: &Command, fmt: Format) -> anyhow::Result<i32> {
    let Command::Diff {
        range,
        patch,
        name_only,
    } = args
    else {
        unreachable!()
    };
    let mut git_args = vec!["diff".to_string()];
    if *name_only {
        git_args.push("--name-only".into());
    } else if *patch || fmt == Format::Text {
        // default to patch
    }
    if let Some(r) = range {
        git_args.push(r.clone());
    }
    let arg_refs: Vec<&str> = git_args.iter().map(String::as_str).collect();
    let out = ctx.block_on(ctx.core().diff_range(&arg_refs))?;
    match fmt {
        Format::Json | Format::Jsonl => print_json(&json!({ "diff": out })),
        Format::Text => print!("{out}"),
    }
    Ok(if out.trim().is_empty() { 0 } else { 1 })
}

pub fn interdiff(ctx: &Ctx, a: &str, b: &str, fmt: Format) -> anyhow::Result<i32> {
    let out = ctx.block_on(ctx.core().diff_interdiff(a, b))?;
    match fmt {
        Format::Json | Format::Jsonl => print_json(&json!({ "interdiff": out })),
        Format::Text => print!("{out}"),
    }
    Ok(0)
}

pub fn lint(ctx: &Ctx, args: &LintArgs, fmt: Format, yes: bool) -> anyhow::Result<i32> {
    let profile: Profile = args.profile.parse().unwrap_or(Profile::Local);
    let commits = resolve_scope(ctx, args)?;
    let findings = ctx.block_on(ctx.core().lint(commits.clone(), profile))?;

    if args.fix {
        for commit in &commits {
            let _ = fix_commit(ctx, commit, None, yes)?;
        }
        let after = ctx.block_on(ctx.core().lint(resolve_scope(ctx, args)?, profile))?;
        emit_findings(&after, fmt);
        return Ok(exit_for_findings(&after, args.fail_on.as_deref()));
    }

    emit_findings(&findings, fmt);
    Ok(exit_for_findings(&findings, args.fail_on.as_deref()))
}

pub fn fix(ctx: &Ctx, args: &FixArgs, fmt: Format, yes: bool) -> anyhow::Result<i32> {
    let result = fix_commit(ctx, &args.commit, args.linter.as_deref(), yes)?;
    match fmt {
        Format::Json | Format::Jsonl => print_json(&result),
        Format::Text => {
            println!("Applied fixes to {}:", args.commit);
            if let Some(rw) = result.get("rewrites").and_then(|r| r.as_array()) {
                for r in rw {
                    println!(
                        "  {} → {}",
                        r[0].as_str().unwrap_or(""),
                        r[1].as_str().unwrap_or("")
                    );
                }
            }
        }
    }
    Ok(0)
}

pub fn edit_begin(ctx: &Ctx, commit: &str, fmt: Format) -> anyhow::Result<i32> {
    let payload = ctx.block_on(ctx.core().edit_begin(commit))?;
    match fmt {
        Format::Text => println!(
            "edit session {} at {} ({} descendants)",
            payload.token, payload.worktree, payload.descendants
        ),
        _ => print_json(&payload),
    }
    Ok(0)
}

pub fn edit_finish(
    ctx: &Ctx,
    token: &str,
    message_file: Option<&str>,
    fmt: Format,
) -> anyhow::Result<i32> {
    let message = match message_file {
        Some(f) => Some(fs::read_to_string(f)?),
        None => None,
    };
    let result = ctx.block_on(ctx.core().edit_finish(token, message.as_deref()))?;
    let payload = result;
    match fmt {
        Format::Text => {
            println!("checkpoint {}", payload.checkpoint);
            for r in &payload.rewrites {
                println!("  {} → {}", r.old, r.new);
            }
        }
        _ => print_json(&payload),
    }
    Ok(0)
}

pub fn edit_abort(ctx: &Ctx, token: &str) -> anyhow::Result<i32> {
    ctx.block_on(ctx.core().edit_abort(token))?;
    println!("aborted edit session {token}");
    Ok(0)
}

pub fn undo(ctx: &Ctx, checkpoint: Option<&str>, fmt: Format) -> anyhow::Result<i32> {
    let result = ctx.block_on(ctx.core().undo(checkpoint))?;
    match fmt {
        Format::Text => {
            println!("restored checkpoint {}", result.checkpoint);
        }
        _ => print_json(&json!({ "checkpoint": result.checkpoint, "generation": result.generation })),
    }
    Ok(0)
}

pub fn checkpoints_ls(ctx: &Ctx, fmt: Format) -> anyhow::Result<i32> {
    let ids = ctx.block_on(ctx.core().checkpoints_list())?;
    match fmt {
        Format::Text => {
            for id in &ids {
                println!("{id}");
            }
        }
        _ => print_json(&json!({ "checkpoints": ids })),
    }
    Ok(0)
}

// --- helpers ---

fn resolve_scope(ctx: &Ctx, args: &LintArgs) -> anyhow::Result<Vec<String>> {
    if let Some(c) = &args.commit {
        return Ok(vec![c.clone()]);
    }
    if let Some(r) = &args.range {
        let out = ctx.block_on(ctx.core().diff_range(&["rev-list", "--reverse", r]))?;
        return Ok(out.lines().map(str::to_string).collect());
    }
    let snap = ctx.block_on(ctx.core().snapshot())?;
    let pick = |s: &Staircase| -> Vec<String> {
        s.segments
            .iter()
            .flat_map(|seg| seg.commits.iter().map(|c| c.oid.clone()))
            .collect()
    };
    if let Some(name) = &args.stair {
        return Ok(snap
            .staircases
            .iter()
            .find(|s| &s.name == name)
            .map(pick)
            .unwrap_or_default());
    }
    if args.all {
        return Ok(snap.staircases.iter().flat_map(pick).collect());
    }
    Ok(snap.staircases.first().map(pick).unwrap_or_default())
}

fn emit_findings(findings: &[Finding], fmt: Format) {
    match fmt {
        Format::Json => print_json(&json!({ "findings": findings })),
        Format::Jsonl => print_jsonl(findings),
        Format::Text => {
            if findings.is_empty() {
                println!("No findings.");
            }
            for f in findings {
                let loc = f
                    .location
                    .file
                    .as_deref()
                    .map(|file| {
                        let line = f.location.range.map(|r| r.start.line).unwrap_or(0);
                        format!("{file}:{line}")
                    })
                    .or_else(|| f.location.message_line.map(|l| format!("commit-msg:{l}")))
                    .unwrap_or_default();
                println!(
                    "{} {} [{}] {} {}",
                    f.severity.glyph(),
                    &f.commit,
                    f.code,
                    loc,
                    f.message
                );
            }
        }
    }
}

fn exit_for_findings(findings: &[Finding], fail_on: Option<&str>) -> i32 {
    let threshold = match fail_on {
        Some("error") => Some(Severity::Error),
        Some("warning") => Some(Severity::Warning),
        Some("info") => Some(Severity::Info),
        _ => None,
    };
    let triggered = match threshold {
        Some(t) => findings.iter().any(|f| f.severity >= t),
        None => !findings.is_empty(),
    };
    if triggered {
        1
    } else {
        0
    }
}

/// Autofix a commit through an edit session: begin → apply suggestions in the
/// scratch worktree → finish (§10.2, §8.5 apply path).
fn fix_commit(
    ctx: &Ctx,
    commit: &str,
    linter: Option<&str>,
    _yes: bool,
) -> anyhow::Result<serde_json::Value> {
    let meta = ctx.block_on(ctx.core().commit_show(commit))?;
    let findings = ctx.block_on(ctx.core().lint(vec![meta.oid.clone()], Profile::Local))?;

    let suggestions: Vec<_> = findings
        .iter()
        .filter(|f| linter.map_or(true, |l| f.source.ends_with(l)))
        .filter_map(|f| f.suggestion.clone())
        .collect();
    if suggestions.is_empty() {
        return Ok(json!({ "rewrites": [], "note": "no autofixable findings" }));
    }

    let begin = ctx.block_on(ctx.core().edit_begin(commit))?;
    let worktree = PathBuf::from(&begin.worktree);
    let token = begin.token.clone();

    let mut files: HashMap<String, String> = HashMap::new();
    for sug in &suggestions {
        for edit in &sug.edits {
            files.entry(edit.file.clone()).or_insert_with(|| {
                fs::read_to_string(worktree.join(&edit.file)).unwrap_or_default()
            });
        }
    }
    let combined = Suggestion {
        edits: suggestions.iter().flat_map(|s| s.edits.clone()).collect(),
    };
    apply_suggestion(&mut files, &combined);
    for (path, content) in &files {
        fs::write(worktree.join(path), content)?;
    }

    let result = ctx.block_on(ctx.core().edit_finish(&token, None))?;
    Ok(json!({
        "rewrites": result.rewrites.iter().map(|r| json!([&r.old, &r.new])).collect::<Vec<_>>(),
        "updatedRefs": result.updated_refs,
        "checkpoint": result.checkpoint,
    }))
}
