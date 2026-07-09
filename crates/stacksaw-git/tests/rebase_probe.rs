//! Integration tests for the read-only rebase probe (§4 preview): it must
//! report a clean replay as `Clean` and an overlapping one as `Conflict`,
//! without mutating any real ref or the user's working tree.

use std::fs;
use std::path::Path;
use std::process::Command;

use stacksaw_git::model::ModelOptions;
use stacksaw_git::rebase_probe::{probe_rebase, RebaseProbe};
use stacksaw_git::{build_snapshot, Repo};
use stacksaw_ssp::types::RebaseStatus;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "2026-07-01T12:00:00")
        .env("GIT_COMMITTER_DATE", "2026-07-01T12:00:00")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit(dir: &Path, file: &str, contents: &str, msg: &str) {
    let path = dir.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn rev(dir: &Path, spec: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", spec])
        .output()
        .expect("rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn config(port: u32) -> String {
    format!("object Config {{\n    const val PORT: Int = {port}\n}}\n")
}

/// main seeds Config.kt, a stack forks and (later) rewrites its PORT line, then
/// main advances the *same* line — so replaying the stack onto the new main must
/// conflict, while a stack that only adds files replays clean.
fn build(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "Config.kt", &config(8080), "seed config");

    // Clean stack: adds a new file only.
    git(dir, &["checkout", "-q", "-b", "cl-1"]);
    commit(dir, "Api.kt", "interface Api\n", "cl: add api");

    // Conflict stack: rewrites the PORT line.
    git(dir, &["checkout", "-q", "-b", "cf-1", "main"]);
    commit(dir, "Config.kt", &config(9090), "cf: bump port");

    // Advance main past both forks, moving the same PORT line.
    git(dir, &["checkout", "-q", "main"]);
    commit(dir, "Config.kt", &config(3000), "main: move port");
}

#[test]
fn probe_reports_clean_and_conflict_without_touching_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    build(dir);

    let repo = Repo::discover(dir).unwrap();
    let common = repo.common_dir();
    let onto = rev(dir, "main");
    let head_before = rev(dir, "HEAD");

    // The conflict stack's fork point is the seed commit; replaying it onto the
    // moved main must conflict on Config.kt.
    let cf_base = rev(dir, "main~1"); // the seed commit (fork point of cf-1)
    let cf_tip = rev(dir, "cf-1");
    match probe_rebase(dir, &common, &onto, &cf_base, &cf_tip).unwrap() {
        RebaseProbe::Conflict { commit, paths } => {
            assert!(
                paths.iter().any(|p| p.ends_with("Config.kt")),
                "expected Config.kt in conflict paths, got {paths:?}"
            );
            // The replay halts on the stack commit that rewrites the PORT line.
            assert_eq!(
                commit.as_deref(),
                Some(cf_tip.as_str()),
                "conflict should be pinned to cf-1's commit"
            );
        }
        other => panic!("expected conflict, got {other:?}"),
    }

    // The clean stack only adds a file, so it replays cleanly.
    let cl_base = rev(dir, "main~1");
    let cl_tip = rev(dir, "cl-1");
    assert_eq!(
        probe_rebase(dir, &common, &onto, &cl_base, &cl_tip).unwrap(),
        RebaseProbe::Clean
    );

    // The probe must not have disturbed the real repo: same HEAD, clean tree,
    // and the branch refs are unchanged.
    assert_eq!(rev(dir, "HEAD"), head_before, "HEAD moved");
    assert_eq!(rev(dir, "cf-1"), cf_tip, "cf-1 ref moved");
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "working tree should be clean after probing"
    );
}

#[test]
fn build_snapshot_marks_behind_stairs_with_a_rebase_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    build(dir);

    let repo = Repo::discover(dir).unwrap();
    let opts = ModelOptions {
        default_upstream: Some("refs/heads/main".to_string()),
    };
    let mut snap = build_snapshot(&repo, 1, &opts).unwrap();
    // build_snapshot no longer probes (too slow for the hot path); one-shot
    // callers opt in via annotate_rebase.
    stacksaw_git::annotate_rebase(&repo, &mut snap.staircases);

    let cf = snap
        .staircases
        .iter()
        .find(|s| s.segments.iter().any(|seg| seg.branch.short() == "cf-1"))
        .expect("cf staircase");
    assert!(cf.behind > 0, "cf should be behind main");
    assert_eq!(
        cf.rebase,
        RebaseStatus::Conflict,
        "cf should flag a conflict"
    );

    let cl = snap
        .staircases
        .iter()
        .find(|s| s.segments.iter().any(|seg| seg.branch.short() == "cl-1"))
        .expect("cl staircase");
    assert!(cl.behind > 0, "cl should be behind main");
    assert_eq!(
        cl.rebase,
        RebaseStatus::Clean,
        "cl should be a clean rebase"
    );
}

/// Amending an early branch in a family orphans its children (they no longer
/// descend from its new tip). The model must recover them via the amended
/// branch's reflog, regroup the family into one staircase, and mark the
/// recovered link stale; the sync annotator then probes it as a *restack*.
#[test]
fn amend_recovers_stale_children_and_flags_a_restack() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "Config.kt", &config(8080), "seed config");

    // A three-branch family stacked on main: step-1 owns Config, step-2/step-3
    // only add files (so a restack onto an amended step-1 replays cleanly).
    git(dir, &["checkout", "-q", "-b", "step-1"]);
    commit(dir, "Config.kt", &config(9090), "step-1: bump port");
    git(dir, &["checkout", "-q", "-b", "step-2"]);
    commit(dir, "Api.kt", "interface Api\n", "step-2: add api");
    git(dir, &["checkout", "-q", "-b", "step-3"]);
    commit(dir, "Db.kt", "interface Db\n", "step-3: add db");

    // Amend step-1: step-2/step-3 now dangle on its *former* tip.
    git(dir, &["checkout", "-q", "step-1"]);
    fs::write(dir.join("Config.kt"), config(7000)).unwrap();
    git(
        dir,
        &[
            "commit",
            "-q",
            "-a",
            "--amend",
            "-m",
            "step-1: bump port (amended)",
        ],
    );

    let repo = Repo::discover(dir).unwrap();
    let opts = ModelOptions {
        default_upstream: Some("refs/heads/main".to_string()),
    };
    let mut snap = build_snapshot(&repo, 1, &opts).unwrap();

    // The family regrouped into one staircase with all three branches.
    let step = snap
        .staircases
        .iter()
        .find(|s| s.name == "step")
        .expect("step staircase should reform");
    let branches: Vec<&str> = step.segments.iter().map(|seg| seg.branch.short()).collect();
    assert_eq!(branches, ["step-1", "step-2", "step-3"], "regrouped order");

    // The recovered (step-2) link is stale; the coherent step-1 link is not.
    let stale: Vec<&str> = step
        .segments
        .iter()
        .filter(|seg| seg.stale)
        .map(|seg| seg.branch.short())
        .collect();
    assert_eq!(stale, ["step-2"], "only the amended-parent link is stale");

    // A restack of the clean children replays without conflict.
    stacksaw_git::annotate_rebase(&repo, &mut snap.staircases);
    let step = snap.staircases.iter().find(|s| s.name == "step").unwrap();
    assert_eq!(
        step.rebase,
        RebaseStatus::Clean,
        "restack of file-only children should be clean"
    );
}
