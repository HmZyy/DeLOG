//! Global timeline: playback model, scrubber and transport (PLAN.md §11).
//!
//! The playhead is the single time authority for plots and the 3D view. The
//! model is pure state + time math (no egui) so it is unit-testable; the
//! widgets live alongside it and only translate input into model calls.

use delog_core::time::TimeRange;

/// Playback speed bounds (§11).
pub const MIN_SPEED: f32 = 0.1;
pub const MAX_SPEED: f32 = 16.0;

/// Play/pause state, playhead position and speed (§11, TLN-01).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    pub playing: bool,
    /// Playhead in canonical effective microseconds.
    pub t_us: i64,
    pub speed: f32,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            playing: false,
            t_us: 0,
            speed: 1.0,
        }
    }
}

impl Playback {
    // The transport/scrubber UI (TLN-02) is the caller of toggle/scrub/
    // set_speed; the expectations error out once it lands.
    #[expect(dead_code)]
    pub fn toggle(&mut self) {
        self.playing = !self.playing;
    }

    /// Advance the playhead by wall-clock `dt` × speed, clamped to `range`.
    /// Reaching the end pauses (no wrap, no overshoot).
    pub fn advance(&mut self, dt_secs: f64, range: TimeRange) {
        debug_assert!(
            (MIN_SPEED..=MAX_SPEED).contains(&self.speed),
            "speed must be set via set_speed"
        );
        if !self.playing {
            return;
        }
        let step = (dt_secs * self.speed as f64 * 1e6) as i64;
        self.t_us = (self.t_us.saturating_add(step)).clamp(range.min_us, range.max_us);
        if self.t_us >= range.max_us {
            self.playing = false;
        }
    }

    /// Move the playhead directly (scrubber drag, jumps), clamped to `range`.
    #[expect(dead_code)]
    pub fn scrub(&mut self, t_us: i64, range: TimeRange) {
        self.t_us = t_us.clamp(range.min_us, range.max_us);
    }

    #[expect(dead_code)]
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
    }

    /// Keep the playhead inside a (possibly grown or shrunk) range without
    /// touching play state — called once per frame as data streams in.
    pub fn clamp_to(&mut self, range: TimeRange) {
        self.t_us = self.t_us.clamp(range.min_us, range.max_us);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range() -> TimeRange {
        TimeRange::new(1_000_000, 5_000_000).unwrap()
    }

    #[test]
    fn advance_moves_by_wall_clock_times_speed() {
        let mut p = Playback {
            playing: true,
            t_us: 1_000_000,
            speed: 2.0,
        };
        p.advance(0.5, range()); // 0.5 s × 2× = 1 s
        assert_eq!(p.t_us, 2_000_000);
        assert!(p.playing);
    }

    #[test]
    fn advance_does_nothing_while_paused() {
        let mut p = Playback {
            playing: false,
            t_us: 1_500_000,
            speed: 1.0,
        };
        p.advance(1.0, range());
        assert_eq!(p.t_us, 1_500_000);
    }

    #[test]
    fn reaching_the_end_clamps_and_pauses() {
        let mut p = Playback {
            playing: true,
            t_us: 4_900_000,
            speed: 16.0,
        };
        p.advance(1.0, range());
        assert_eq!(p.t_us, 5_000_000);
        assert!(!p.playing);
    }

    #[test]
    fn scrub_clamps_to_the_range() {
        let mut p = Playback::default();
        p.scrub(0, range());
        assert_eq!(p.t_us, 1_000_000);
        p.scrub(9_000_000, range());
        assert_eq!(p.t_us, 5_000_000);
        p.scrub(3_000_000, range());
        assert_eq!(p.t_us, 3_000_000);
    }

    #[test]
    fn speed_is_clamped_to_spec_bounds() {
        let mut p = Playback::default();
        p.set_speed(0.01);
        assert_eq!(p.speed, MIN_SPEED);
        p.set_speed(100.0);
        assert_eq!(p.speed, MAX_SPEED);
        p.set_speed(4.0);
        assert_eq!(p.speed, 4.0);
    }

    #[test]
    fn clamp_to_keeps_playhead_inside_a_shrunk_range() {
        let mut p = Playback {
            playing: true,
            t_us: 9_000_000,
            speed: 1.0,
        };
        p.clamp_to(range());
        assert_eq!(p.t_us, 5_000_000);
        assert!(p.playing, "clamp_to never touches play state");
    }
}
