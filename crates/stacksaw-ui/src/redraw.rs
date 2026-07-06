//! Redraw rate limiting.
//!
//! A redraw is the costly step over a remote terminal (tmux/ssh): each frame is
//! a flush the far end must apply. Rapid state changes — most visibly a fast
//! mouse sweep that moves the hover highlight across many rows — would otherwise
//! redraw (and flush) once per intermediate step, which reads as lag even though
//! each individual draw is cheap.
//!
//! [`RedrawGate`] coalesces those changes in *time*: at most one redraw per
//! `min_interval`, so a sweep collapses to a handful of frames and the final
//! state still settles correctly. Time is supplied by the caller as monotonic
//! milliseconds, keeping it deterministic and unit-testable (the host derives it
//! from an [`std::time::Instant`]; the perf sweep feeds synthetic time).

/// Minimum gap between redraws, ~60 fps. Fast enough to feel immediate, slow
/// enough that a rapid sweep can't queue a flush per row over a remote link.
pub const REDRAW_MIN_INTERVAL_MS: u64 = 16;

/// Rate-limits redraws to at most one per `min_interval_ms`.
#[derive(Debug, Clone)]
pub struct RedrawGate {
    min_interval_ms: u64,
    last_ms: Option<u64>,
}

impl RedrawGate {
    /// A gate that permits a redraw at most every `min_interval_ms`. An interval
    /// of `0` never withholds (every request draws).
    pub fn new(min_interval_ms: u64) -> Self {
        Self {
            min_interval_ms,
            last_ms: None,
        }
    }

    /// Whether a redraw is permitted at `now_ms`. Returns `true` on the first
    /// call and once `min_interval_ms` has elapsed since the last permitted
    /// draw, recording `now_ms` as that draw. Call it only when you will draw if
    /// it returns `true`, so the recorded time tracks actual frames.
    pub fn ready(&mut self, now_ms: u64) -> bool {
        match self.last_ms {
            Some(last) if now_ms.saturating_sub(last) < self.min_interval_ms => false,
            _ => {
                self.last_ms = Some(now_ms);
                true
            }
        }
    }

    /// Milliseconds until the next redraw is permitted (`0` when ready now). Use
    /// it to size a poll timeout so a withheld redraw still lands promptly.
    pub fn wait_ms(&self, now_ms: u64) -> u64 {
        match self.last_ms {
            Some(last) => self
                .min_interval_ms
                .saturating_sub(now_ms.saturating_sub(last)),
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_draw_is_always_permitted() {
        let mut gate = RedrawGate::new(16);
        assert!(gate.ready(0));
    }

    #[test]
    fn withholds_within_the_interval_then_permits() {
        let mut gate = RedrawGate::new(16);
        assert!(gate.ready(100));
        assert!(!gate.ready(105), "5ms later is within the 16ms budget");
        assert!(!gate.ready(115), "15ms later still within budget");
        assert!(gate.ready(116), "16ms later crosses the budget");
    }

    #[test]
    fn withheld_calls_do_not_reset_the_clock() {
        let mut gate = RedrawGate::new(16);
        assert!(gate.ready(0));
        assert!(!gate.ready(10)); // withheld; must not push the deadline out
        assert!(gate.ready(16), "deadline is measured from the last real draw");
    }

    #[test]
    fn wait_ms_counts_down_to_the_next_frame() {
        let mut gate = RedrawGate::new(16);
        assert_eq!(gate.wait_ms(0), 0, "no draw yet: ready now");
        assert!(gate.ready(100));
        assert_eq!(gate.wait_ms(100), 16);
        assert_eq!(gate.wait_ms(110), 6);
        assert_eq!(gate.wait_ms(200), 0, "past the budget: ready now");
    }

    #[test]
    fn zero_interval_never_withholds() {
        let mut gate = RedrawGate::new(0);
        assert!(gate.ready(0));
        assert!(gate.ready(0));
        assert!(gate.ready(0));
    }
}
