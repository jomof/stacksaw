//! CLI command handlers (§10). Most operate in-process (no daemon) for
//! hermetic, CI-friendly runs; caching differs but semantics are identical
//! (§3.1).

use std::collections::HashMap;
use std::fs;

use serde_json::json;
use stacksaw_core::service::build_lint_jobs;
use stacksaw_git::refs::{self, git};
use stacksaw_git::{annotate_rebase, build_snapshot, edit, snapshot};
use stacksaw_lint::{apply_suggestion, collect_findings, default_builtins, Profile};
use stacksaw_ssp::types::{
    EditBegin, EditFinish, Finding, Rewrite, Severity, Staircase, Suggestion, SCHEMA_VERSION,
};

use crate::cli::*;
use crate::context::Ctx;
use crate::output::{print_json, print_json_error, print_jsonl, Format};

pub fn ls(ctx: &Ctx, fmt: Format) -> anyhow::Result<i32> {
    let repo = ctx.repo()?;
    let mut snap = build_snapshot(&repo, 0, &ctx.model_options())?;
    // One-shot command: probe the rebase-onto-upstream verdict synchronously
    // (the interactive TUI does this in the background instead).
    annotate_rebase(&repo, &mut snap.staircases);
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
    let repo = ctx.repo()?;
    let snap = build_snapshot(&repo, 0, &ctx.model_options())?;
    let dirty = repo
        .workdir()
        .and_then(|w| snapshot::is_worktree_dirty(&w).ok())
        .unwrap_or(false);
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
    let repo = ctx.repo()?;
    let oid = repo.resolve(rev)?;
    let meta = repo.commit_meta(oid)?;
    let findings = lint_commits(ctx, &[oid.to_string()], Profile::Local)?;
    let payload = json!({
        "oid": meta.oid.to_string(),
        "short": meta.short(),
        "subject": meta.subject,
        "body": meta.body,
        "author": meta.author_name,
        "authorEmail": meta.author_email,
        "changeId": meta.change_id,
        "parents": meta.parents.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        "findings": findings,
    });
    match fmt {
        Format::Json | Format::Jsonl => print_json(&payload),
        Format::Text => {
            println!("commit {}", meta.oid);
            println!("Author: {} <{}>", meta.author_name, meta.author_email);
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
    let out = git(&ctx.repo_root, &arg_refs)?;
    match fmt {
        Format::Json | Format::Jsonl => print_json(&json!({ "diff": out })),
        Format::Text => print!("{out}"),
    }
    Ok(if out.trim().is_empty() { 0 } else { 1 })
}

pub fn interdiff(ctx: &Ctx, a: &str, b: &str, fmt: Format) -> anyhow::Result<i32> {
    let out = git(&ctx.repo_root, &["range-diff", a, b])?;
    match fmt {
        Format::Json | Format::Jsonl => print_json(&json!({ "interdiff": out })),
        Format::Text => print!("{out}"),
    }
    Ok(0)
}

pub fn lint(ctx: &Ctx, args: &LintArgs, fmt: Format, yes: bool) -> anyhow::Result<i32> {
    let profile: Profile = args.profile.parse().unwrap_or(Profile::Local);
    let commits = resolve_scope(ctx, args)?;
    let findings = lint_commits(ctx, &commits, profile)?;

    if args.fix {
        for commit in &commits {
            let _ = fix_commit(ctx, commit, None, yes)?;
        }
        // Re-lint after fixing to report the residual.
        let after = lint_commits(ctx, &resolve_scope(ctx, args)?, profile)?;
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
    let repo = ctx.repo()?;
    let begin = edit::begin(&repo, commit)?;
    let payload = EditBegin {
        schema_version: SCHEMA_VERSION,
        token: begin.session.token,
        worktree: begin.session.worktree.display().to_string(),
        commit: begin.session.commit,
        descendants: begin.descendants,
    };
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
    let repo = ctx.repo()?;
    let message = match message_file {
        Some(f) => Some(fs::read_to_string(f)?),
        None => None,
    };
    let result = edit::finish(&repo, token, message.as_deref())?;
    let payload = EditFinish {
        schema_version: SCHEMA_VERSION,
        rewrites: result
            .rewrites
            .into_iter()
            .map(|(old, new)| Rewrite { old, new })
            .collect(),
        updated_refs: result.updated_refs,
        checkpoint: result.checkpoint,
    };
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
    let repo = ctx.repo()?;
    edit::abort(&repo, token)?;
    println!("aborted edit session {token}");
    Ok(0)
}

pub fn undo(ctx: &Ctx, checkpoint: Option<&str>, fmt: Format) -> anyhow::Result<i32> {
    let id = match checkpoint {
        Some(c) => c.to_string(),
        None => refs::list_checkpoints(&ctx.git_dir)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no checkpoints to undo"))?,
    };
    let restored = refs::restore_checkpoint(&ctx.git_dir, &id)?;
    match fmt {
        Format::Text => {
            println!("restored checkpoint {id}:");
            for r in &restored {
                println!("  {r}");
            }
        }
        _ => print_json(&json!({ "checkpoint": id, "restored": restored })),
    }
    Ok(0)
}

pub fn checkpoints_ls(ctx: &Ctx, fmt: Format) -> anyhow::Result<i32> {
    let ids = refs::list_checkpoints(&ctx.git_dir)?;
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
        let out = git(&ctx.repo_root, &["rev-list", "--reverse", r])?;
        return Ok(out.lines().map(str::to_string).collect());
    }
    let repo = ctx.repo()?;
    let snap = build_snapshot(&repo, 0, &ctx.model_options())?;
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

fn lint_commits(
    ctx: &Ctx,
    commits: &[String],
    profile: Profile,
) -> anyhow::Result<Vec<Finding>> {
    let repo = ctx.repo()?;
    let jobs = build_lint_jobs(&repo, &ctx.repo_root, commits, profile)?;
    let linters = default_builtins();
    let outcomes = stacksaw_lint::run(&jobs, &linters);
    let (findings, errors) = collect_findings(outcomes);
    for (id, e) in errors {
        print_json_error("linter-error", &format!("{id}: {e}"));
    }
    Ok(findings)
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
    let repo = ctx.repo()?;
    let oid = repo.resolve(commit)?;
    let findings = lint_commits(ctx, &[oid.to_string()], Profile::Local)?;

    let suggestions: Vec<_> = findings
        .iter()
        .filter(|f| linter.map_or(true, |l| f.source.ends_with(l)))
        .filter_map(|f| f.suggestion.clone())
        .collect();
    if suggestions.is_empty() {
        return Ok(json!({ "rewrites": [], "note": "no autofixable findings" }));
    }

    let begin = edit::begin(&repo, commit)?;
    let worktree = begin.session.worktree.clone();
    let token = begin.session.token.clone();

    // Load affected files from the worktree, apply, write back.
    let mut files: HashMap<String, String> = HashMap::new();
    for sug in &suggestions {
        for edit in &sug.edits {
            files.entry(edit.file.clone()).or_insert_with(|| {
                fs::read_to_string(worktree.join(&edit.file)).unwrap_or_default()
            });
        }
    }
    // Apply all edits in one batch against the original coordinate space, so
    // insertions from one suggestion don't shift ranges from another.
    let combined = Suggestion {
        edits: suggestions.iter().flat_map(|s| s.edits.clone()).collect(),
    };
    apply_suggestion(&mut files, &combined);
    for (path, content) in &files {
        fs::write(worktree.join(path), content)?;
    }

    let result = edit::finish(&repo, &token, None)?;
    Ok(json!({
        "rewrites": result.rewrites.iter().map(|(o, n)| json!([o, n])).collect::<Vec<_>>(),
        "updatedRefs": result.updated_refs,
        "checkpoint": result.checkpoint,
    }))
}
