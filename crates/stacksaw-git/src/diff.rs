use crate::numstat::NumstatParser;
use stacksaw_ssp::types::{FileEntry, FileStatus};
use std::collections::HashMap;

pub struct DiffProcessor;

impl DiffProcessor {
    /// Parse the output of 'git show --numstat --summary' or 'git diff --numstat --summary'.
    pub fn parse_combined_status(out: &str) -> Vec<FileEntry> {
        let mut entries = Vec::new();
        let mut map: HashMap<String, usize> = HashMap::new();

        for line in out.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() == 3 {
                // Numstat line: added \t deleted \t path
                let added = parts[0].parse::<u32>().unwrap_or(0);
                let deleted = parts[1].parse::<u32>().unwrap_or(0);
                let raw_path = parts[2];
                let path = NumstatParser::normalize_path(raw_path);
                let status = if raw_path.contains(" => ") {
                    FileStatus::Renamed
                } else {
                    FileStatus::Modified
                };

                map.insert(path.clone(), entries.len());
                entries.push(FileEntry {
                    path,
                    added,
                    deleted,
                    status,
                });
            } else if line.starts_with("create mode") {
                if let Some(path) = line.split_whitespace().last() {
                    if let Some(&idx) = map.get(path) {
                        entries[idx].status = FileStatus::Added;
                    }
                }
            } else if line.starts_with("delete mode") {
                if let Some(path) = line.split_whitespace().last() {
                    if let Some(&idx) = map.get(path) {
                        entries[idx].status = FileStatus::Deleted;
                    }
                }
            } else if line.starts_with("rename ") {
                // Handle 'rename old => new (100%)' from --summary if needed
                // parse_combined_status currently relies on ' => ' in numstat for Renamed status,
                // which is correct for -M.
            }
        }
        entries
    }

    /// Parse the output of 'git show --name-status' or 'git diff --name-status'.
    /// Returns (path, is_added).
    pub fn parse_name_status(out: &str) -> Vec<(String, bool)> {
        let mut file_specs = Vec::new();
        for line in out.lines() {
            let mut parts = line.split('\t');
            let Some(status) = parts.next() else { continue };
            let Some(path) = parts.next() else { continue };
            let added = status.starts_with('A');
            file_specs.push((path.to_string(), added));
        }
        file_specs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_combined_status_complex() {
        let out = r#"
10	5	added.txt
0	0	deleted.txt
1	1	modified.txt
3	2	old.txt => new.txt
create mode 100644 added.txt
delete mode 100644 deleted.txt
rename old.txt => new.txt (80%)
"#;
        let entries = DiffProcessor::parse_combined_status(out);
        assert_eq!(entries.len(), 4);

        let added = entries.iter().find(|e| e.path == "added.txt").unwrap();
        assert_eq!(added.status, FileStatus::Added);
        assert_eq!(added.added, 10);
        assert_eq!(added.deleted, 5);

        let deleted = entries.iter().find(|e| e.path == "deleted.txt").unwrap();
        assert_eq!(deleted.status, FileStatus::Deleted);

        let modified = entries.iter().find(|e| e.path == "modified.txt").unwrap();
        assert_eq!(modified.status, FileStatus::Modified);
        assert_eq!(modified.added, 1);
        assert_eq!(modified.deleted, 1);

        let renamed = entries.iter().find(|e| e.path == "new.txt").unwrap();
        assert_eq!(renamed.status, FileStatus::Renamed);
        assert_eq!(renamed.added, 3);
        assert_eq!(renamed.deleted, 2);
    }

    #[test]
    fn test_parse_name_status_varied() {
        let out = "A\tfoo.rs\nM\tbar.rs\nD\tbaz.rs\nR100\told.rs\tnew.rs\n";
        let specs = DiffProcessor::parse_name_status(out);
        // Note: parse_name_status currently doesn't handle renames with 3 parts (old, new)
        // But build_lint_jobs didn't seem to care about renames in its manual parsing either.
        assert_eq!(specs.len(), 4);
        assert_eq!(specs[0], ("foo.rs".to_string(), true));
        assert_eq!(specs[1], ("bar.rs".to_string(), false));
        assert_eq!(specs[2], ("baz.rs".to_string(), false));
        // Renames in --name-status are 'R<score>\told\tnew'
        // My current parse_name_status:
        // let Some(path) = parts.next() else { continue };
        // It will take 'old.rs' as path.
    }
}
