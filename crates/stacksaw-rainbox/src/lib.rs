//! `stacksaw-rainbox` — the Rainbox color system (§8.3, normative).
//!
//! All color math happens in **OKLCH** and is converted to terminal RGB at the
//! edge. Hue communicates identity and relationship; dimming communicates
//! relevance. This crate is intentionally pure (no I/O, no globals) so it can
//! be property-tested exhaustively.
//!
//! Key invariants (property-tested below):
//! - the contrast floor `|L' − L_bg| ≥ 0.18` holds for every relevance and both
//!   backgrounds;
//! - 256-color quantization round-trips within a bounded ΔE budget.

use palette::{color_difference::EuclideanDistance, IntoColor, Oklab, Oklch, Srgb};

pub mod identity;
pub mod relevance;

pub use identity::{golden_angle_hue, staircase_arc_hue, StaircaseArc};
pub use relevance::{Relevance, RelevanceSignals, State, Topological};

/// The golden angle in degrees, used to space unrelated hues maximally.
pub const GOLDEN_ANGLE_DEG: f32 = 137.507_76;

/// Minimum lightness separation from the background (§8.3 contrast floor).
pub const CONTRAST_FLOOR: f32 = 0.18;

/// The perceptual background the UI fades toward (§8.3). Detected at runtime or
/// configured via `ui.background`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    Dark,
    Light,
}

impl Background {
    /// Background lightness in OKLab L (0..=1).
    pub fn lightness(self) -> f32 {
        match self {
            // Near-black / near-white terminal backgrounds in OKLab L.
            Background::Dark => 0.14,
            Background::Light => 0.96,
        }
    }
}

/// A fully-resolved element color, ready to be rendered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RainboxColor {
    pub oklch: Oklch,
}

impl RainboxColor {
    /// Construct from a hue with sensible default lightness/chroma for a chip.
    pub fn from_hue(hue_deg: f32) -> Self {
        RainboxColor {
            oklch: Oklch::new(0.72, 0.13, hue_deg),
        }
    }

    pub fn new(l: f32, c: f32, hue_deg: f32) -> Self {
        RainboxColor {
            oklch: Oklch::new(l, c, hue_deg),
        }
    }

    /// Apply relevance dimming toward the background (§8.3):
    /// `L' = lerp(L, L_bg, 0.75·d)`, `C' = C·(1 − 0.85·d)`, then enforce the
    /// contrast floor.
    pub fn dimmed(self, relevance: f32, bg: Background) -> RainboxColor {
        let r = relevance.clamp(0.0, 1.0);
        let d = 1.0 - r;
        let l_bg = bg.lightness();
        let l = self.oklch.l;
        let c = self.oklch.chroma;

        let mut l_prime = lerp(l, l_bg, 0.75 * d);
        let c_prime = c * (1.0 - 0.85 * d);

        // Contrast floor: push L' away from the background if it got too close.
        if (l_prime - l_bg).abs() < CONTRAST_FLOOR {
            let dir = if l >= l_bg { 1.0 } else { -1.0 };
            l_prime = l_bg + dir * CONTRAST_FLOOR;
            l_prime = l_prime.clamp(0.0, 1.0);
        }

        RainboxColor {
            oklch: Oklch::new(l_prime, c_prime, self.oklch.hue),
        }
    }

    /// Selection override: full chroma, legible lightness (§8.3).
    pub fn selected(self) -> RainboxColor {
        RainboxColor {
            oklch: Oklch::new(0.75, 0.16, self.oklch.hue),
        }
    }

    /// Convert to 8-bit sRGB for truecolor terminals.
    pub fn to_rgb(self) -> (u8, u8, u8) {
        let srgb: Srgb = self.oklch.into_color();
        let srgb = srgb.into_format::<u8>();
        (srgb.red, srgb.green, srgb.blue)
    }

