//! Syntax highlighting for the Diff column (§8.5).
//!
//! Uses `syntect`, which preships a large corpus of Sublime/TextMate grammars
//! and themes. Assets load once (lazily) and are shared; a [`Highlighter`]
//! carries parser state across the lines of a file so multi-line constructs
//! (block comments, strings) colorize correctly.
//!
//! Kotlin is not part of syntect's default corpus, so we bundle a
//! public-domain `Kotlin.sublime-syntax` (see `assets/Kotlin.LICENSE.md`) and
//! fold it into the default set at load time — important since stacksaw is a
//! Kotlin-centric tool.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use ratatui::style::Color;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::syntax_definition::SyntaxDefinition;
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// The bundled Kotlin grammar, embedded at compile time.
const KOTLIN_SYNTAX: &str = include_str!("../assets/Kotlin.sublime-syntax");

struct Assets {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| Assets {
        syntaxes: syntaxes(),
        themes: ThemeSet::load_defaults(),
    })
}

/// The default syntect corpus plus our bundled Kotlin grammar. The defaults are
/// loaded in their newline-terminated form (matching how [`Highlighter::line`]
/// feeds lines), and Kotlin is added with `lines_include_newline = true` to
/// stay consistent. A malformed bundled grammar is dropped rather than
/// panicking, so highlighting degrades to "Kotlin renders as plain text".
fn syntaxes() -> SyntaxSet {
    let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
    match SyntaxDefinition::load_from_str(KOTLIN_SYNTAX, true, Some("Kotlin")) {
        Ok(def) => {
            builder.add(def);
        }
        Err(err) => {
            tracing::warn!("bundled Kotlin grammar failed to load: {err}");
        }
    }
    builder.build()
}

/// Resolve `name` to a shared, `'static` syntect theme, resolving each distinct
/// name once. `syntect`'s `HighlightLines` borrows its theme for `'static`, so
/// caching here lets a `Highlighter` outlive its build call without leaking a
/// fresh clone per diff load. Falls back to a bundled dark theme (then any) if
/// the name is missing, so highlighting never fails on a bad name.
fn static_theme(name: &str) -> &'static Theme {
    static CACHE: OnceLock<Mutex<HashMap<String, &'static Theme>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().expect("theme cache lock");
    if let Some(theme) = map.get(name) {
        return theme;
    }
    let themes = &assets().themes;
    let resolved = themes
        .themes
        .get(name)
        .or_else(|| themes.themes.get("base16-ocean.dark"))
        .or_else(|| themes.themes.get("base16-eighties.dark"))
        .or_else(|| themes.themes.values().next())
        .expect("syntect ships default themes")
        .clone();
    let leaked: &'static Theme = Box::leak(Box::new(resolved));
    map.insert(name.to_string(), leaked);
    leaked
}

/// A per-file highlighter. Build one with [`Highlighter::for_path`], then feed
/// it the file's lines in order via [`Highlighter::line`].
pub struct Highlighter {
    hl: HighlightLines<'static>,
    truecolor: bool,
}

impl Highlighter {
    /// Build a highlighter for `path` (matched by file extension) using the
    /// syntect theme named by `theme` (from the UI theme). Unknown or
    /// extension-less paths (e.g. the commit-message row) fall back to plain
    /// text, which simply yields the theme's default foreground.
    pub fn for_path(path: &str, truecolor: bool, theme: &str) -> Self {
        let a = assets();
        let syntax = syntax_for_path(&a.syntaxes, path);
        Highlighter {
            hl: HighlightLines::new(syntax, static_theme(theme)),
            truecolor,
        }
    }

    /// Highlight one line (no trailing newline needed) into colored segments.
    /// On any parse error the whole line is returned as a single uncolored span
    /// so rendering can never fail.
    pub fn line(&mut self, text: &str) -> Vec<(Color, String)> {
        let a = assets();
        // syntect wants a trailing newline for correct state transitions.
        let owned = format!("{text}\n");
        match self.hl.highlight_line(&owned, &a.syntaxes) {
            Ok(ranges) => ranges
                .into_iter()
                .map(|(style, piece)| {
                    let piece = piece.strip_suffix('\n').unwrap_or(piece);
                    (to_color(style.foreground, self.truecolor), piece.to_string())
                })
                .filter(|(_, s)| !s.is_empty())
                .collect(),
            Err(_) => vec![(Color::Reset, text.to_string())],
        }
    }
}

