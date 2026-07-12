//! CLI command handlers (§10). All repo reads and writes go through the
//! semantic [`Core`] handle (in-process or attached daemon).

use std::fs;
use serde_json::json;

use crate::cli::*;
use crate::context::Ctx;
use crate::output::{print_json, Format};

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
    let payload = json!({
        "oid": meta.oid,
        "short": meta.short,
        "subject": meta.subject,
        "body": meta.body,
        "author": meta.author,
        "authorEmail": meta.author_email,
        "changeId": meta.change_id,
        "parents": meta.parents,
    });
    match fmt {
        Format::Json | Format::Jsonl => print_json(&payload),
        Format::Text => {
            println!("commit {}", meta.oid);
            println!("Author: {} <{}>", meta.author, meta.author_email);
            println!("\n    {}\n", meta.subject);
            if !meta.body.is_empty() {
                println!("    {}\n", meta.body.replace('\n', "\n    "));
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
        _ => {
            print_json(&json!({ "checkpoint": result.checkpoint, "generation": result.generation }))
        }
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

