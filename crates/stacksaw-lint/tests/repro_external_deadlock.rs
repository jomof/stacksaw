use stacksaw_lint::{ExternalLinter, LintJob, FileChange, Profile, Linter};
use std::time::Duration;
use std::path::PathBuf;

#[test]
fn test_external_linter_deadlock() {
    // ARRANGE
    let mut linter = ExternalLinter::default();
    // A script that fills the stderr buffer before reading stdin
    linter.command = "python3".to_string();
    linter.args = vec!["-c".to_string(), "import sys; sys.stderr.write('X' * 100000); sys.stderr.flush(); sys.stdin.read()".to_string()];
    linter.timeout = Duration::from_secs(2);

    let job = LintJob {
        commit: "abc1234".to_string(),
        author_year: 2026,
        message: "test".to_string(),
        files: vec![FileChange {
            path: "test.rs".to_string(),
            old_oid: None,
            new_oid: None,
            changed_ranges: vec![],
            content: Some("X".repeat(100000)), // Large stdin to trigger deadlock
            added: true,
        }],
        repo_root: PathBuf::from("."),
        worktree: PathBuf::from("."),
        profile: Profile::Local,
    };

    // ACT & ASSERT
    // This will hang indefinitely if the deadlock is present. 
    // The test runner (timeout) will catch the hang.
    let _ = linter.run(&job);
}
