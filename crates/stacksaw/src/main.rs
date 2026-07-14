//! `stacksaw` — one binary, three faces (§1, §3): a TUI, a per-repo core
//! service, and a scriptable CLI. This entry point dispatches by role.

mod cli;
mod commands;
mod context;
mod output;
mod runner;
mod schema;
mod tui;

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::process;

use clap::{CommandFactory, Parser};
use cli::{Cli, Command};
use context::Ctx;
use output::Format;
use stacksaw_core::config;
use stacksaw_core::recent::RecentStore;
use stacksaw_ssp::types::MutatePlan;
use stacksaw_ssp::method::ClientKind;
use tracing::{info, subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::EnvFilter;

fn main() {
    let cli = Cli::parse();
    let log_path = cli
        .log_file
        .clone()
        .or_else(|| Some(env::temp_dir().join("stacksaw.debug.log")));
    let _guard = init_logging(log_path.as_deref());
    info!("stacksaw started");
    let fmt: Format = cli.output.into();
    let code = run(cli, fmt);
    if code != 0 {
        process::exit(code);
    }
}

/// Initialize file logging, but only when the user opts in via `--log-file`
/// (or `STACKSAW_LOG_FILE`). Returns the appender's flush guard, which must be
/// held for the process lifetime. Verbosity honours `RUST_LOG`, defaulting to
/// `debug` for the requested log file.
fn init_logging(log_file: Option<&Path>) -> Option<WorkerGuard> {
    let path = log_file?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Start each run from a clean log so stale cycles don't confuse a session.
    let _ = OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(path);
    let file_name = path.file_name()?;
    let directory = path.parent().filter(|p| !p.as_os_str().is_empty());
    let file_appender = rolling::never(directory.unwrap_or_else(|| Path::new(".")), file_name);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(filter)
        .with_ansi(false)
        .finish();

    subscriber::set_global_default(subscriber).ok()?;
    Some(guard)
}

/// Open the most-recently-used repo from the MRU, skipping any entry that no
/// longer resolves to a git repo. Used as a fallback when `stacksaw` is launched
/// outside a repository. Returns `None` when the MRU is empty or all stale.
fn open_most_recent(upstream: Option<String>) -> Option<Ctx> {
    RecentStore::load()
        .repos
        .iter()
        .find_map(|r| Ctx::open_at(&r.path, upstream.clone(), ClientKind::Ui).ok())
}

fn run(cli: Cli, fmt: Format) -> i32 {
    // No subcommand → open a UI window (§3). Launched outside a git repo, fall
    // back to the most-recently-used repo so the window still opens somewhere
    // useful; only error if there's nothing to fall back to.
    let Some(command) = &cli.command else {
        let upstream = cli.upstream.clone();
        let ctx = Ctx::open(cli.upstream.clone())
            .or_else(|e| open_most_recent(cli.upstream.clone()).ok_or(e));
        return match ctx.and_then(|ctx| tui::run(ctx, upstream)) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("stacksaw: {e:#}");
                3
            }
        };
    };

    // Commands that never need a repo context.
    match command {
        Command::Schema { name } => return schema::print(name.as_deref()),
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(*shell, &mut cmd, "stacksaw", &mut io::stdout());
            return 0;
        }
        _ => {}
    }

    let ctx = match Ctx::open(cli.upstream.clone()) {
        Ok(c) => c,
        Err(e) => {
            if fmt == Format::Json {
                output::print_json_error("repo", &e.to_string());
            } else {
                eprintln!("stacksaw: {e:#}");
            }
            return 3;
        }
    };

    let result: anyhow::Result<i32> = match command {
        Command::Ls => commands::ls(&ctx, fmt),
        Command::Status => commands::status(&ctx, fmt),
        Command::Show { rev } => commands::show(&ctx, rev, fmt),
        Command::Diff { .. } => commands::diff(&ctx, command, fmt),
        Command::Interdiff { ref_a, ref_b } => commands::interdiff(&ctx, ref_a, ref_b, fmt),
        Command::Restack(args) => restack(&ctx, args, fmt),
        Command::Edit { op } => match op {
            cli::EditOp::Begin { commit } => commands::edit_begin(&ctx, commit, fmt),
            cli::EditOp::Finish {
                token,
                message_file,
            } => commands::edit_finish(&ctx, token, message_file.as_deref(), fmt),
            cli::EditOp::Abort { token } => commands::edit_abort(&ctx, token),
        },

        Command::Watch => watch(&ctx, fmt),
        Command::Undo { checkpoint } => commands::undo(&ctx, checkpoint.as_deref(), fmt),
        Command::Checkpoints { op } => match op {
            cli::CheckpointsOp::Ls => commands::checkpoints_ls(&ctx, fmt),
        },
        Command::Stair { op } => stair(&ctx, op),
        Command::Config { op } => config_show(&ctx, op, fmt),
        Command::Schema { .. } | Command::Completions { .. } => {
            unreachable!()
        }
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            if fmt == Format::Json {
                output::print_json_error("error", &e.to_string());
            } else {
                eprintln!("stacksaw: {e:#}");
            }
            3
        }
    }
}



