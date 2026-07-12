use stacksaw_git::executor::GitExecutor;
use stacksaw_git::model::{build_staircases, ModelOptions};
use stacksaw_git::repo::Repo;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_stale_link_recovery_when_child_is_exactly_at_former_tip() {
    let tmp = tempdir().unwrap();
    let repo_dir = tmp.path();
    let git = |args: &[&str]| {
        GitExecutor::new(repo_dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap();
    };

    git(&["init", "-q", "-b", "main"]);
    fs::write(repo_dir.join("file"), "1").unwrap();
    git(&["add", "file"]);
    git(&["commit", "-m", "c1"]);

    git(&["checkout", "-b", "feat/a"]);
    fs::write(repo_dir.join("file"), "2").unwrap();
    git(&["add", "file"]);
    git(&["commit", "-m", "c2"]);

    git(&["checkout", "-b", "feat/b"]); // feat/b is now at c2

    // Manually manage them to link them since git-staircase discovery ignores same-commit parents.
    let git_repo = git_staircase::GitRepo::new(repo_dir.to_path_buf());
    let repo = Repo::open(repo_dir).unwrap();
    let c2_oid = repo.resolve("feat/b").unwrap().to_string();
    let metadata = git_staircase::model::StaircaseMetadata {
        id: "test-staircase-id".to_string(),
        name: "feat".to_string(),
        target: "refs/heads/main".to_string(),
        steps: vec![
            git_staircase::model::Step {
                id: "step-a".to_string(),
                name: "feat/a".to_string(),
                cut: c2_oid.clone(),
                branch: Some("feat/a".to_string()),
            },
            git_staircase::model::Step {
                id: "step-b".to_string(),
                name: "feat/b".to_string(),
                cut: c2_oid,
                branch: Some("feat/b".to_string()),
            },
        ],
        verification_policy: None,
    };
    git_staircase::core::persistence::write_metadata(&git_repo, &metadata).unwrap();

    git(&["checkout", "feat/a"]);
    fs::write(repo_dir.join("file"), "2-amended").unwrap();
    git(&["add", "file"]);
    git(&["commit", "--amend", "-m", "c2-amended"]);

    let opts = ModelOptions::default();
    let staircases = build_staircases(&repo, &opts).unwrap();

    let feat_b_seg = staircases
        .iter()
        .flat_map(|s| &s.segments)
        .find(|seg| seg.branch.leaf() == "feat/b")
        .expect("Should find feat/b");

    assert!(
        feat_b_seg.stale,
        "feat/b should be marked stale even if it is exactly at the former tip"
    );
}
