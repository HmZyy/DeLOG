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
    pub fn scrub(&mut self, t_us: i64, range: TimeRange) {
        self.t_us = t_us.clamp(range.min_us, range.max_us);
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
    }

    /// Keep the playhead inside a (possibly grown or shrunk) range without
    /// touching play state — called once per frame as data streams in.
    pub fn clamp_to(&mut self, range: TimeRange) {
        self.t_us = self.t_us.clamp(range.min_us, range.max_us);
    }
}

/// Where the pointer x lands on the scrubber, as canonical microseconds.
fn bar_time_at(x: f32, rect: egui::Rect, range: TimeRange) -> i64 {
    let frac = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0) as f64;
    range.min_us + (frac * (range.max_us - range.min_us) as f64).round() as i64
}

/// Playhead time → scrubber x.
fn bar_x_at(t_us: i64, rect: egui::Rect, range: TimeRange) -> f32 {
    let span = (range.max_us - range.min_us).max(1) as f64;
    let frac = ((t_us - range.min_us) as f64 / span).clamp(0.0, 1.0);
    rect.left() + rect.width() * frac as f32
}

/// Speed steps offered by the picker (within §11's 0.1–16× bounds).
const SPEED_STEPS: [f32; 8] = [0.1, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];

/// The timeline bar: transport buttons, speed picker and the scrubber
/// (§11, TLN-02). `any_live` shades the tail of the bar to mark a still
/// growing extent (live links, M7).
pub fn ui(ui: &mut egui::Ui, playback: &mut Playback, range: TimeRange, any_live: bool) {
    ui.horizontal(|ui| {
        let icon = if playback.playing { "⏸" } else { "▶" };
        if ui
            .button(icon)
            .on_hover_text("Play/pause (Space)")
            .clicked()
        {
            playback.toggle();
        }

        egui::ComboBox::from_id_salt("playback_speed")
            .selected_text(format!("{}×", playback.speed))
            .width(56.0)
            .show_ui(ui, |ui| {
                for step in SPEED_STEPS {
                    if ui
                        .selectable_label(playback.speed == step, format!("{step}×"))
                        .clicked()
                    {
                        playback.set_speed(step);
                    }
                }
            });

        scrubber(ui, playback, range, any_live);
    });
}

/// Full-range bar with a draggable playhead handle (TLN-02).
fn scrubber(ui: &mut egui::Ui, playback: &mut Playback, range: TimeRange, any_live: bool) {
    let height = 16.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );
    if !ui.is_rect_visible(rect) {
        return;
    }

    // Click or drag anywhere on the bar moves the playhead there.
    if (response.clicked() || response.dragged())
        && let Some(pos) = response.interact_pointer_pos()
    {
        playback.scrub(bar_time_at(pos.x, rect, range), range);
    }

    let painter = ui.painter();
    let visuals = ui.visuals();
    let bar = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 6.0));
    painter.rect_filled(bar, 3.0, visuals.extreme_bg_color);

    // Elapsed portion.
    let x = bar_x_at(playback.t_us, rect, range);
    let elapsed = egui::Rect::from_min_max(bar.min, egui::pos2(x, bar.max.y));
    painter.rect_filled(elapsed, 3.0, visuals.selection.bg_fill);

    // Live links keep growing the extent — tint the tail as a reminder that
    // the end of the bar is moving (§11; live arrives with M7).
    if any_live {
        let tail_w = (rect.width() * 0.03).clamp(4.0, 24.0);
        let tail = egui::Rect::from_min_max(egui::pos2(bar.right() - tail_w, bar.top()), bar.max);
        painter.rect_filled(tail, 3.0, visuals.warn_fg_color.gamma_multiply(0.5));
    }

    // Playhead handle.
    let stroke_color = if response.hovered() || response.dragged() {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    painter.vline(x, rect.y_range(), egui::Stroke::new(2.0, stroke_color));
    painter.circle_filled(egui::pos2(x, rect.center().y), 4.0, stroke_color);
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
    fn scrubber_mapping_round_trips_between_time_and_x() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 0.0), egui::vec2(200.0, 16.0));
        let range = range();

        assert_eq!(bar_time_at(100.0, rect, range), 1_000_000);
        assert_eq!(bar_time_at(300.0, rect, range), 5_000_000);
        assert_eq!(bar_time_at(200.0, rect, range), 3_000_000);
        // Outside the bar clamps to the ends.
        assert_eq!(bar_time_at(0.0, rect, range), 1_000_000);
        assert_eq!(bar_time_at(999.0, rect, range), 5_000_000);

        assert_eq!(bar_x_at(1_000_000, rect, range), 100.0);
        assert_eq!(bar_x_at(5_000_000, rect, range), 300.0);
        assert_eq!(bar_x_at(3_000_000, rect, range), 200.0);
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