fn restack(ctx: &Ctx, args: &cli::RestackArgs, fmt: Format) -> anyhow::Result<i32> {
    let snap = ctx.block_on(ctx.core().snapshot())?;
    let stair = match &args.stair {
        Some(name) => snap.staircases.iter().find(|staircase| {
            &staircase.name == name
                || staircase.selector.stable_id() == Some(name.as_str())
        }),
        None => snap.staircases.first(),
    };
    let Some(stair) = stair else {
        anyhow::bail!("no staircase to restack");
    };
    let plan = if let Some(onto) = &args.onto {
        MutatePlan::Rebase {
            selector: stair.selector.clone(),
            expected_record_revision: stair.record_revision.clone(),
            onto: onto.clone(),
            leave_upper_steps_stale: false,
        }
    } else {
        MutatePlan::Restack {
            selector: stair.selector.clone(),
            expected_record_revision: stair.record_revision.clone(),
            from_step_id: None,
        }
    };
    let outcome = ctx.block_on(ctx.core().mutate(plan, Some(snap.generation)))?;
    match fmt {
        Format::Text => println!("restacked (checkpoint {})", outcome.checkpoint),
        _ => output::print_json(&outcome),
    }
    Ok(0)
}



fn watch(ctx: &Ctx, _fmt: Format) -> anyhow::Result<i32> {
    let mut rx = ctx.block_on(ctx.core().subscribe());
    loop {
        match rx.blocking_recv() {
            Ok(ev) => {
                let line = match ev {
                    stacksaw_core::ChangeEvent::SnapshotAdvanced { generation } => {
                        serde_json::json!({ "event": "snapshot/didAdvance", "generation": generation })
                    }
                    stacksaw_core::ChangeEvent::RefsChanged => {
                        serde_json::json!({ "event": "refs/didChange" })
                    }
                    stacksaw_core::ChangeEvent::WorktreeChanged => {
                        serde_json::json!({ "event": "worktree/didChange" })
                    }
                };
                println!("{line}");
                let _ = io::stdout().flush();
            }
            Err(_) => break,
        }
    }
    Ok(0)
}



fn stair(ctx: &Ctx, op: &cli::StairOp) -> anyhow::Result<i32> {
    let snapshot = ctx.block_on(ctx.core().snapshot())?;
    let selected = snapshot
        .staircases
        .first()
        .ok_or_else(|| anyhow::anyhow!("no canonical active staircase"))?;
    let plan = match op {
        cli::StairOp::New { name } => MutatePlan::Name {
            selector: selected.selector.clone(),
            name: name.clone(),
        },
        cli::StairOp::InsertAfter { rev } => {
            let (staircase, segment) = snapshot
                .staircases
                .iter()
                .find_map(|staircase| {
                    staircase
                        .segments
                        .iter()
                        .find(|segment| segment.commits.iter().any(|commit| &commit.oid == rev))
                        .map(|segment| (staircase, segment))
                })
                .ok_or_else(|| anyhow::anyhow!("revision is not in a canonical staircase"))?;
            MutatePlan::Split {
                selector: staircase.selector.clone(),
                expected_record_revision: staircase.record_revision.clone(),
                step_id: segment
                    .step_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("implicit step has no stable ID; name it first"))?,
                at_commit: rev.clone(),
                new_step_name: None,
                no_ref: false,
            }
        }
        cli::StairOp::Fold { branch } => {
            let (staircase, index) = snapshot
                .staircases
                .iter()
                .find_map(|staircase| {
                    staircase
                        .segments
                        .iter()
                        .position(|segment| segment.branch.short() == branch)
                        .map(|index| (staircase, index))
                })
                .ok_or_else(|| anyhow::anyhow!("branch is not a canonical staircase step"))?;
            let upper = staircase
                .segments
                .get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("top step cannot be folded upward"))?;
            MutatePlan::Join {
                selector: staircase.selector.clone(),
                expected_record_revision: staircase.record_revision.clone(),
                lower_step_id: staircase.segments[index]
                    .step_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("step has no stable ID; name it first"))?,
                upper_step_id: upper
                    .step_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("step has no stable ID; name it first"))?,
                keep_retired_ref: false,
            }
        }
        cli::StairOp::Rename { from, to } => {
            let staircase = snapshot
                .staircases
                .iter()
                .find(|staircase| staircase.name == *from)
                .ok_or_else(|| anyhow::anyhow!("no canonical staircase named '{from}'"))?;
            MutatePlan::Rename {
                selector: staircase.selector.clone(),
                expected_record_revision: staircase.record_revision.clone(),
                name: to.clone(),
            }
        }
    };
    let result = ctx.block_on(
        ctx.core()
            .mutate(plan, Some(snapshot.generation)),
    )?;
    println!("updated canonical staircase (checkpoint {})", result.checkpoint);
    Ok(0)
}

fn config_show(ctx: &Ctx, op: &cli::ConfigOp, fmt: Format) -> anyhow::Result<i32> {
    let cli::ConfigOp::Show { origin } = op;
    let (config, prov) = config::load(&ctx.repo_root, &ctx.git_dir);
    if *origin {
        output::print_json(&serde_json::json!({
            "config": config,
            "origins": prov.origins,
        }));
    } else {
        match fmt {
            Format::Text => println!("{}", toml::to_string_pretty(&config).unwrap_or_default()),
            _ => output::print_json(&config),
        }
    }
    Ok(0)
}
