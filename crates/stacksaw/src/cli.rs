//! clap command surface (§10.1).

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputArg {
    Text,
    Json,
    Jsonl,
}

impl From<OutputArg> for crate::output::Format {
    fn from(a: OutputArg) -> Self {
        match a {
            OutputArg::Text => crate::output::Format::Text,
            OutputArg::Json => crate::output::Format::Json,
            OutputArg::Jsonl => crate::output::Format::Jsonl,
        }
    }
}

/// stacksaw — view, review, and reshape stacked and staircased git branches.
#[derive(Debug, Parser)]
#[command(name = "stacksaw", version, about, long_about = None)]
pub struct Cli {
    /// Output format for machine consumers.
    #[arg(long, value_enum, default_value = "text", global = true)]
    pub output: OutputArg,

    /// Never spawn or attach to a core daemon; build a one-shot snapshot.
    #[arg(long, global = true, env = "STACKSAW_NO_DAEMON")]
    pub no_daemon: bool,

    /// Assume "yes" to prompts (non-interactive).
    #[arg(long, global = true)]
    pub yes: bool,

    /// Never prompt; fail instead of asking.
    #[arg(long, global = true)]
    pub no_input: bool,

    /// Override the upstream ref for this invocation.
    #[arg(long, global = true)]
    pub upstream: Option<String>,

    /// Write debug logs to this file.
    #[arg(long, global = true, env = "STACKSAW_LOG_FILE")]
    pub log_file: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List staircases and their segments.
    Ls,
    /// Show working-tree and stack status.
    Status,
    /// Show a commit with trailers and findings.
    Show { rev: String },
    /// Show a diff for a range (defaults to the selected commit).
    Diff {
        range: Option<String>,
        #[arg(long)]
        patch: bool,
        #[arg(long)]
        name_only: bool,
    },
    /// Show a range-diff between two stack versions.
    Interdiff { ref_a: String, ref_b: String },
    /// Run linters over a scope.
    Lint(LintArgs),
    /// Apply autofixes for a commit (amend + restack descendants).
    Fix(FixArgs),
    /// Rebase a staircase onto upstream, optionally fixing lints.
    Restack(RestackArgs),
    /// Staircase reshaping operations.
    Stair {
        #[command(subcommand)]
        op: StairOp,
    },
    /// Edit sessions — the flagship inbound primitive.
    Edit {
        #[command(subcommand)]
        op: EditOp,
    },
    /// Local review notes.
    Comment {
        #[command(subcommand)]
        op: CommentOp,
    },
    /// Stream live change events as jsonl.
    Watch,
    /// Restore a checkpoint.
    Undo { checkpoint: Option<String> },
    /// List available checkpoints.
    Checkpoints {
        #[command(subcommand)]
        op: CheckpointsOp,
    },
    /// Outbound agents.
    Agent {
        #[command(subcommand)]
        op: AgentOp,
    },
    /// Print a JSON Schema for machine consumption.
    Schema { name: Option<String> },
    /// Manage the per-repo core service.
    Core {
        #[command(subcommand)]
        op: CoreOp,
    },
    /// Print shell completions.
    Completions { shell: clap_complete::Shell },
    /// Show the merged configuration.
    Config {
        #[command(subcommand)]
        op: ConfigOp,
    },
}

#[derive(Debug, Args)]
pub struct LintArgs {
    #[arg(long)]
    pub commit: Option<String>,
    #[arg(long)]
    pub range: Option<String>,
    #[arg(long)]
    pub stair: Option<String>,
    #[arg(long)]
    pub all: bool,
    #[arg(long, default_value = "local")]
    pub profile: String,
    /// Exit non-zero when findings at/above this level exist.
    #[arg(long)]
    pub fail_on: Option<String>,
    /// Apply autofixes after linting.
    #[arg(long)]
    pub fix: bool,
}

#[derive(Debug, Args)]
pub struct FixArgs {
    #[arg(long)]
    pub commit: String,
    #[arg(long)]
    pub linter: Option<String>,
}

#[derive(Debug, Args)]
pub struct RestackArgs {
    #[arg(long)]
    pub onto: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub fix_lints: bool,
    #[arg(long)]
    pub stair: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum StairOp {
    New { name: String },
    InsertAfter { rev: String },
    Fold { branch: String },
    Rename { from: String, to: String },
}

#[derive(Debug, Subcommand)]
pub enum EditOp {
    Begin {
        #[arg(long)]
        commit: String,
    },
    Finish {
        #[arg(long)]
        token: String,
        #[arg(long)]
        message_file: Option<String>,
    },
    Abort {
        #[arg(long)]
        token: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum CommentOp {
    Add {
        #[arg(long)]
        file: String,
        #[arg(long)]
        line: u32,
        text: String,
    },
    Ls,
    Export,
}

#[derive(Debug, Subcommand)]
pub enum CheckpointsOp {
    Ls,
}

#[derive(Debug, Subcommand)]
pub enum AgentOp {
    List,
    Run {
        workflow: String,
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum CoreOp {
    Serve {
        #[arg(long)]
        daemon: bool,
    },
    Stop,
    Status,
    Verify,
}

#[derive(Debug, Subcommand)]
pub enum ConfigOp {
    Show {
        #[arg(long)]
        origin: bool,
    },
}
