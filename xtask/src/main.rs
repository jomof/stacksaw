//! Build tasks: fixture repo generation, grammar query checks, benchmarks
//! (§4, §7.5, §8.6, §14).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate fixture repositories used by tests (§14).
    Fixtures {
        #[arg(default_value = "target/fixtures")]
        dir: PathBuf,
    },
    /// Validate the ktfqn tree-sitter queries against the pinned grammar (§7.5).
    LintQueries,
    /// Placeholder for the performance-budget benchmark harness (§8.6).
    Bench,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Fixtures { dir } => fixtures(&dir),
        Cmd::LintQueries => lint_queries(),
        Cmd::Bench => {
            println!("bench: not yet implemented (§8.6 budgets)");
            Ok(())
        }
    }
}

/// Node kinds the ktfqn queries depend on. If the pinned grammar renames any of
/// these, this check fails in CI before shipping a broken linter (§7.5).
const REQUIRED_NODE_KINDS: &[&str] = &[
    "navigation_expression",
    "user_type",
    "import_header",
    "package_header",
    "type_identifier",
    "simple_identifier",
];

fn lint_queries() -> Result<()> {
    let language = stacksaw_lint_kotlin::language();
    let mut missing = Vec::new();
    for kind in REQUIRED_NODE_KINDS {
        // id 0 (the "end" sentinel) means the grammar has no such named node.
        let id = language.id_for_node_kind(kind, true);
        if id == 0 {
            missing.push(*kind);
        }
    }
    if !missing.is_empty() {
        bail!(
            "pinned tree-sitter-kotlin grammar is missing node kinds required by ktfqn: {}",
            missing.join(", ")
        );
    }
    println!("ktfqn query check: all {} node kinds present", REQUIRED_NODE_KINDS.len());
    Ok(())
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .status()
        .with_context(|| format!("running git {args:?}"))?;
    if !status.success() {
        bail!("git {args:?} failed");
    }
    Ok(())
}

fn commit(dir: &Path, file: &str, contents: &str, msg: &str) -> Result<()> {
    std::fs::write(dir.join(file), contents)?;
    git(dir, &["add", "."])?;
    git(dir, &["commit", "-q", "-m", msg])?;
    Ok(())
}

fn fixtures(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;

    // 1. Linear 3-commit stack.
    let linear = dir.join("linear-stack");
    if !linear.exists() {
        std::fs::create_dir_all(&linear)?;
        git(&linear, &["init", "-q", "-b", "main"])?;
        commit(&linear, "base.txt", "base\n", "Initial commit")?;
        git(&linear, &["checkout", "-q", "-b", "feat"])?;
        commit(&linear, "a.txt", "a\n", "Add a")?;
        commit(&linear, "b.txt", "b\n", "Add b")?;
        commit(&linear, "c.txt", "c\n", "Add c")?;
    }

    // 2. 8-step staircase.
    let stair = dir.join("staircase-8");
    if !stair.exists() {
        std::fs::create_dir_all(&stair)?;
        git(&stair, &["init", "-q", "-b", "main"])?;
        commit(&stair, "base.txt", "base\n", "Initial commit")?;
        for i in 1..=8 {
            git(&stair, &["checkout", "-q", "-b", &format!("step{i}")])?;
            commit(&stair, &format!("f{i}.txt"), &format!("{i}\n"), &format!("Step {i}"))?;
        }
    }

    // 3. Forked segment tree.
    let forked = dir.join("forked-tree");
    if !forked.exists() {
        std::fs::create_dir_all(&forked)?;
        git(&forked, &["init", "-q", "-b", "main"])?;
        commit(&forked, "base.txt", "base\n", "Initial commit")?;
        git(&forked, &["checkout", "-q", "-b", "trunk"])?;
        commit(&forked, "t.txt", "t\n", "Trunk")?;
        git(&forked, &["checkout", "-q", "-b", "left"])?;
        commit(&forked, "l.txt", "l\n", "Left")?;
        git(&forked, &["checkout", "-q", "trunk"])?;
        git(&forked, &["checkout", "-q", "-b", "right"])?;
        commit(&forked, "r.txt", "r\n", "Right")?;
    }

    // 4. Twins: same Change-Id on two branches.
    let twins = dir.join("twins");
    if !twins.exists() {
        std::fs::create_dir_all(&twins)?;
        git(&twins, &["init", "-q", "-b", "main"])?;
        commit(&twins, "base.txt", "base\n", "Initial commit")?;
        let msg = "Shared change\n\nChange-Id: I0123456789abcdef0123456789abcdef01234567";
        git(&twins, &["checkout", "-q", "-b", "a"])?;
        commit(&twins, "x.txt", "x\n", msg)?;
        git(&twins, &["checkout", "-q", "main"])?;
        git(&twins, &["checkout", "-q", "-b", "b"])?;
        commit(&twins, "y.txt", "y\n", msg)?;
    }

    println!("fixtures written under {}", dir.display());
    Ok(())
}
