//! Relevance signals and their combination into a dim factor (§8.3).
//!
//! Each element carries relevance `r ∈ [0,1]` = the max of weighted signals.

use std::f32::consts::LN_2;

/// Topological proximity to the focused element (§8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topological {
    Focused,
    SameSegment,
    AdjacentSegment,
    Unrelated,
}

impl Topological {
    pub fn value(self) -> f32 {
        match self {
            Topological::Focused => 1.0,
            Topological::SameSegment => 0.75,
            Topological::AdjacentSegment => 0.55,
            Topological::Unrelated => 0.30,
        }
    }
}

/// State-based clamp (§8.3): merged/landed and upstream context are pushed low.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Normal,
    /// Merged/landed branches clamp to 0.15.
    Landed,
    /// Upstream context commits clamp to 0.20.
    UpstreamContext,
}

impl State {
    fn clamp(self) -> Option<f32> {
        match self {
            State::Normal => None,
            State::Landed => Some(0.15),
            State::UpstreamContext => Some(0.20),
        }
    }
}

/// All the inputs that determine an element's relevance.
#[derive(Debug, Clone, Copy)]
pub struct RelevanceSignals {
    /// Age of the element in days (branches use last-commit age).
    pub age_days: f32,
    /// Half-life for temporal decay, in days (default 14).
    pub half_life_days: f32,
    pub topological: Topological,
    /// Element has open findings or an active agent (floor 0.85).
    pub attention: bool,
    pub state: State,
    /// Element matches an active palette/filter search (forces 1.0).
    pub search_match: bool,
}

impl Default for RelevanceSignals {
    fn default() -> Self {
        RelevanceSignals {
            age_days: 0.0,
            half_life_days: 14.0,
            topological: Topological::Unrelated,
            attention: false,
            state: State::Normal,
            search_match: false,
        }
    }
}

/// A computed relevance value in `[0,1]`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Relevance(pub f32);

impl Relevance {
    /// Combine signals per §8.3: temporal decay, topological weight, attention
    /// floor, state clamp, and search override.
    pub fn compute(s: RelevanceSignals) -> Relevance {
        // Search override wins outright.
        if s.search_match {
            return Relevance(1.0);
        }

        let temporal = temporal_decay(s.age_days, s.half_life_days);
        let mut r = temporal.max(s.topological.value());

        if s.attention {
            r = r.max(0.85);
        }

        // State clamps are ceilings (landed/upstream never blaze bright)…
        if let Some(ceiling) = s.state.clamp() {
            r = r.min(ceiling);
        }

        Relevance(r.clamp(0.0, 1.0))
    }

    pub fn dim_factor(self) -> f32 {
        1.0 - self.0
    }
}

/// `exp(−ln2 · age/half_life)` (§8.3).
pub fn temporal_decay(age_days: f32, half_life_days: f32) -> f32 {
    if half_life_days <= 0.0 {
        return 1.0;
    }
    (-LN_2 * age_days / half_life_days).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporal_halves_at_half_life() {
        assert!((temporal_decay(14.0, 14.0) - 0.5).abs() < 1e-4);
        assert!((temporal_decay(0.0, 14.0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn search_forces_full_relevance_over_landed_clamp() {
        let s = RelevanceSignals {
            state: State::Landed,
            search_match: true,
            ..Default::default()
        };
        assert_eq!(Relevance::compute(s).0, 1.0);
    }

    #[test]
    fn landed_clamps_even_when_focused() {
        let s = RelevanceSignals {
            topological: Topological::Focused,
            state: State::Landed,
            ..Default::default()
        };
        assert!((Relevance::compute(s).0 - 0.15).abs() < 1e-4);
    }

    #[test]
    fn attention_floor_applies() {
        let s = RelevanceSignals {
            age_days: 1000.0, // fully decayed
            topological: Topological::Unrelated,
            attention: true,
            ..Default::default()
        };
        assert!(Relevance::compute(s).0 >= 0.85);
    }
}
