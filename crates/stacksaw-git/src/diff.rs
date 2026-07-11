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
                let path = Self::normalize_path(raw_path);
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
                // parse_combined_status currently relies on ' => ' in numstat for Renamed status.
            }
        }
        entries
    }

    /// Parse the output of 'git show --name-status' or 'git diff --name-status'.
    /// Returns (path, status).
    pub fn parse_name_status(out: &str) -> Vec<(String, FileStatus)> {
        let mut file_specs = Vec::new();
        for line in out.lines() {
            let mut parts = line.split('\t');
            let Some(status_str) = parts.next() else {
                continue;
            };
            let Some(path) = parts.next() else { continue };

            let status = FileStatus::from(status_str.chars().next().unwrap_or('M'));
            let final_path = if status == FileStatus::Renamed || status == FileStatus::Copied {
                parts.next().unwrap_or(path).to_string()
            } else {
                path.to_string()
            };

            file_specs.push((final_path, status));
        }
        file_specs
    }

    /// Parse the output of 'git diff --numstat' (without --summary).
    pub fn parse_numstat(out: &str) -> Vec<FileEntry> {
        let mut entries = Vec::new();
        for line in out.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split('\t');
            let (Some(a), Some(d), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let added = a.parse::<u32>().unwrap_or(0);
            let deleted = d.parse::<u32>().unwrap_or(0);
            let path = Self::normalize_path(path);
            let status = if path.contains(" => ") {
                FileStatus::Renamed
            } else {
                FileStatus::Modified
            };
            entries.push(FileEntry {
                path,
                added,
                deleted,
                status,
            });
        }
        entries
    }

    pub fn normalize_path(path: &str) -> String {
        if let Some(open) = path.find('{') {
            if let Some(close) = path.find('}') {
                if open < close {
                    if let Some(arrow) = path[open..close].find(" => ") {
                        let mid_start = open + arrow + " => ".len();
                        let new_mid = &path[mid_start..close];
                        return format!("{}{}{}", &path[..open], new_mid, &path[close + 1..]);
                    }
                }
            }
        }
        if let Some((_, new)) = path.split_once(" => ") {
            return new.to_string();
        }
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_combined_status_complex() {
        // ARRANGE
        let out = r#"
10	5	added.txt
0	0	deleted.txt
1	1	modified.txt
3	2	old.txt => new.txt
create mode 100644 added.txt
delete mode 100644 deleted.txt
rename old.txt => new.txt (80%)
"#;
        // ACT
        let entries = DiffProcessor::parse_combined_status(out);

        // ASSERT
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
    fn test_parse_name_status_renames() {
        // ARRANGE
        let out = "A\tadded.rs\nM\tmodified.rs\nD\tdeleted.rs\nR100\told.rs\tnew.rs\nC80\torig.rs\tcopy.rs\n";

        // ACT
        let specs = DiffProcessor::parse_name_status(out);

        // ASSERT
        assert_eq!(specs.len(), 5);
        assert_eq!(specs[0], ("added.rs".to_string(), FileStatus::Added));
        assert_eq!(specs[1], ("modified.rs".to_string(), FileStatus::Modified));
        assert_eq!(specs[2], ("deleted.rs".to_string(), FileStatus::Deleted));
        assert_eq!(specs[3], ("new.rs".to_string(), FileStatus::Renamed));
        assert_eq!(specs[4], ("copy.rs".to_string(), FileStatus::Copied));
    }

    #[test]
    fn test_normalize_path_braced_rename() {
        assert_eq!(
            DiffProcessor::normalize_path("src/{old => new}/file.txt"),
            "src/new/file.txt"
        );
        assert_eq!(
            DiffProcessor::normalize_path("{old => new}/file.txt"),
            "new/file.txt"
        );
    }
}
