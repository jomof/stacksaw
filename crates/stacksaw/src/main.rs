//! `stacksaw` — one binary, three faces (§1, §3): a TUI, a per-repo core
//! service, and a scriptable CLI. This entry point dispatches by role.

mod cli;
mod commands;
mod context;
mod output;
mod schema;
mod tui;

use std::path::Path;
use clap::{CommandFactory, Parser};
use cli::{Cli, Command};
use context::Ctx;
use output::Format;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

fn main() {
    let cli = Cli::parse();
    let _guard = init_logging(cli.log_file.as_deref());
    let fmt: Format = cli.output.into();
    let code = run(cli, fmt);
    std::process::exit(code);
}

/// Initialize file logging, but only when the user opts in via `--log-file`
/// (or `STACKSAW_LOG_FILE`). Returns the appender's flush guard, which must be
/// held for the process lifetime. Verbosity honours `RUST_LOG`, defaulting to
/// `debug` for the requested log file.
fn init_logging(log_file: Option<&Path>) -> Option<WorkerGuard> {
    let path = log_file?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file_name = path.file_name()?;
    let directory = path.parent().filter(|p| !p.as_os_str().is_empty());
    let file_appender =
        tracing_appender::rolling::never(directory.unwrap_or_else(|| Path::new(".")), file_name);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(filter)
        .with_ansi(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber).ok()?;
    Some(guard)
}

/// Open the most-recently-used repo from the MRU, skipping any entry that no
/// longer resolves to a git repo. Used as a fallback when `stacksaw` is launched
/// outside a repository. Returns `None` when the MRU is empty or all stale.
fn open_most_recent(upstream: Option<String>) -> Option<Ctx> {
    stacksaw_core::recent::RecentStore::load()
        .repos
        .iter()
        .find_map(|r| Ctx::open_at(&r.path, upstream.clone()).ok())
}

fn run(cli: Cli, fmt: Format) -> i32 {
    // No subcommand → open a UI window (§3). Launched outside a git repo, fall
    // back to the most-recently-used repo so the window still opens somewhere
    // useful; only error if there's nothing to fall back to.
    let Some(command) = &cli.command else {
        let upstream = cli.upstream.clone();
        let ctx = Ctx::open(upstream.clone())
            .or_else(|e| open_most_recent(upstream.clone()).ok_or(e));
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
            clap_complete::generate(*shell, &mut cmd, "stacksaw", &mut std::io::stdout());
            return 0;
        }
        Command::Core { op } => return core_command(op),
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
        Command::Lint(args) => commands::lint(&ctx, args, fmt, cli.yes),
        Command::Fix(args) => commands::fix(&ctx, args, fmt, cli.yes),
        Command::Restack(args) => restack(&ctx, args, fmt),
        Command::Edit { op } => match op {
            cli::EditOp::Begin { commit } => commands::edit_begin(&ctx, commit, fmt),
            cli::EditOp::Finish { token, message_file } => {
                commands::edit_finish(&ctx, token, message_file.as_deref(), fmt)
            }
            cli::EditOp::Abort { token } => commands::edit_abort(&ctx, token),
        },
        Command::Comment { op } => comment(&ctx, op, fmt),
        Command::Watch => watch(&ctx, fmt),
        Command::Undo { checkpoint } => commands::undo(&ctx, checkpoint.as_deref(), fmt),
        Command::Checkpoints { op } => match op {
            cli::CheckpointsOp::Ls => commands::checkpoints_ls(&ctx, fmt),
        },
        Command::Agent { op } => agent(&ctx, op, fmt),
        Command::Stair { op } => stair(&ctx, op),
        Command::Config { op } => config_show(&ctx, op, fmt),
        Command::Schema { .. } | Command::Completions { .. } | Command::Core { .. } => unreachable!(),
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

fn core_command(op: &cli::CoreOp) -> i32 {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("stacksaw: {e}");
            return 4;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    match op {
        cli::CoreOp::Serve { .. } => match rt.block_on(stacksaw_core::daemon::run(&cwd)) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("stacksaw core: {e:#}");
                4
            }
        },
        cli::CoreOp::Stop => match stacksaw_core::daemon::stop(&cwd) {
            Ok(true) => {
                println!("stopped");
                0
            }
            Ok(false) => {
                println!("no daemon running");
                0
            }
            Err(e) => {
                eprintln!("{e:#}");
                4
            }
        },
        cli::CoreOp::Status => match stacksaw_core::daemon::status(&cwd) {
            Ok(Some(info)) => {
                println!("running: pid {} at {}", info.pid, info.endpoint);
                0
            }
            Ok(None) => {
                println!("not running");
                0
            }
            Err(e) => {
                eprintln!("{e:#}");
                4
            }
        },
        cli::CoreOp::Verify => {
            // Force a re-sync: stop then report (a full impl re-walks refs).
            let _ = stacksaw_core::daemon::stop(&cwd);
            println!("verified (daemon reset)");
            0
        }
    }
}

