use stacksaw_git::Repo;

#[test]
fn test_patch_ids_e2big() {
    // ARRANGE
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .unwrap();
    let repo = Repo::discover(dir).unwrap();

    // Create 100,000 fake oids (approx 4.1MB of arguments, exceeding 2MB ARG_MAX)
    let oids: Vec<String> = (0..100000).map(|i| format!("{:040}", i)).collect();

    // ACT
    let result = repo.patch_ids(&oids);

    // ASSERT
    match result {
        Err(e) => {
            let err_str = format!("{:?}", e);
            assert!(
                err_str.contains("Argument list too long") || err_str.contains("os error 7"),
                "Expected E2BIG, got {:?}",
                e
            );
        }
        Ok(_) => {
            panic!("Expected E2BIG, but command succeeded. ARG_MAX might be larger than 4MB on this system.");
        }
    }
}
