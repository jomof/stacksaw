//! clap command surface (§10.1).

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::output::Format;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputArg {
    Text,
    Json,
    Jsonl,
}

impl From<OutputArg> for Format {
    fn from(a: OutputArg) -> Self {
        match a {
            OutputArg::Text => Format::Text,
            OutputArg::Json => Format::Json,
            OutputArg::Jsonl => Format::Jsonl,
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
    pub log_file: Option<PathBuf>,

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
    /// Rebase a staircase onto upstream.
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

    /// Stream live change events as jsonl.
    Watch,
    /// Restore a checkpoint.
    Undo { checkpoint: Option<String> },
    /// List available checkpoints.
    Checkpoints {
        #[command(subcommand)]
        op: CheckpointsOp,
    },
    /// Print a JSON Schema for machine consumption.
    Schema { name: Option<String> },

    /// Print shell completions.
    Completions { shell: clap_complete::Shell },
    /// Show the merged configuration.
    Config {
        #[command(subcommand)]
        op: ConfigOp,
    },
}

#[derive(Debug, Args)]
pub struct RestackArgs {
    #[arg(long)]
    pub onto: Option<String>,
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
pub enum CheckpointsOp {
    Ls,
}



#[derive(Debug, Subcommand)]
pub enum ConfigOp {
    Show {
        #[arg(long)]
        origin: bool,
    },
}
