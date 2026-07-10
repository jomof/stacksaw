use std::collections::HashMap;

pub struct NumstatEntry {
    pub path: String,
    pub added: u32,
    pub deleted: u32,
}

pub struct NumstatParser;

impl NumstatParser {
    pub fn parse(out: &str) -> Vec<NumstatEntry> {
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
            entries.push(NumstatEntry {
                path: Self::normalize_path(path),
                added,
                deleted,
            });
        }
        entries
    }

    pub fn parse_to_map(out: &str) -> HashMap<String, (u32, u32)> {
        Self::parse(out)
            .into_iter()
            .map(|e| (e.path, (e.added, e.deleted)))
            .collect()
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
    fn test_normalize_path_simple() {
        assert_eq!(NumstatParser::normalize_path("file.txt"), "file.txt");
    }

    #[test]
    fn test_normalize_path_rename() {
        assert_eq!(
            NumstatParser::normalize_path("old.txt => new.txt"),
            "new.txt"
        );
    }

    #[test]
    fn test_normalize_path_braced_rename() {
        assert_eq!(
            NumstatParser::normalize_path("src/{old => new}/file.txt"),
            "src/new/file.txt"
        );
        assert_eq!(
            NumstatParser::normalize_path("{old => new}/file.txt"),
            "new/file.txt"
        );
        assert_eq!(NumstatParser::normalize_path("src/{old => new}"), "src/new");
    }

    #[test]
    fn test_normalize_path_braced_empty() {
        assert_eq!(
            NumstatParser::normalize_path("dir/{ => new}/file.txt"),
            "dir/new/file.txt"
        );
        assert_eq!(
            NumstatParser::normalize_path("dir/{old => }/file.txt"),
            "dir//file.txt"
        );
    }

    #[test]
    fn test_parse_numstat() {
        let out = "10\t20\tfile.txt\n5\t0\told => new\n-\t-\tbin.dat\n";
        let entries = NumstatParser::parse(out);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, "file.txt");
        assert_eq!(entries[0].added, 10);
        assert_eq!(entries[0].deleted, 20);
        assert_eq!(entries[1].path, "new");
        assert_eq!(entries[1].added, 5);
        assert_eq!(entries[1].deleted, 0);
        assert_eq!(entries[2].path, "bin.dat");
        assert_eq!(entries[2].added, 0);
        assert_eq!(entries[2].deleted, 0);
    }

    #[test]
    fn test_normalize_path_weird_braces() {
        // This should not panic
        NumstatParser::normalize_path("} {");
    }
}
