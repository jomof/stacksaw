use std::fs;
use std::path::Path;

use stacksaw_core::config::Config;
use stacksaw_core::Core;
use stacksaw_git::executor::GitExecutor;
use stacksaw_ssp::method::ClientKind;
use stacksaw_ssp::types::{MutatePlan, RepresentationKind};

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

#[tokio::test]
async fn split_join_archive_and_undo_use_canonical_identity_and_leases() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "base");
    git(dir, &["checkout", "-qb", "feature"]);
    let split_at = commit(dir, "one");
    commit(dir, "two");
    commit(dir, "three");
    git(dir, &["branch", "--set-upstream-to=main", "feature"]);

    let core = Core::attach_or_local(
        dir.to_path_buf(),
        dir.join(".git"),
        Config::default(),
        ClientKind::Cli,
    )
    .await
    .unwrap();
    let implicit = core.snapshot().await.unwrap();
    let staircase = &implicit.staircases[0];
    assert_eq!(staircase.representation, RepresentationKind::Implicit);
    core.mutate(
        MutatePlan::Name {
            selector: staircase.selector.clone(),
            name: "feature".into(),
        },
        Some(implicit.generation),
    )
    .await
    .unwrap();

    let managed = core.snapshot().await.unwrap();
    let staircase = &managed.staircases[0];
    let original_lineage = staircase.selector.lineage_id.clone().unwrap();
    let step_id = staircase.segments[0].step_id.clone().unwrap();
    core.mutate(
        MutatePlan::Split {
            selector: staircase.selector.clone(),
            expected_record_revision: staircase.record_revision.clone(),
            step_id,
            at_commit: split_at,
            new_step_name: Some("feature-base".into()),
            no_ref: false,
        },
        Some(managed.generation),
    )
    .await
    .unwrap();

    let split = core.snapshot().await.unwrap();
    let staircase = &split.staircases[0];
    assert_eq!(
        staircase.selector.lineage_id.as_deref(),
        Some(original_lineage.as_str())
    );
    assert_eq!(staircase.segments.len(), 2);
    let lower = staircase.segments[0].step_id.clone().unwrap();
    let upper = staircase.segments[1].step_id.clone().unwrap();
    core.mutate(
        MutatePlan::Join {
            selector: staircase.selector.clone(),
            expected_record_revision: staircase.record_revision.clone(),
            lower_step_id: lower,
            upper_step_id: upper,
            keep_retired_ref: false,
        },
        Some(split.generation),
    )
    .await
    .unwrap();

    let joined = core.snapshot().await.unwrap();
    let staircase = &joined.staircases[0];
    assert_eq!(staircase.segments.len(), 1);
    let stale_record = split.staircases[0].record_revision.clone();
    assert!(core
        .mutate(
            MutatePlan::CanonicalArchive {
                selector: staircase.selector.clone(),
                expected_record_revision: stale_record,
                reason: None,
            },
            Some(joined.generation),
        )
        .await
        .is_err());

    let archive = core
        .mutate(
            MutatePlan::CanonicalArchive {
                selector: staircase.selector.clone(),
                expected_record_revision: staircase.record_revision.clone(),
                reason: Some("test".into()),
            },
            Some(joined.generation),
        )
        .await
        .unwrap();
    assert!(core.snapshot().await.unwrap().staircases.is_empty());
    core.undo(Some(&archive.checkpoint)).await.unwrap();
    let restored = core.snapshot().await.unwrap();
    assert_eq!(
        restored.staircases[0].selector.lineage_id.as_deref(),
        Some(original_lineage.as_str())
    );
}
