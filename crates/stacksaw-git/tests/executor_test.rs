use stacksaw_git::executor::GitExecutor;
use tempfile::tempdir;

#[test]
fn test_git_executor_args() {
    let tmp = tempdir().unwrap();
    let executor = GitExecutor::new(tmp.path())
        .arg("rev-parse")
        .arg("--is-inside-work-tree");

    let cmd = executor.command();
    let args: Vec<_> = cmd.get_args().collect();

    assert_eq!(args[0], "-C");
    assert_eq!(args[1], tmp.path().as_os_str());
    assert_eq!(args[2], "rev-parse");
    assert_eq!(args[3], "--is-inside-work-tree");
}

#[test]
fn test_git_executor_inert() {
    let tmp = tempdir().unwrap();
    let executor = GitExecutor::new(tmp.path()).inert().arg("status");

    let cmd = executor.command();
    let args: Vec<_> = cmd.get_args().collect();

    // -C <path> -c ... -c ... -c ... -c ... status
    assert!(args.iter().any(|&a| a == "core.hooksPath=/dev/null"));
    assert_eq!(args.last().unwrap(), &"status");
}

#[test]
fn test_git_executor_run_captured_success() {
    let tmp = tempdir().unwrap();
    // Initialize a repo
    GitExecutor::new(tmp.path()).arg("init").status().unwrap();

    let executor = GitExecutor::new(tmp.path())
        .arg("rev-parse")
        .arg("--is-inside-work-tree");

    let result = executor.run_captured().unwrap();
    assert_eq!(result, "true");
}

#[test]
fn test_git_executor_run_captured_failure() {
    let tmp = tempdir().unwrap();
    // Not a repo
    let executor = GitExecutor::new(tmp.path())
        .arg("rev-parse")
        .arg("--show-toplevel");

    let result = executor.run_captured();
    assert!(result.is_err());
    if let Err(stacksaw_git::GitError::Command { code, .. }) = result {
        assert_ne!(code, 0);
    } else {
        panic!("Expected GitError::Command, got {:?}", result);
    }
}
