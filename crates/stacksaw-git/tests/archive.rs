//! End-to-end tests of archiving stacks against real fixture repos.

use std::fs;
use std::path::Path;

use stacksaw_git::archive::{self, ARCHIVE_PREFIX};
use stacksaw_git::executor::GitExecutor;
use stacksaw_git::model::ModelOptions;
use stacksaw_git::reshape;
use stacksaw_git::{build_staircases, Repo};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = GitExecutor::new(dir)
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
    String::from_utf8_lossy(&out.stdout).to_string()
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

/// A three-step staircase off `main`: `feature-1`=[c1], `feature-2`=[c2],
/// `feature`=[c3..c6]. Leaves HEAD on `main` so the stack is archivable.
fn staircase_on_main(dir: &Path) {
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
    git(dir, &["checkout", "-q", "main"]);
}

fn local_branches(dir: &Path) -> Vec<String> {
    let repo = Repo::discover(dir).unwrap();
    let mut names: Vec<String> = repo
        .local_branches()
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    names.sort();
    names
}

/// The archive refs present, as `(leaf name, oid)`.
fn archive_refs(dir: &Path) -> Vec<(String, String)> {
    let text = git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            ARCHIVE_PREFIX,
        ],
    );
    text.lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(name, oid)| {
            let leaf = name
                .strip_prefix(&format!("{ARCHIVE_PREFIX}/"))
                .unwrap_or(name);
            (leaf.to_string(), oid.to_string())
        })
        .collect()
}

#[test]
fn archiving_a_staircase_parks_all_its_branches_and_undo_restores_them() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    staircase_on_main(dir);

    // The Stacks model sees one `feature` staircase of three branches.
    let repo = Repo::discover(dir).unwrap();
    let stair = build_staircases(&repo, &opts())
        .unwrap()
        .into_iter()
        .find(|s| s.name == "feature")
        .expect("feature staircase");
    let branches: Vec<String> = stair
        .segments
        .iter()
        .map(|s| s.branch.short().to_string())
        .collect();
    assert_eq!(branches, vec!["feature-1", "feature-2", "feature"]);

    // Record tips so we can prove the commits survive.
    let tips: Vec<String> = branches
        .iter()
        .map(|b| {
            git(dir, &["rev-parse", &format!("refs/heads/{b}")])
                .trim()
                .to_string()
        })
        .collect();

    let repo = Repo::discover(dir).unwrap();
    let undo = archive::archive(&repo, &opts(), &branches)
        .unwrap()
        .expect("refs moved");

    // The branches are gone from `refs/heads/` but parked under the archive
    // namespace at their exact tips (so the objects stay reachable).
    assert_eq!(local_branches(dir), vec!["main".to_string()]);
    let mut parked = archive_refs(dir);
    parked.sort();
    assert_eq!(
        parked,
        vec![
            ("feature".to_string(), tips[2].clone()),
            ("feature-1".to_string(), tips[0].clone()),
            ("feature-2".to_string(), tips[1].clone()),
        ]
    );
    // Reachable → rev-parse of the archived tip still succeeds.
    for (b, tip) in branches.iter().zip(&tips) {
        let got = git(dir, &["rev-parse", &format!("{ARCHIVE_PREFIX}/{b}")]);
        assert_eq!(got.trim(), tip);
    }
    // The staircase no longer appears in the model.
    let repo = Repo::discover(dir).unwrap();
    assert!(build_staircases(&repo, &opts())
        .unwrap()
        .iter()
        .all(|s| s.name != "feature"));

    // Undo brings the branches back at their original tips and clears the
    // archive refs.
    let repo = Repo::discover(dir).unwrap();
    reshape::undo(&repo, &undo).unwrap();
    assert_eq!(
        local_branches(dir),
        vec![
            "feature".to_string(),
            "feature-1".to_string(),
            "feature-2".to_string(),
            "main".to_string()
        ]
    );
    for (b, tip) in branches.iter().zip(&tips) {
        assert_eq!(
            git(dir, &["rev-parse", &format!("refs/heads/{b}")]).trim(),
            tip
        );
    }
    assert!(archive_refs(dir).is_empty(), "archive refs cleared on undo");
}

#[test]
fn archiving_a_single_branch_leaves_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    staircase_on_main(dir);

    let repo = Repo::discover(dir).unwrap();
    archive::archive(&repo, &opts(), &["feature-1".to_string()])
        .unwrap()
        .expect("refs moved");

    assert_eq!(
        local_branches(dir),
        vec![
            "feature".to_string(),
            "feature-2".to_string(),
            "main".to_string()
        ]
    );
    assert_eq!(
        archive_refs(dir)
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>(),
        vec!["feature-1"]
    );
}

fn head_branch(dir: &Path) -> String {
    git(dir, &["symbolic-ref", "--short", "HEAD"])
        .trim()
        .to_string()
}