/// Resolve a syntax by the path's file extension, else plain text.
fn syntax_for_path<'a>(set: &'a SyntaxSet, path: &str) -> &'a SyntaxReference {
    let file = path.rsplit('/').next().unwrap_or(path);
    // Try the extension first, then the whole file name (handles `Makefile`,
    // `Dockerfile`, …), then plain text.
    file.rsplit_once('.')
        .and_then(|(_, ext)| set.find_syntax_by_extension(ext))
        .or_else(|| set.find_syntax_by_extension(file))
        .unwrap_or_else(|| set.find_syntax_plain_text())
}

/// Convert a syntect color to a ratatui color, honoring terminal depth.
fn to_color(c: syntect::highlighting::Color, truecolor: bool) -> Color {
    if truecolor {
        Color::Rgb(c.r, c.g, c.b)
    } else {
        Color::Indexed(rgb_to_ansi256(c.r, c.g, c.b))
    }
}

/// Map a 24-bit color to the nearest xterm-256 palette index (6×6×6 cube plus
/// the grayscale ramp), for terminals without truecolor.
fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return (232 + (r - 8) * 24 / 247) as u8;
    }
    let idx = |v: i32| -> i32 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    };
    (16 + 36 * idx(r) + 6 * idx(g) + idx(b)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_source_is_tokenized_into_colored_spans() {
        let mut hl = Highlighter::for_path("src/lib.rs", true, "base16-ocean.dark");
        let spans = hl.line("fn main() { let x = 1; }");
        // Multiple distinct tokens (keyword vs identifier vs punctuation).
        assert!(spans.len() > 1, "expected several colored spans, got {spans:?}");
        // The concatenated text is preserved exactly.
        let joined: String = spans.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(joined, "fn main() { let x = 1; }");
        // At least two different foreground colors were assigned.
        let mut colors: Vec<_> = spans.iter().map(|(c, _)| *c).collect();
        colors.dedup();
        assert!(colors.len() > 1, "expected >1 color, got {colors:?}");
    }

    #[test]
    fn kotlin_source_is_tokenized_into_colored_spans() {
        let mut hl = Highlighter::for_path("src/Main.kt", true, "base16-ocean.dark");
        let spans = hl.line("fun main() { val x = 1 }");
        // Text is preserved exactly.
        let joined: String = spans.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(joined, "fun main() { val x = 1 }");
        // The bundled Kotlin grammar tokenizes keywords/identifiers distinctly,
        // so we expect more than one color (plain text would yield one).
        let mut colors: Vec<_> = spans.iter().map(|(c, _)| *c).collect();
        colors.dedup();
        assert!(colors.len() > 1, "expected >1 color for Kotlin, got {colors:?}");
    }

    #[test]
    fn kotlin_grammar_is_registered() {
        let set = super::syntaxes();
        let kt = set.find_syntax_by_extension("kt");
        assert!(kt.is_some(), "Kotlin syntax should be registered for .kt");
        assert_eq!(kt.unwrap().name, "Kotlin");
    }

    #[test]
    fn unknown_extension_falls_back_to_plain_text() {
        // Should not panic and should preserve the text verbatim.
        let mut hl = Highlighter::for_path("commit message", true, "base16-ocean.dark");
        let spans = hl.line("Add codec");
        let joined: String = spans.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(joined, "Add codec");
    }

    #[test]
    fn non_truecolor_yields_indexed_colors() {
        let mut hl = Highlighter::for_path("x.rs", false, "base16-ocean.dark");
        let spans = hl.line("let y = 2;");
        assert!(spans.iter().all(|(c, _)| matches!(c, Color::Indexed(_))));
    }
}
