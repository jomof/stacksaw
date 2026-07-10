//! Applying [`Suggestion`] edit lists to file content — the single code path
//! used by `stacksaw fix`, the UI's `a` binding, and agents (§7.1, §8.5).

use std::{cmp::Reverse, collections::HashMap};

use stacksaw_ssp::types::{Edit, Suggestion};

/// Apply a suggestion to an in-memory view of files (`path → content`). Edits
/// are applied in a single pass from the end of the document to ensure offsets
/// and line numbers stay valid (§7.1).
pub fn apply_suggestion(files: &mut HashMap<String, String>, suggestion: &Suggestion) {
    // Group edits by file so cross-file suggestions apply consistently.
    let mut by_file: HashMap<String, Vec<&Edit>> = HashMap::new();
    for e in &suggestion.edits {
        by_file.entry(e.file.clone()).or_default().push(e);
    }

    for (file, edits) in by_file {
        let content = files.entry(file).or_default();
        *content = apply_file_edits(content, &edits);
    }
}

fn apply_file_edits(original: &str, edits: &[&Edit]) -> String {
    // Convert all edits to byte-range replacements.
    let mut combined: Vec<(usize, usize, String)> = Vec::new();

    for e in edits {
        if let Some(r) = e.range {
            if let (Some(start), Some(end)) = (
                line_col_to_byte(original, r.start.line, r.start.col),
                line_col_to_byte(original, r.end.line, r.end.col),
            ) {
                combined.push((start, end, e.new_text.clone()));
            }
        } else if let Some(after) = e.insert_after_line {
            // "Insert after line N" is a zero-width replacement at the start of line N+1.
            if let Some(offset) = line_start_byte(original, after + 1) {
                let mut text = e.new_text.clone();
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                combined.push((offset, offset, text));
            }
        }
    }

    // Sort by start offset descending so changes don't shift upcoming offsets.
    combined.sort_by_key(|a| Reverse(a.0));

    let mut text = original.to_string();
    for (start, end, new) in combined {
        if start <= end && end <= text.len() {
            text.replace_range(start..end, &new);
        }
    }
    text
}

/// Convert a 1-based line/1-based byte-column position to a byte offset.
fn line_col_to_byte(text: &str, line: u32, col: u32) -> Option<usize> {
    if line == 0 {
        return None;
    }
    let mut offset = 0usize;
    let mut current_line = 1u32;
    for l in text.split_inclusive('\n') {
        if current_line == line {
            let col_bytes = (col.saturating_sub(1)) as usize;
            let line_len = l.trim_end_matches('\n').len();
            return Some(offset + col_bytes.min(line_len));
        }
        offset += l.len();
        current_line += 1;
    }
    // Position past the last line: clamp to end.
    if current_line == line {
        return Some(text.len());
    }
    None
}

/// Byte offset of the start of a 1-indexed line.
fn line_start_byte(text: &str, line: u32) -> Option<usize> {
    if line <= 1 {
        return Some(0);
    }
    let mut offset = 0usize;
    let mut current_line = 1u32;
    for l in text.split_inclusive('\n') {
        offset += l.len();
        current_line += 1;
        if current_line == line {
            return Some(offset);
        }
    }
    // Past last line: end of file.
    if line > current_line {
        return Some(text.len());
    }
    Some(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stacksaw_ssp::types::{Position, Range};

    fn edit_range(file: &str, sl: u32, sc: u32, el: u32, ec: u32, txt: &str) -> Edit {
        Edit {
            file: file.into(),
            range: Some(Range {
                start: Position { line: sl, col: sc },
                end: Position { line: el, col: ec },
            }),
            insert_after_line: None,
            new_text: txt.into(),
        }
    }

    #[test]
    fn replaces_a_range() {
        let mut files = HashMap::new();
        files.insert("A.kt".into(), "val m = com.foo.Bar.baz()\n".to_string());
        // Replace "com.foo.Bar" (cols 9..=19) with "Bar".
        let sug = Suggestion {
            edits: vec![edit_range("A.kt", 1, 9, 1, 20, "Bar")],
        };
        apply_suggestion(&mut files, &sug);
        assert_eq!(files["A.kt"], "val m = Bar.baz()\n");
    }

    #[test]
    fn inserts_after_line() {
        let mut files = HashMap::new();
        files.insert("A.kt".into(), "package x\nclass A\n".to_string());
        let sug = Suggestion {
            edits: vec![Edit {
                file: "A.kt".into(),
                range: None,
                insert_after_line: Some(1),
                new_text: "import java.util.List".into(),
            }],
        };
        apply_suggestion(&mut files, &sug);
        assert_eq!(files["A.kt"], "package x\nimport java.util.List\nclass A\n");
    }

    #[test]
    fn combined_replace_and_insert() {
        let mut files = HashMap::new();
        files.insert(
            "A.kt".into(),
            "package x\nval m: com.foo.Bar = z\n".to_string(),
        );
        let sug = Suggestion {
            edits: vec![
                edit_range("A.kt", 2, 8, 2, 19, "Bar"),
                Edit {
                    file: "A.kt".into(),
                    range: None,
                    insert_after_line: Some(1),
                    new_text: "import com.foo.Bar".into(),
                },
            ],
        };
        apply_suggestion(&mut files, &sug);
        assert_eq!(
            files["A.kt"],
            "package x\nimport com.foo.Bar\nval m: Bar = z\n"
        );
    }

    #[test]
    fn test_drift_when_range_replacement_deletes_lines() {
        let mut files = HashMap::new();
        files.insert(
            "test.txt".into(),
            "line 1\nline 2\nline 3\nline 4\n".to_string(),
        );

        let sug = Suggestion {
            edits: vec![
                // Replace lines 1 and 2 with a single line. (Deletes one \n)
                edit_range("test.txt", 1, 1, 3, 1, "new line 1-2\n"),
                // Insert after original line 3.
                Edit {
                    file: "test.txt".into(),
                    range: None,
                    insert_after_line: Some(3),
                    new_text: "inserted".into(),
                },
            ],
        };

        apply_suggestion(&mut files, &sug);

        // After the fix, even though line 1-2 replacement happens, "insert after 3"
        // still logically refers to the point after "line 3" because we apply
        // from the end of the file using original offsets.
        let expected = "new line 1-2\nline 3\ninserted\nline 4\n";
        assert_eq!(files["test.txt"], expected);
    }
}