#[test]
fn archiving_the_checked_out_stack_lands_on_base_and_undo_returns() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    staircase_on_main(dir);
    // Stand on the tip and archive the whole stack.
    git(dir, &["checkout", "-q", "feature"]);
    let tip = git(dir, &["rev-parse", "refs/heads/feature"])
        .trim()
        .to_string();
    let main_before = git(dir, &["rev-parse", "refs/heads/main"])
        .trim()
        .to_string();

    let repo = Repo::discover(dir).unwrap();
    let undo = archive::archive(
        &repo,
        &opts(),
        &[
            "feature-1".to_string(),
            "feature-2".to_string(),
            "feature".to_string(),
        ],
    )
    .unwrap()
    .expect("refs moved");

    // Landed on the base branch; the stack is archived and gone from heads.
    assert_eq!(head_branch(dir), "main");
    assert_eq!(local_branches(dir), vec!["main".to_string()]);
    // Working tree now matches main (feature's file is gone).
    assert!(
        !dir.join("c6.txt").exists(),
        "checked out base, stack files gone"
    );

    // Undo restores every branch and checks the tip back out.
    let repo = Repo::discover(dir).unwrap();
    reshape::undo(&repo, &undo).unwrap();
    assert_eq!(head_branch(dir), "feature");
    assert_eq!(git(dir, &["rev-parse", "refs/heads/feature"]).trim(), tip);
    assert_eq!(
        git(dir, &["rev-parse", "refs/heads/main"]).trim(),
        main_before
    );
    assert!(dir.join("c6.txt").exists(), "tip files restored on undo");
}

#[test]
fn refuses_when_the_checked_out_stack_is_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    staircase_on_main(dir);
    git(dir, &["checkout", "-q", "feature"]);
    fs::write(dir.join("c6.txt"), "dirty\n").unwrap();

    let repo = Repo::discover(dir).unwrap();
    let err = archive::archive(&repo, &opts(), &["feature".to_string()]);
    assert!(err.is_err(), "a dirty checked-out stack must be refused");
    assert!(local_branches(dir).contains(&"feature".to_string()));
    assert!(archive_refs(dir).is_empty());
}

#[test]
fn synthetic_rows_with_no_real_branch_are_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    staircase_on_main(dir);

    // A short-oid "branch" name (as a detached-HEAD row would carry) is not a
    // real head, so there is nothing to archive.
    let repo = Repo::discover(dir).unwrap();
    let res = archive::archive(&repo, &opts(), &["deadbeef".to_string()]).unwrap();
    assert!(res.is_none());
    assert!(archive_refs(dir).is_empty());
}

#[test]
fn archiving_checked_out_branch_with_no_upstream_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "feature"]);
    commit(dir, "base.txt", "base");
    // HEAD is on feature, and it is the only branch.

    let repo = Repo::discover(dir).unwrap();
    let err = archive::archive(&repo, &ModelOptions::default(), &["feature".to_string()]);
    assert!(err.is_err());
    if let Err(stacksaw_git::error::GitError::Other(msg)) = err {
        assert!(msg.contains("no local base branch to land on"));
    } else {
        panic!("expected GitError::Other");
    }
}

#[test]
fn archiving_non_checked_out_branch_with_no_upstream_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base");
    git(dir, &["checkout", "-q", "-b", "feature"]);
    commit(dir, "c1.txt", "c1");
    git(dir, &["checkout", "-q", "main"]);
    // HEAD is on main, feature has no upstream.

    let repo = Repo::discover(dir).unwrap();
    let undo = archive::archive(&repo, &ModelOptions::default(), &["feature".to_string()])
        .unwrap()
        .expect("refs moved");

    assert_eq!(local_branches(dir), vec!["main".to_string()]);
    assert_eq!(
        archive_refs(dir)
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>(),
        vec!["feature"]
    );

    reshape::undo(&repo, &undo).unwrap();
    assert_eq!(
        local_branches(dir),
        vec!["feature".to_string(), "main".to_string()]
    );
}

#[test]
fn archiving_checked_out_branch_with_no_upstream_lands_on_fallback_main() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base");
    git(dir, &["checkout", "-q", "-b", "feature"]);
    commit(dir, "c1.txt", "c1");
    // HEAD is on feature, and it has no upstream. 'main' exists.

    let repo = Repo::discover(dir).unwrap();
    let undo = archive::archive(&repo, &ModelOptions::default(), &["feature".to_string()])
        .unwrap()
        .expect("refs moved");

    assert_eq!(head_branch(dir), "main");
    assert_eq!(local_branches(dir), vec!["main".to_string()]);

    reshape::undo(&repo, &undo).unwrap();
    assert_eq!(head_branch(dir), "feature");
}

#[test]
fn archiving_branch_using_full_ref_name_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base");
    git(dir, &["checkout", "-q", "-b", "feature"]);
    commit(dir, "c1.txt", "c1");
    git(dir, &["checkout", "-q", "main"]);

    let repo = Repo::discover(dir).unwrap();
    let undo = archive::archive(
        &repo,
        &ModelOptions::default(),
        &["refs/heads/feature".to_string()],
    )
    .unwrap()
    .expect("refs moved");

    assert_eq!(local_branches(dir), vec!["main".to_string()]);
    assert_eq!(
        archive_refs(dir)
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>(),
        vec!["feature"]
    );

    reshape::undo(&repo, &undo).unwrap();
    assert_eq!(
        local_branches(dir),
        vec!["feature".to_string(), "main".to_string()]
    );
}

#[test]
fn archiving_checked_out_branch_with_no_upstream_and_full_ref_lands_on_fallback_main() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base.txt", "base");
    git(dir, &["checkout", "-q", "-b", "feature"]);
    commit(dir, "c1.txt", "c1");
    // HEAD is on feature, and it has no upstream. 'main' exists. We use full ref name.

    let repo = Repo::discover(dir).unwrap();
    let undo = archive::archive(
        &repo,
        &ModelOptions::default(),
        &["refs/heads/feature".to_string()],
    )
    .unwrap()
    .expect("refs moved");

    assert_eq!(head_branch(dir), "main");
    assert_eq!(local_branches(dir), vec!["main".to_string()]);

    reshape::undo(&repo, &undo).unwrap();
    assert_eq!(head_branch(dir), "feature");
}
