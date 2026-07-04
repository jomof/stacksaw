//! The `RenderSurface` seam (§12).
//!
//! Layout is expressed in terms of abstract cells whose colors are OKLCH
//! (Rainbox), independent of any backend. The ratatui backend is one
//! implementation; a future `--gui` wgpu renderer would be another. Keeping
//! this seam is a MUST per §12, even though the pixel renderer is a non-goal.

use stacksaw_rainbox::{Background, RainboxColor};

/// A styled span of text placed by the layout, with an abstract foreground
/// color in OKLCH and a relevance for dimming.
#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub color: Option<RainboxColor>,
    pub relevance: f32,
    pub selected: bool,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Span {
            text: text.into(),
            color: None,
            relevance: 1.0,
            selected: false,
        }
    }

    pub fn colored(text: impl Into<String>, color: RainboxColor, relevance: f32) -> Self {
        Span {
            text: text.into(),
            color: Some(color),
            relevance,
            selected: false,
        }
    }

    /// Resolve to concrete 8-bit RGB, applying dimming/selection against the
    /// detected background. This is the only place the abstract scene becomes
    /// backend colors.
    pub fn resolve_rgb(&self, bg: Background) -> Option<(u8, u8, u8)> {
        let color = self.color?;
        let resolved = if self.selected {
            color.selected()
        } else {
            color.dimmed(self.relevance, bg)
        };
        Some(resolved.to_rgb())
    }
}

/// A logical row emitted by a column's layout.
#[derive(Debug, Clone, Default)]
pub struct SurfaceRow {
    pub spans: Vec<Span>,
}

impl SurfaceRow {
    pub fn push(&mut self, span: Span) {
        self.spans.push(span);
    }
}