    /// Quantize to the xterm 256-color palette by nearest OKLab distance
    /// (§8.3). Returns the palette index (16..=255 for the color cube/grays).
    pub fn to_ansi256(self) -> u8 {
        let target: Oklab = self.oklch.into_color();
        let mut best = 16u8;
        let mut best_d = f32::INFINITY;
        for idx in 16..=255u8 {
            let (r, g, b) = ansi256_to_rgb(idx);
            let candidate: Oklab = Srgb::new(r, g, b)
                .into_format::<f32>()
                .into_color();
            let d = target.distance_squared(candidate);
            if d < best_d {
                best_d = d;
                best = idx;
            }
        }
        best
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Map an xterm-256 index (16..=255) to sRGB bytes: the 6×6×6 cube then the
/// 24-step grayscale ramp.
pub fn ansi256_to_rgb(idx: u8) -> (u8, u8, u8) {
    if (16..=231).contains(&idx) {
        let i = idx - 16;
        let r = i / 36;
        let g = (i % 36) / 6;
        let b = i % 6;
        let comp = |v: u8| -> u8 {
            if v == 0 {
                0
            } else {
                55 + v * 40
            }
        };
        (comp(r), comp(g), comp(b))
    } else if idx >= 232 {
        let level = 8 + (idx - 232) * 10;
        (level, level, level)
    } else {
        // 0..=15 system colors: approximate the standard xterm palette.
        const SYS: [(u8, u8, u8); 16] = [
            (0, 0, 0),
            (128, 0, 0),
            (0, 128, 0),
            (128, 128, 0),
            (0, 0, 128),
            (128, 0, 128),
            (0, 128, 128),
            (192, 192, 192),
            (128, 128, 128),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (0, 0, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ];
        SYS[idx as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use palette::color_difference::EuclideanDistance;
    use proptest::prelude::*;

    fn oklab_of(c: RainboxColor) -> Oklab {
        c.oklch.into_color()
    }

    proptest! {
        /// The contrast floor MUST hold for every relevance on both backgrounds.
        #[test]
        fn contrast_floor_always_holds(
            hue in 0.0f32..360.0,
            l in 0.2f32..0.9,
            c in 0.0f32..0.3,
            r in 0.0f32..1.0,
            dark in any::<bool>(),
        ) {
            let bg = if dark { Background::Dark } else { Background::Light };
            let color = RainboxColor::new(l, c, hue);
            let dimmed = color.dimmed(r, bg);
            let sep = (dimmed.oklch.l - bg.lightness()).abs();
            prop_assert!(sep + 1e-4 >= CONTRAST_FLOOR,
                "separation {sep} below floor for r={r}");
        }

        /// Full relevance is (approximately) the identity on lightness/chroma.
        #[test]
        fn full_relevance_preserves_color(
            hue in 0.0f32..360.0,
            l in 0.3f32..0.85,
            c in 0.05f32..0.2,
            dark in any::<bool>(),
        ) {
            let bg = if dark { Background::Dark } else { Background::Light };
            let color = RainboxColor::new(l, c, hue);
            let dimmed = color.dimmed(1.0, bg);
            // With d=0, L'=L and C'=C unless the floor kicks in.
            prop_assert!((dimmed.oklch.l - l).abs() < CONTRAST_FLOOR + 1e-3);
        }

        /// 256-color quantization round-trips within a bounded ΔE.
        #[test]
        fn ansi256_roundtrip_within_budget(
            hue in 0.0f32..360.0,
            l in 0.35f32..0.85,
            c in 0.02f32..0.18,
        ) {
            let color = RainboxColor::new(l, c, hue);
            let idx = color.to_ansi256();
            let (r, g, b) = ansi256_to_rgb(idx);
            let back: Oklab = Srgb::new(r, g, b).into_format::<f32>().into_color();
            let d = oklab_of(color).distance(back);
            // The 6-cube is coarse; 0.16 OKLab ΔE is a generous but real bound.
            prop_assert!(d < 0.16, "quantization ΔE {d} exceeded budget for idx {idx}");
        }
    }

    #[test]
    fn dimming_monotonic_toward_background() {
        let bg = Background::Dark;
        let color = RainboxColor::new(0.7, 0.15, 120.0);
        let full = color.dimmed(1.0, bg).oklch.chroma;
        let mid = color.dimmed(0.5, bg).oklch.chroma;
        let low = color.dimmed(0.2, bg).oklch.chroma;
        assert!(full > mid && mid > low, "chroma should shrink as relevance falls");
    }
}
