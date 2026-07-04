//! Applying [`Suggestion`] edit lists to file content — the single code path
//! used by `stacksaw fix`, the UI's `a` binding, and agents (§7.1, §8.5).

use std::collections::HashMap;

use stacksaw_ssp::types::{Edit, Suggestion};

/// Apply a suggestion to an in-memory view of files (`path → content`). Edits
/// that target a range are applied first (in reverse document order so offsets
/// stay valid), then line insertions.
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
    // Phase 1: range replacements, applied from the end of the document.
    let mut replacements: Vec<(usize, usize, &str)> = edits
        .iter()
        .filter_map(|e| {
            let r = e.range?;
            let start = line_col_to_byte(original, r.start.line, r.start.col)?;
            let end = line_col_to_byte(original, r.end.line, r.end.col)?;
            Some((start, end, e.new_text.as_str()))
        })
        .collect();
    replacements.sort_by(|a, b| b.0.cmp(&a.0));

    let mut text = original.to_string();
    for (start, end, new) in replacements {
        if start <= end && end <= text.len() {
            text.replace_range(start..end, new);
        }
    }

    // Phase 2: line insertions (after N; 0 = top of file).
    let mut insertions: Vec<(u32, &str)> = edits
        .iter()
        .filter_map(|e| e.insert_after_line.map(|l| (l, e.new_text.as_str())))
        .collect();
    if insertions.is_empty() {
        return text;
    }
    insertions.sort_by(|a, b| b.0.cmp(&a.0));

    let mut lines: Vec<String> = text.split_inclusive('\n').map(|s| s.to_string()).collect();
    for (after, new) in insertions {
        let idx = (after as usize).min(lines.len());
        let insert = if new.ends_with('\n') {
            new.to_string()
        } else {
            format!("{new}\n")
        };
        // Ensure the preceding line ends with a newline.
        if idx > 0 {
            if let Some(prev) = lines.get_mut(idx - 1) {
                if !prev.ends_with('\n') {
                    prev.push('\n');
                }
            }
        }
        lines.insert(idx, insert);
    }
    lines.concat()
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
}
