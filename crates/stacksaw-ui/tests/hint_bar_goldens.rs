//! Golden for the contextual hint bar (§8.2).
//!
//! For every focusable column this renders the hint bar's priority-ordered
//! command list and the width cutoff, then compares it to a source-controlled
//! text file (`tests/goldens/hint_bar.txt`) so the ordering and overflow
//! behavior can be eyeballed in review.
//!
//! Regenerate after intentional changes with:
//!   UPDATE_GOLDENS=1 cargo test -p stacksaw-ui --test hint_bar_goldens

use std::env;
use std::fs;
use std::path::PathBuf;

use stacksaw_ui::command::{self, Focus, HintItem, StacksRow, ViewportKind};
use stacksaw_ui::ColumnKind;

/// The hard-coded column budget the golden renders against. Picked to mimic a
/// narrower-than-full terminal so the busy contexts overflow and the cutoff
/// divider lands mid-list where it's meaningful.
const GOLDEN_WIDTH: usize = 200;

/// Separator width in the default theme: `" · "`.
const SEP_W: usize = 3;

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/hint_bar.txt")
}

fn fmt_hint(item: &HintItem, note: &str) -> String {
    let line = format!("  {:<8} {}", item.keys, item.label);
    if note.is_empty() {
        line
    } else {
        format!("{line}  {note}")
    }
}

fn render_goldens() -> String {
    let mut out = String::new();
    out.push_str("Contextual hint bar — priority order & width cutoff\n");
    out.push_str(&format!(
        "Rendered at {GOLDEN_WIDTH} columns (separator \" · \" = {SEP_W}).\n",
    ));
    out.push_str("Above the divider: shown left->right (Help is pinned to the far\n");
    out.push_str("right; `…` marks overflow). Below: hints that fall off the end.\n");
    out.push_str("The divider is exactly the column budget wide.\n\n");

    // Every reachable focus; Stacks splits into its two selection sub-contexts.
    let contexts: Vec<(String, Focus)> = vec![
        (
            "Stacks · branch/staircase selected".to_string(),
            Focus::stacks(StacksRow::Staircase),
        ),
        (
            "Stacks · recent repo selected".to_string(),
            Focus::stacks(StacksRow::Recent),
        ),
        ("Commits".to_string(), Focus::column(ColumnKind::Commits)),
        ("Files".to_string(), Focus::column(ColumnKind::Files)),
        (
            "Viewport · diff tab active".to_string(),
            Focus::diff(ViewportKind::Diff),
        ),
        (
            "Viewport · run tab active".to_string(),
            Focus::diff(ViewportKind::Run),
        ),
        ("Checks".to_string(), Focus::column(ColumnKind::Checks)),
    ];

    for (label, focus) in contexts {
        let fit = command::fit_hints(focus, GOLDEN_WIDTH, SEP_W);
        out.push_str(&format!("=== {label} ===\n"));

        for item in &fit.shown {
            out.push_str(&fmt_hint(item, ""));
            out.push('\n');
        }
        if fit.truncated {
            out.push_str("  …\n");
        }
        if let Some(help) = &fit.pinned {
            out.push_str(&fmt_hint(help, "(pinned)"));
            out.push('\n');
        }

        out.push_str(&"-".repeat(GOLDEN_WIDTH));
        out.push('\n');

        if fit.dropped.is_empty() {
            out.push_str("  (nothing dropped)\n");
        } else {
            for item in &fit.dropped {
                out.push_str(&fmt_hint(item, ""));
                out.push('\n');
            }
        }
        out.push('\n');
    }
    out
}

#[test]
fn hint_bar_matches_golden() {
    let actual = render_goldens();
    let path = golden_path();

    if env::var_os("UPDATE_GOLDENS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &actual).unwrap();
        return;
    }

    let expected = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; regenerate with `UPDATE_GOLDENS=1 cargo test -p stacksaw-ui --test hint_bar_goldens`",
            path.display()
        )
    });

    assert_eq!(
        actual, expected,
        "hint bar golden drifted; review the change and, if intended, regenerate with \
         `UPDATE_GOLDENS=1 cargo test -p stacksaw-ui --test hint_bar_goldens`"
    );
}