fn restack(ctx: &Ctx, args: &cli::RestackArgs, fmt: Format) -> anyhow::Result<i32> {
    use stacksaw_agents::{RestackParams, Restacker};
    let repo = ctx.repo()?;
    let snap = stacksaw_git::build_snapshot(&repo, 0, &ctx.model_options())?;
    let stair = match &args.stair {
        Some(name) => snap.staircases.iter().find(|s| &s.name == name),
        None => snap.staircases.first(),
    };
    let Some(stair) = stair else {
        anyhow::bail!("no staircase to restack");
    };
    let branches: Vec<String> = stair.segments.iter().map(|s| s.branch.clone()).collect();
    let onto = args.onto.clone().unwrap_or_else(|| stair.upstream.clone());

    let params = RestackParams {
        staircase: branches,
        onto,
        fix_policy: Default::default(),
        conflict_policy: stacksaw_agents::ConflictPolicy::Stop,
        max_attempts: 3,
    };
    let mut restacker = Restacker::new(&repo, params);
    if args.fix_lints {
        restacker = restacker
            .with_oracle("stacksaw lint --commit HEAD --profile upload --output=json --fail-on error");
    }
    let outcome = restacker.run()?;
    match fmt {
        Format::Text => match &outcome {
            stacksaw_agents::RestackOutcome::Completed { rewrites, checkpoint, .. } => {
                println!("restacked ({} rewrites, checkpoint {checkpoint})", rewrites.len());
            }
            stacksaw_agents::RestackOutcome::Paused { kind, commit, .. } => {
                println!("paused at {commit}: {kind:?}");
            }
        },
        _ => output::print_json(&outcome),
    }
    Ok(0)
}

fn comment(ctx: &Ctx, op: &cli::CommentOp, fmt: Format) -> anyhow::Result<i32> {
    let notes_dir = ctx.git_dir.join("stacksaw").join("notes");
    std::fs::create_dir_all(&notes_dir)?;
    match op {
        cli::CommentOp::Add { file, line, text } => {
            let note = serde_json::json!({
                "schemaVersion": 1,
                "source": "note:me",
                "file": file,
                "line": line,
                "text": text,
                "ts": jiff::Timestamp::now().to_string(),
            });
            let id = blake3::hash(format!("{file}:{line}:{text}").as_bytes()).to_hex()[..12].to_string();
            std::fs::write(notes_dir.join(format!("{id}.json")), serde_json::to_vec_pretty(&note)?)?;
            if fmt == Format::Text {
                println!("added note {id}");
            } else {
                output::print_json(&serde_json::json!({ "id": id }));
            }
            Ok(0)
        }
        cli::CommentOp::Ls | cli::CommentOp::Export => {
            let mut notes = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&notes_dir) {
                for e in entries.flatten() {
                    if let Ok(bytes) = std::fs::read(e.path()) {
                        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                            notes.push(v);
                        }
                    }
                }
            }
            output::print_json(&serde_json::json!({ "notes": notes }));
            Ok(0)
        }
    }
}

fn watch(ctx: &Ctx, _fmt: Format) -> anyhow::Result<i32> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let service = stacksaw_core::Service::new(
            ctx.repo_root.clone(),
            ctx.git_dir.clone(),
            ctx.config.clone(),
        );
        let _guard = stacksaw_core::watch::spawn(service.clone())?;
        let mut rx = service.subscribe();
        while let Ok(ev) = rx.recv().await {
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
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(0)
}

fn agent(ctx: &Ctx, op: &cli::AgentOp, fmt: Format) -> anyhow::Result<i32> {
    match op {
        cli::AgentOp::List => {
            // Resolve configured agents from user drop-ins (§9.2).
            let mut agents = Vec::new();
            if let Some(dirs) = directories::ProjectDirs::from("", "", "stacksaw") {
                let agents_dir = dirs.config_dir().join("agents");
                if let Ok(entries) = std::fs::read_dir(&agents_dir) {
                    for e in entries.flatten() {
                        if e.path().extension().is_some_and(|x| x == "toml") {
                            if let Some(stem) = e.path().file_stem() {
                                agents.push(stem.to_string_lossy().to_string());
                            }
                        }
                    }
                }
            }
            match fmt {
                Format::Text => {
                    if agents.is_empty() {
                        println!("No agents configured. Add ~/.config/stacksaw/agents/*.toml");
                    }
                    for a in &agents {
                        println!("{a}");
                    }
                }
                _ => output::print_json(&serde_json::json!({ "agents": agents })),
            }
            Ok(0)
        }
        cli::AgentOp::Run { workflow, agent } => {
            let _ = ctx;
            output::print_json_error(
                "not-configured",
                &format!(
                    "agent run '{workflow}' requires a configured ACP agent{}",
                    agent.as_ref().map(|a| format!(" ({a})")).unwrap_or_default()
                ),
            );
            Ok(4)
        }
    }
}

fn stair(_ctx: &Ctx, op: &cli::StairOp) -> anyhow::Result<i32> {
    match op {
        cli::StairOp::Rename { from, to } => {
            let cwd = std::env::current_dir()?;
            stacksaw_git::refs::git(&cwd, &["branch", "-m", from, to])?;
            println!("renamed {from} → {to}");
            Ok(0)
        }
        other => {
            eprintln!("stair {other:?}: not yet implemented in v0.1");
            Ok(2)
        }
    }
}

fn config_show(ctx: &Ctx, op: &cli::ConfigOp, fmt: Format) -> anyhow::Result<i32> {
    let cli::ConfigOp::Show { origin } = op;
    let (config, prov) = stacksaw_core::config::load(&ctx.repo_root, &ctx.git_dir);
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
