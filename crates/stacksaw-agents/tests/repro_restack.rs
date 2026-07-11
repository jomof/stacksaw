use stacksaw_agents::restack::{RestackOutcome, Restacker};
use stacksaw_agents::workflow::{ConflictPolicy, FixPolicy, RestackParams};
use stacksaw_git::executor::GitExecutor;
use stacksaw_git::Repo;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_restack_reports_wrong_commit_on_conflict() {
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
    fs::write(repo_dir.join("file.txt"), "line 1\n").unwrap();
    git(&["add", "file.txt"]);
    git(&["commit", "-m", "initial"]);
    let _oid_initial = GitExecutor::new(repo_dir)
        .args(["rev-parse", "HEAD"])
        .run_captured()
        .unwrap()
        .trim()
        .to_string();

    // Branch feat-a off initial
    git(&["checkout", "-b", "feat-a"]);
    fs::write(repo_dir.join("file.txt"), "line 1\nline 2\n").unwrap();
    git(&["add", "file.txt"]);
    git(&["commit", "-m", "feat-a"]);
    let oid_a = GitExecutor::new(repo_dir)
        .args(["rev-parse", "HEAD"])
        .run_captured()
        .unwrap()
        .trim()
        .to_string();

    // Create a new base that conflicts with feat-a
    git(&["checkout", "main"]);
    fs::write(repo_dir.join("file.txt"), "line 1 conflicted\n").unwrap();
    git(&["add", "file.txt"]);
    git(&["commit", "-m", "new-base"]);
    let oid_new_base = GitExecutor::new(repo_dir)
        .args(["rev-parse", "HEAD"])
        .run_captured()
        .unwrap()
        .trim()
        .to_string();

    let repo = Repo::open(repo_dir).unwrap();
    let params = RestackParams {
        staircase: vec!["feat-a".to_string()],
        onto: "main".to_string(),
        fix_policy: FixPolicy::default(),
        conflict_policy: ConflictPolicy::Stop,
        max_attempts: 3,
    };

    let restacker = Restacker::new(&repo, params);
    let outcome = restacker.run().unwrap();

    match outcome {
        RestackOutcome::Paused { commit, .. } => {
            // The bug: it reports the 'onto' side (new base) instead of the commit being applied (oid_a).
            assert_eq!(
                commit, oid_new_base,
                "BUG CONFIRMED: it reported the 'onto' base instead of the conflicted commit"
            );
            assert_ne!(
                commit, oid_a,
                "BUG NOT REPRODUCED: it actually reported the correct commit OID!"
            );
        }
        _ => panic!("Expected restack to pause due to conflict"),
    }
}
