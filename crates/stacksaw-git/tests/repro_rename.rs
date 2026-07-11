use stacksaw_git::executor::GitExecutor;
use stacksaw_git::repo::Repo;
use tempfile::tempdir;

#[test]
fn test_tree_diff_rename_fallback() {
    let tmp = tempdir().unwrap();
    let repo_dir = tmp.path();
    GitExecutor::new(repo_dir)
        .args(["init", "-q", "-b", "main"])
        .status()
        .unwrap();

    std::fs::write(repo_dir.join("old.txt"), "content").unwrap();
    GitExecutor::new(repo_dir)
        .args(["add", "old.txt"])
        .status()
        .unwrap();
    GitExecutor::new(repo_dir)
        .args(["commit", "-m", "initial"])
        .status()
        .unwrap();

    GitExecutor::new(repo_dir)
        .args(["mv", "old.txt", "new.txt"])
        .status()
        .unwrap();
    GitExecutor::new(repo_dir)
        .args(["commit", "-m", "rename"])
        .status()
        .unwrap();

    let repo = Repo::open(repo_dir).unwrap();
    let tip = repo.resolve("HEAD").unwrap();

    let changes_fallback = repo.tree_diff(None, tip).unwrap();
    let rename_entry = changes_fallback.iter().find(|(_, s)| *s == 'R');
    assert!(rename_entry.is_some());
    let (path, _) = rename_entry.unwrap();
    assert_eq!(path, "new.txt");
}
