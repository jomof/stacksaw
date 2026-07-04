//! Hue assignment: identity and relationship (§8.3).

use crate::GOLDEN_ANGLE_DEG;

/// Stable, well-separated hue for an *unrelated* branch, derived from a stable
/// hash of its name via the golden-angle sequence (§8.3). Stable across
/// sessions because it depends only on the name.
pub fn golden_angle_hue(branch_name: &str) -> f32 {
    let h = stable_hash(branch_name);
    // Multiply the golden angle by the hashed index for maximal separation.
    let idx = (h % 997) as f32; // small prime keeps the sequence lively
    (idx * GOLDEN_ANGLE_DEG).rem_euclid(360.0)
}

/// FNV-1a 64-bit — deterministic and dependency-free (unlike `DefaultHasher`,
/// whose output is not guaranteed stable across releases).
fn stable_hash(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The staircase arc configuration (§8.3): `h(i) = h₀ + span · i/(max(n−1,1))`.
#[derive(Debug, Clone, Copy)]
pub struct StaircaseArc {
    pub h0_deg: f32,
    pub span_deg: f32,
}

impl Default for StaircaseArc {
    fn default() -> Self {
        // blue → magenta → orange
        StaircaseArc {
            h0_deg: 250.0,
            span_deg: -190.0,
        }
    }
}

/// Hue for step `i` of `n` along the staircase arc so the staircase literally
/// reads as a rainbow ramp (§8.3).
pub fn staircase_arc_hue(arc: StaircaseArc, i: usize, n: usize) -> f32 {
    let denom = (n.max(2) - 1) as f32;
    let t = i as f32 / denom;
    (arc.h0_deg + arc.span_deg * t).rem_euclid(360.0)
}

/// Hue midpoint of two segments, for parent→child edge coloring (§8.3).
/// Uses the shorter arc around the hue circle.
pub fn hue_midpoint(a_deg: f32, b_deg: f32) -> f32 {
    let a = a_deg.rem_euclid(360.0);
    let b = b_deg.rem_euclid(360.0);
    let mut diff = b - a;
    if diff > 180.0 {
        diff -= 360.0;
    } else if diff < -180.0 {
        diff += 360.0;
    }
    (a + diff / 2.0).rem_euclid(360.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_hue_is_stable_and_in_range() {
        let a = golden_angle_hue("feat/wire-proto");
        let b = golden_angle_hue("feat/wire-proto");
        assert_eq!(a, b);
        assert!((0.0..360.0).contains(&a));
        assert_ne!(golden_angle_hue("feat/a"), golden_angle_hue("feat/b"));
    }

    #[test]
    fn arc_endpoints() {
        let arc = StaircaseArc::default();
        assert!((staircase_arc_hue(arc, 0, 4) - 250.0).abs() < 1e-3);
        // last step reaches h0 + span (mod 360): 250 - 190 = 60
        assert!((staircase_arc_hue(arc, 3, 4) - 60.0).abs() < 1e-3);
    }

    #[test]
    fn single_step_is_h0() {
        let arc = StaircaseArc::default();
        assert!((staircase_arc_hue(arc, 0, 1) - 250.0).abs() < 1e-3);
    }

    #[test]
    fn midpoint_wraps_shortest_arc() {
        // Between 350 and 10 the midpoint is 0, not 180.
        assert!((hue_midpoint(350.0, 10.0) - 0.0).abs() < 1e-3);
        assert!((hue_midpoint(10.0, 350.0) - 0.0).abs() < 1e-3);
    }
}
