//! End-to-end tests of indent/unindent reshaping against real fixture repos.

use std::fs;
use std::path::Path;
use std::process::Command;

use stacksaw_git::model::ModelOptions;
use stacksaw_git::reshape::{self, Op};
use stacksaw_git::{build_staircases, Repo};

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
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit(dir: &Path, file: &str, msg: &str) {
    fs::write(dir.join(file), format!("{file}\n")).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn opts() -> ModelOptions {
    ModelOptions {
        default_upstream: Some("refs/heads/main".to_string()),
    }
}

/// A single branch `feature` of six commits off `main`, checked out on the tip.
fn single_stack(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base");
    git(dir, &["checkout", "-q", "-b", "feature"]);
    for i in 1..=6 {
        commit(dir, &format!("c{i}.txt"), &format!("c{i}"));
    }
    git(dir, &["branch", "--set-upstream-to=main", "feature"]);
}

/// A three-step staircase off `main` matching the UI screenshot:
/// `feature-1`=[c1], `feature-2`=[c2], `feature`=[c3..c6]; HEAD on the tip.
fn staircase_stack(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base");
    git(dir, &["checkout", "-q", "-b", "feature-1"]);
    commit(dir, "c1.txt", "c1");
    git(dir, &["checkout", "-q", "-b", "feature-2"]);
    commit(dir, "c2.txt", "c2");
    git(dir, &["checkout", "-q", "-b", "feature"]);
    for i in 3..=6 {
        commit(dir, &format!("c{i}.txt"), &format!("c{i}"));
    }
    for b in ["feature-1", "feature-2", "feature"] {
        git(dir, &["branch", "--set-upstream-to=main", b]);
    }
}

/// The staircase containing `oid`, as `(branch, [commit oids]) `steps.
fn steps(dir: &Path) -> Vec<(String, Vec<String>)> {
    let repo = Repo::discover(dir).unwrap();
    let stairs = build_staircases(&repo, &opts()).unwrap();
    let stair = stairs
        .iter()
        .find(|s| s.name == "feature")
        .expect("feature staircase");
    stair
        .segments
        .iter()
        .map(|seg| {
            (
                seg.branch.short().to_string(),
                seg.commits.iter().map(|c| c.oid.clone()).collect(),
            )
        })
        .collect()
}

/// The flat commit sequence (oldest→newest) of the `feature` staircase.
fn sequence(dir: &Path) -> Vec<String> {
    steps(dir).into_iter().flat_map(|(_, c)| c).collect()
}

fn head_branch(dir: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn indent_merges_a_middle_step_into_feature_and_undo_restores_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    staircase_stack(dir);
    let seq = sequence(dir);
    assert_eq!(seq.len(), 6);
    assert_eq!(steps(dir).len(), 3);

    // Indent c2 (the sole commit of feature-2): it moves into feature, so
    // feature-2 disappears — exactly the screenshot's expectation.
    let repo = Repo::discover(dir).unwrap();
    let undo = reshape::apply(&repo, &opts(), &seq[1], Op::Indent)
        .unwrap()
        .expect("refs moved");

    let s = steps(dir);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].0, "feature-1");
    assert_eq!(s[0].1, seq[0..1]);
    assert_eq!(s[1].0, "feature");
    assert_eq!(s[1].1, seq[1..6]);
    // No reorder, tip unchanged.
    assert_eq!(sequence(dir), seq);
    assert_eq!(head_branch(dir), "feature");
    let repo = Repo::discover(dir).unwrap();
    assert!(repo
        .local_branches()
        .unwrap()
        .iter()
        .all(|b| b.name != "feature-2"));

    // Undo restores feature-2 exactly.
    let repo = Repo::discover(dir).unwrap();
    reshape::undo(&repo, &undo).unwrap();
    let s = steps(dir);
    assert_eq!(s.len(), 3);
    let names: Vec<&str> = s.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["feature-1", "feature-2", "feature"]);
    assert_eq!(s[1].1, seq[1..2]);
}

#[test]
fn indent_a_step_tip_moves_only_that_commit_one_step_deeper() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // From feature-1=[c1], feature-2=[c2], feature=[c3..c6], pull c3 down into
    // feature-2 so it is [c2,c3] (a two-commit middle step).
    staircase_stack(dir);
    let seq = sequence(dir);
    let repo = Repo::discover(dir).unwrap();
    reshape::apply(&repo, &opts(), &seq[2], Op::Unindent).unwrap();
    assert_eq!(steps(dir)[1].1, seq[1..3], "feature-2 is now [c2,c3]");

    // Indent c3 (the tip of feature-2): only c3 moves into feature.
    let repo = Repo::discover(dir).unwrap();
    reshape::apply(&repo, &opts(), &seq[2], Op::Indent)
        .unwrap()
        .expect("refs moved");
    let s = steps(dir);
    assert_eq!(s.len(), 3);
    let names: Vec<&str> = s.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["feature-1", "feature-2", "feature"]);
    assert_eq!(s[1].1, seq[1..2]); // feature-2 back to just c2
    assert_eq!(s[2].1, seq[2..6]); // feature gained c3
}

#[test]
fn indent_cuts_a_new_step_on_a_single_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    single_stack(dir);
    let seq = sequence(dir);
    // Indenting c3 on the single `feature` branch cuts a step before it.
    let repo = Repo::discover(dir).unwrap();
    reshape::apply(&repo, &opts(), &seq[2], Op::Indent)
        .unwrap()
        .expect("refs moved");
    let s = steps(dir);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].0, "feature-1");
    assert_eq!(s[0].1, seq[0..2]);
    assert_eq!(s[1].0, "feature");
    assert_eq!(s[1].1, seq[2..6]);
    assert_eq!(sequence(dir), seq, "no reorder");
    assert_eq!(head_branch(dir), "feature");
}

#[test]
fn indent_the_first_commit_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    single_stack(dir);
    let seq = sequence(dir);
    // Nothing precedes c1, so there is no step to cut before it.
    let repo = Repo::discover(dir).unwrap();
    let again = reshape::apply(&repo, &opts(), &seq[0], Op::Indent).unwrap();
    assert!(again.is_none(), "indenting c1 is a no-op");
    assert_eq!(steps(dir).len(), 1);
}

#[test]
fn unindent_first_commit_of_first_step_creates_a_prior_step() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    single_stack(dir);
    let seq = sequence(dir);

    // Unindent c3 of the sole step: c1..c3 peel off into a new base step.
    let repo = Repo::discover(dir).unwrap();
    reshape::apply(&repo, &opts(), &seq[2], Op::Unindent)
        .unwrap()
        .expect("refs moved");
    let s = steps(dir);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].0, "feature-1");
    assert_eq!(s[0].1, seq[0..3]);
    assert_eq!(s[1].0, "feature");
    assert_eq!(s[1].1, seq[3..6]);
}
