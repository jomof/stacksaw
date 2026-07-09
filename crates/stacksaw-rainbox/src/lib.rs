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
pub use relevance::{temporal_decay, Relevance, RelevanceSignals, State, Topological};

/// The golden angle in degrees, used to space unrelated hues maximally.
pub const GOLDEN_ANGLE_DEG: f32 = 137.507_76;

/// Minimum lightness separation from the background (§8.3 contrast floor).
pub const CONTRAST_FLOOR: f32 = 0.18;

/// How relevance fades a color toward the background (§8.3). These are style
/// parameters the theme owns (`[rainbow.dim]` + `[rainbow].contrast_floor`); the
/// crate carries only the canonical defaults so callers without a theme (and the
/// [`dimmed`](RainboxColor::dimmed) convenience) still have a sane curve.
#[derive(Debug, Clone, Copy)]
pub struct DimCurve {
    /// Fraction of the way `L` is pulled toward the background at full dim.
    pub lightness_toward_bg: f32,
    /// Fraction of chroma removed at full dim.
    pub chroma: f32,
    /// Minimum `|L − L_bg|` kept so a dimmed color never sinks into the ground.
    pub contrast_floor: f32,
}

impl Default for DimCurve {
    fn default() -> Self {
        DimCurve {
            lightness_toward_bg: 0.75,
            chroma: 0.85,
            contrast_floor: CONTRAST_FLOOR,
        }
    }
}

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
    /// Lightness is held constant across all hues (brightness is reserved to
    /// carry relevance/state, not identity); chroma is pushed as high as the
    /// gamut allows via [`to_rgb`]'s mapping, so hues stay maximally distinct.
    pub fn from_hue(hue_deg: f32) -> Self {
        RainboxColor {
            oklch: Oklch::new(0.72, 0.18, hue_deg),
        }
    }

    pub fn new(l: f32, c: f32, hue_deg: f32) -> Self {
        RainboxColor {
            oklch: Oklch::new(l, c, hue_deg),
        }
    }

    /// Construct from an 8-bit sRGB color, so a fixed palette color can be run
    /// through the same relevance dimming as a generated hue (§8.3). A gray
    /// (chroma ≈ 0) simply darkens toward the background.
    pub fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        let oklch: Oklch = Srgb::new(r, g, b).into_format::<f32>().into_color();
        RainboxColor { oklch }
    }

    /// Apply relevance dimming toward the background using the default
    /// [`DimCurve`] (§8.3). See [`dimmed_with`](Self::dimmed_with).
    pub fn dimmed(self, relevance: f32, bg: Background) -> RainboxColor {
        self.dimmed_with(relevance, bg, DimCurve::default())
    }

    /// Apply relevance dimming toward the background with an explicit `curve`
    /// (§8.3). At dim factor `d = 1 − r`:
    /// `L' = lerp(L, L_bg, lightness_toward_bg·d)`, `C' = C·(1 − chroma·d)`,
    /// then push `L'` back to `contrast_floor` from `L_bg` if it drifted too
    /// close. This is where the theme's `[rainbow.dim]` values take effect.
    pub fn dimmed_with(self, relevance: f32, bg: Background, curve: DimCurve) -> RainboxColor {
        let r = relevance.clamp(0.0, 1.0);
        let d = 1.0 - r;
        let l_bg = bg.lightness();
        let l = self.oklch.l;
        let c = self.oklch.chroma;

        let mut l_prime = lerp(l, l_bg, curve.lightness_toward_bg * d);
        let c_prime = c * (1.0 - curve.chroma * d);

        // Contrast floor: push L' away from the background if it got too close.
        if (l_prime - l_bg).abs() < curve.contrast_floor {
            let dir = if l >= l_bg { 1.0 } else { -1.0 };
            l_prime = l_bg + dir * curve.contrast_floor;
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
    ///
    /// Uses proper gamut mapping: rather than clipping channels (which distorts
    /// hue and lightness — light hues clip toward white and collapse together),
    /// we hold **L and hue fixed** and reduce chroma until the color fits the
    /// sRGB gamut. This keeps perceived brightness constant across all hues
    /// (brightness stays free to carry other meaning) and keeps hues distinct.
    pub fn to_rgb(self) -> (u8, u8, u8) {
        let srgb = gamut_map(self.oklch);
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
            let candidate: Oklab = Srgb::new(r, g, b).into_format::<f32>().into_color();
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

/// True when `oklch` maps to an in-gamut sRGB color (all channels within
/// `[0, 1]`, allowing a tiny epsilon for float error).
fn oklch_in_gamut(oklch: Oklch) -> bool {
    let rgb: Srgb = oklch.into_color();
    let eps = 1e-3;
    let ok = |v: f32| v >= -eps && v <= 1.0 + eps;
    ok(rgb.red) && ok(rgb.green) && ok(rgb.blue)
}

/// Gamut-map an OKLCH color into sRGB by holding lightness and hue fixed and
/// bisecting chroma down to the largest in-gamut value (CSS Color 4 style,
/// simplified). Any residual sub-epsilon overshoot is clamped.
fn gamut_map(oklch: Oklch) -> Srgb {
    if oklch_in_gamut(oklch) {
        return clamp_srgb(oklch.into_color());
    }
    let (l, hue) = (oklch.l, oklch.hue);
    let mut lo = 0.0f32; // in gamut (chroma 0 is a gray)
    let mut hi = oklch.chroma; // out of gamut
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if oklch_in_gamut(Oklch::new(l, mid, hue)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    clamp_srgb(Oklch::new(l, lo, hue).into_color())
}

fn clamp_srgb(srgb: Srgb) -> Srgb {
    Srgb::new(
        srgb.red.clamp(0.0, 1.0),
        srgb.green.clamp(0.0, 1.0),
        srgb.blue.clamp(0.0, 1.0),
    )
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

    proptest! {
        /// Gamut mapping preserves lightness and hue while producing in-gamut
        /// sRGB: brightness stays constant across hues, so it is free to carry
        /// other meaning.
        #[test]
        fn gamut_map_preserves_lightness_and_hue(hue in 0.0f32..360.0) {
            let oklch = RainboxColor::from_hue(hue).oklch;
            let mapped = gamut_map(oklch);
            // Round-trips back to (approximately) the same OKLCH lightness:
            // the only drift is a sub-perceptual clamp at the gamut boundary.
            let back: Oklch = mapped.into_color();
            prop_assert!((back.l - oklch.l).abs() < 0.05,
                "lightness drifted for hue {hue}: {} vs {}", back.l, oklch.l);
            // And it is genuinely in gamut (0..=1 per channel).
            prop_assert!((0.0..=1.0).contains(&mapped.red));
            prop_assert!((0.0..=1.0).contains(&mapped.green));
            prop_assert!((0.0..=1.0).contains(&mapped.blue));
        }
    }

    #[test]
    fn constant_lightness_across_hues() {
        // Every identity hue renders at the same perceptual lightness.
        let ls: Vec<f32> = (0..12)
            .map(|k| {
                let hue = k as f32 * 30.0;
                let (r, g, b) = RainboxColor::from_hue(hue).to_rgb();
                let ok: Oklab = Srgb::new(r, g, b).into_format::<f32>().into_color();
                ok.l
            })
            .collect();
        let min = ls.iter().cloned().fold(f32::INFINITY, f32::min);
        let max = ls.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max - min < 0.06,
            "lightness varied {min}..{max} across hues"
        );
    }

    #[test]
    fn dimming_monotonic_toward_background() {
        let bg = Background::Dark;
        let color = RainboxColor::new(0.7, 0.15, 120.0);
        let full = color.dimmed(1.0, bg).oklch.chroma;
        let mid = color.dimmed(0.5, bg).oklch.chroma;
        let low = color.dimmed(0.2, bg).oklch.chroma;
        assert!(
            full > mid && mid > low,
            "chroma should shrink as relevance falls"
        );
    }
}
