//! The Diff viewport contributor: parses a unified diff (or raw file) into
//! cached, syntax-highlighted rows. It is data-only — `app::draw_diff` renders
//! it, since that needs the app's theme and selection context for placeholders.

use ratatui::style::Color;

use crate::highlight::Highlighter;

/// Context rows kept above the first change when a full-file diff opens (§8.5).
const DIFF_CONTEXT_ABOVE: u16 = 3;

/// Whether a diff row is unchanged, added, or deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Context,
    Add,
    Del,
}

/// One rendered diff row: change kind, before/after line numbers, and
/// syntax-highlighted text segments (marker already stripped).
pub struct DiffRow {
    pub kind: DiffKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub spans: Vec<(Color, String)>,
}

/// The singleton Diff contributor. Holds the loaded diff data; `app::draw_diff`
/// renders it (it needs the app's theme and selection context for placeholders).
#[derive(Default)]
pub struct DiffView {
    pub rows: Vec<DiffRow>,
    pub is_raw: bool,
    pub is_message: bool,
    pub loaded_key: Option<(String, String)>,
    pub scroll: u16,
}

impl DiffView {
    /// Parse `text` into cached, highlighted rows for `(oid, path)`.
    pub fn set_diff(
        &mut self,
        oid: String,
        path: String,
        text: &str,
        raw: bool,
        is_message: bool,
        truecolor: bool,
        syntax_theme: &str,
    ) {
        let mut hl = Highlighter::for_path(&path, truecolor, syntax_theme);
        let mut rows = Vec::new();
        let mut old_no: u32 = 0;
        let mut new_no: u32 = 0;
        for line in text.lines() {
            if raw {
                new_no += 1;
                rows.push(DiffRow {
                    kind: DiffKind::Context,
                    old: None,
                    new: Some(new_no),
                    spans: hl.line(line),
                });
                continue;
            }
            if let Some((old_start, new_start)) = parse_hunk_header(line) {
                old_no = old_start;
                new_no = new_start;
                continue;
            }
            if is_diff_meta(line) {
                continue;
            }
            let (kind, body, old, new) = match line.as_bytes().first() {
                Some(b'+') => {
                    let n = new_no;
                    new_no += 1;
                    (DiffKind::Add, &line[1..], None, Some(n))
                }
                Some(b'-') => {
                    let o = old_no;
                    old_no += 1;
                    (DiffKind::Del, &line[1..], Some(o), None)
                }
                Some(b' ') => {
                    let (o, n) = (old_no, new_no);
                    old_no += 1;
                    new_no += 1;
                    (DiffKind::Context, &line[1..], Some(o), Some(n))
                }
                _ => {
                    let (o, n) = (old_no, new_no);
                    old_no += 1;
                    new_no += 1;
                    (DiffKind::Context, line, Some(o), Some(n))
                }
            };
            rows.push(DiffRow {
                kind,
                old,
                new,
                spans: hl.line(body),
            });
        }
        self.rows = rows;
        self.is_raw = raw;
        self.is_message = is_message;
        self.loaded_key = Some((oid, path));
        self.scroll = if raw {
            0
        } else {
            self.first_change_scroll(DIFF_CONTEXT_ABOVE)
        };
    }

    fn first_change_scroll(&self, context: u16) -> u16 {
        for (body, row) in self.rows.iter().enumerate() {
            if row.kind != DiffKind::Context {
                return (body as u16).saturating_sub(context);
            }
        }
        0
    }

    pub fn on_scroll(&mut self, down: bool) {
        let last = self.rows.len().saturating_sub(1) as u16;
        self.scroll = if down {
            (self.scroll + 3).min(last)
        } else {
            self.scroll.saturating_sub(3)
        };
    }
}

/// Parse a unified-diff hunk header `@@ -old[,n] +new[,m] @@ ...`, returning the
/// 1-based starting line numbers `(old, new)`.
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ ")?;
    let mut fields = rest.split(' ');
    let old = fields.next()?.strip_prefix('-')?;
    let new = fields.next()?.strip_prefix('+')?;
    let old_start = old.split(',').next()?.parse().ok()?;
    let new_start = new.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

fn is_diff_meta(line: &str) -> bool {
    line.starts_with("diff ")
        || line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("@@")
        || line.starts_with("new file")
        || line.starts_with("deleted file")
        || line.starts_with("old mode")
        || line.starts_with("new mode")
        || line.starts_with("similarity ")
        || line.starts_with("rename ")
        || line.starts_with("copy ")
        || line.starts_with("\\ No newline")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_view_parses_a_patch() {
        let mut d = DiffView::default();
        d.set_diff(
            "oid".into(),
            "src/lib.rs".into(),
            "diff --git a b\n@@ -1,2 +1,2 @@\n-old\n+new\n ctx\n",
            false,
            false,
            false,
            "base16-ocean.dark",
        );
        let kinds: Vec<DiffKind> = d.rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![DiffKind::Del, DiffKind::Add, DiffKind::Context]);
        assert_eq!(d.loaded_key.as_ref().map(|(o, _)| o.as_str()), Some("oid"));
    }
}
