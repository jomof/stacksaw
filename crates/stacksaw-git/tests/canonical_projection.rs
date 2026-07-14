use std::fs;
use std::path::Path;

use stacksaw_git::executor::GitExecutor;
use stacksaw_git::{build_snapshot, Repo};
use stacksaw_git::model::ModelOptions;
use stacksaw_ssp::types::{RepresentationKind, StructuralState};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = GitExecutor::new(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str) -> String {
    fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", file]);
    git(dir, &["rev-parse", "HEAD"])
}

fn init(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base");
}

fn opts() -> ModelOptions {
    ModelOptions {
        default_upstream: Some("refs/heads/main".into()),
    }
}

#[test]
fn managed_projection_carries_canonical_revisions_and_steps() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    init(dir);
    git(dir, &["checkout", "-qb", "feature-1"]);
    let first = commit(dir, "one");
    git(dir, &["checkout", "-qb", "feature"]);
    let second = commit(dir, "two");
    let canonical = git_staircase::GitRepo::new(dir.to_path_buf());
    let discovered = git_staircase::core::resolve_staircase(
        &canonical,
        "feature",
        Some("refs/heads/main"),
    )
    .unwrap()
    .unwrap();
    let managed = git_staircase::core::adopt(&canonical, discovered.metadata()).unwrap();

    let repo = Repo::discover(dir).unwrap();
    let snapshot = build_snapshot(&repo, 7, &opts()).unwrap();
    let staircase = snapshot
        .staircases
        .iter()
        .find(|staircase| {
            staircase.selector.lineage_id.as_deref() == Some(managed.id.as_str())
        })
        .unwrap();
    assert_eq!(staircase.representation, RepresentationKind::Managed);
    assert!(staircase.record_revision.is_some());
    assert!(staircase.structure_revision.is_some());
    assert_eq!(staircase.integration.target, "refs/heads/main");
    assert_eq!(staircase.structural_state, StructuralState::Clean);
    assert_eq!(
        staircase
            .segments
            .iter()
            .map(|segment| segment.cut.as_str())
            .collect::<Vec<_>>(),
        vec![first, second]
    );
    assert!(staircase
        .segments
        .iter()
        .all(|segment| segment.step_id.is_some()));
}

#[test]
fn partial_landing_preserves_lineage_and_remaining_decomposition() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    init(dir);
    git(dir, &["checkout", "-qb", "feature-1"]);
    let landed = commit(dir, "one");
    git(dir, &["checkout", "-qb", "feature"]);
    let remaining = commit(dir, "two");
    let canonical = git_staircase::GitRepo::new(dir.to_path_buf());
    let discovered = git_staircase::core::resolve_staircase(
        &canonical,
        "feature",
        Some("refs/heads/main"),
    )
    .unwrap()
    .unwrap();
    let managed = git_staircase::core::adopt(&canonical, discovered.metadata()).unwrap();
    git(dir, &["update-ref", "refs/heads/main", &landed]);

    let repo = Repo::discover(dir).unwrap();
    let snapshot = build_snapshot(&repo, 1, &opts()).unwrap();
    let staircase = snapshot
        .staircases
        .iter()
        .find(|staircase| {
            staircase.selector.lineage_id.as_deref() == Some(managed.id.as_str())
        })
        .unwrap();
    assert!(staircase.segments[0].commits.is_empty());
    assert_eq!(
        staircase.segments[1].commits.last().unwrap().oid,
        remaining
    );
}

#[test]
fn forked_discovery_lists_canonical_family_paths() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    init(dir);
    git(dir, &["checkout", "-qb", "root"]);
    commit(dir, "root");
    git(dir, &["checkout", "-qb", "left"]);
    commit(dir, "left");
    git(dir, &["checkout", "root"]);
    git(dir, &["checkout", "-qb", "right"]);
    commit(dir, "right");

    let repo = Repo::discover(dir).unwrap();
    let snapshot = build_snapshot(&repo, 1, &opts()).unwrap();
    let family_paths = snapshot
        .staircases
        .iter()
        .filter(|staircase| staircase.representation == RepresentationKind::FamilyPath)
        .collect::<Vec<_>>();
    assert_eq!(family_paths.len(), 2);
    assert!(family_paths
        .iter()
        .all(|path| path.selector.path_id.is_some()));
}

#[test]
fn detached_checkout_is_context_not_a_synthetic_staircase() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    init(dir);
    git(dir, &["checkout", "--detach", "-q", "HEAD"]);

    let repo = Repo::discover(dir).unwrap();
    let snapshot = build_snapshot(&repo, 1, &opts()).unwrap();
    assert!(snapshot.detached);
    assert!(snapshot.checkout.as_ref().unwrap().detached);
    assert!(snapshot.staircases.is_empty());
}
