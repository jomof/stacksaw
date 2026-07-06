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

/// How long pointer motion must pause before a hover change is painted. Any-
/// motion mouse tracking (DEC 1003) emits an event per cell, so a fast drag
/// floods input; waiting for a brief settle collapses the whole flurry into a
/// single frame at the final row instead of animating through every row it
/// crossed. ~2 frames — imperceptible when you land on a row.
pub const HOVER_SETTLE_MS: u64 = 30;

/// Upper bound on how long a hover change may be withheld while the pointer
/// keeps moving, so a long continuous drag still tracks the cursor in coarse
/// steps (~12 fps) rather than freezing until motion stops.
pub const HOVER_MAX_WAIT_MS: u64 = 80;

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

/// Debounces hover redraws. A hover change (the pointer moving to a different
/// row/divider) is not painted immediately: it waits until motion settles for
/// `settle_ms`, or `max_wait_ms` has elapsed since the last hover paint —
/// whichever comes first. This drops the stale intermediate positions a fast
/// drag would otherwise render, so the highlight jumps to where the pointer
/// actually is instead of trailing behind it.
#[derive(Debug, Clone)]
pub struct HoverThrottle {
    settle_ms: u64,
    max_wait_ms: u64,
    dirty: bool,
    last_change_ms: u64,
    last_draw_ms: u64,
}

impl HoverThrottle {
    pub fn new(settle_ms: u64, max_wait_ms: u64) -> Self {
        Self {
            settle_ms,
            max_wait_ms,
            dirty: false,
            last_change_ms: 0,
            last_draw_ms: 0,
        }
    }

    /// Record a hover change at `now_ms`. Restarts the settle timer, so ongoing
    /// motion keeps deferring the paint until it pauses (or `max_wait` fires).
    pub fn touched(&mut self, now_ms: u64) {
        self.dirty = true;
        self.last_change_ms = now_ms;
    }

    /// Whether a pending hover change should be painted at `now_ms`: motion has
    /// settled, or it has been withheld for `max_wait_ms`. Non-mutating.
    pub fn due(&self, now_ms: u64) -> bool {
        self.dirty
            && (now_ms.saturating_sub(self.last_change_ms) >= self.settle_ms
                || now_ms.saturating_sub(self.last_draw_ms) >= self.max_wait_ms)
    }

    /// Record that a redraw painted the current hover state at `now_ms`.
    pub fn drawn(&mut self, now_ms: u64) {
        self.dirty = false;
        self.last_draw_ms = now_ms;
    }

    /// Milliseconds until [`due`](Self::due) next becomes true, or `None` when
    /// no hover change is pending. Used to size the event-loop poll timeout.
    pub fn next_due_in(&self, now_ms: u64) -> Option<u64> {
        if !self.dirty {
            return None;
        }
        let settle = self.settle_ms.saturating_sub(now_ms.saturating_sub(self.last_change_ms));
        let max_wait = self
            .max_wait_ms
            .saturating_sub(now_ms.saturating_sub(self.last_draw_ms));
        Some(settle.min(max_wait))
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

    #[test]
    fn hover_waits_for_motion_to_settle() {
        let mut hover = HoverThrottle::new(30, 80);
        hover.drawn(1000); // a recent paint, so max_wait isn't already overdue
        hover.touched(1010);
        assert!(!hover.due(1020), "still moving: withhold");
        assert!(!hover.due(1039), "just under the settle window");
        assert!(hover.due(1040), "30ms of stillness: paint");
    }

    #[test]
    fn first_hover_after_idle_paints_immediately() {
        // With no prior paint, max_wait is already exceeded, so the very first
        // hover lands at once — a hover onto a row after idle feels instant.
        let mut hover = HoverThrottle::new(30, 80);
        hover.touched(5000);
        assert!(hover.due(5000));
    }

    #[test]
    fn continuous_motion_still_paints_at_max_wait() {
        let mut hover = HoverThrottle::new(30, 80);
        hover.drawn(1000); // a hover frame just landed
        // The pointer keeps moving every 10ms, so settle never triggers...
        for t in (1010..=1070).step_by(10) {
            hover.touched(t);
            assert!(!hover.due(t), "settle keeps resetting under continuous motion");
        }
        // ...but max_wait (80ms since the last paint) forces a coarse update.
        hover.touched(1080);
        assert!(hover.due(1080));
    }

    #[test]
    fn nothing_pending_reports_no_wait() {
        let hover = HoverThrottle::new(30, 80);
        assert_eq!(hover.next_due_in(1000), None);
    }

    #[test]
    fn next_due_in_is_the_sooner_of_settle_and_max_wait() {
        let mut hover = HoverThrottle::new(30, 80);
        hover.drawn(1000);
        hover.touched(1005);
        // settle fires at 1035 (30ms), max_wait at 1080 (80ms) -> settle wins.
        assert_eq!(hover.next_due_in(1005), Some(30));
        // Later, a fresh move at 1070 pushes settle to 1100, past max_wait 1080.
        hover.touched(1070);
        assert_eq!(hover.next_due_in(1070), Some(10), "max_wait now the limiter");
    }
}
