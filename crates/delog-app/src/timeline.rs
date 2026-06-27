//! Global timeline: playback model, scrubber and transport.
//!
//! The model is pure state + time math (no egui) so it is unit-testable; the
//! widgets only translate input into model calls.

use delog_core::time::TimeRange;

use crate::plot::ViewX;

pub const MIN_SPEED: f32 = 0.1;
pub const MAX_SPEED: f32 = 100.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    pub playing: bool,
    /// Canonical effective microseconds.
    pub t_us: i64,
    pub speed: f32,
    /// Live-stream tail-follow: each frame pins the playhead to the range end
    /// until a manual scrub clears it.
    pub follow_live: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            playing: false,
            t_us: 0,
            speed: 1.0,
            follow_live: false,
        }
    }
}

impl Playback {
    pub fn toggle(&mut self) {
        self.playing = !self.playing;
    }

    /// Advance the playhead by wall-clock `dt` × speed, clamped to `range`;
    /// reaching the end pauses.
    pub fn advance(&mut self, dt_secs: f64, range: TimeRange) {
        debug_assert!(
            (MIN_SPEED..=MAX_SPEED).contains(&self.speed),
            "speed must be set via set_speed"
        );
        if self.follow_live {
            self.t_us = range.max_us;
            return;
        }
        if !self.playing {
            return;
        }
        let step = (dt_secs * self.speed as f64 * 1e6) as i64;
        self.t_us = (self.t_us.saturating_add(step)).clamp(range.min_us, range.max_us);
        if self.t_us >= range.max_us {
            self.playing = false;
        }
    }

    pub fn scrub(&mut self, t_us: i64, range: TimeRange) {
        self.follow_live = false;
        self.t_us = t_us.clamp(range.min_us, range.max_us);
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
    }

    /// Keep the playhead inside a (grown or shrunk) range without touching play
    /// state; called once per frame as data streams in.
    pub fn clamp_to(&mut self, range: TimeRange) {
        if self.follow_live {
            self.t_us = range.max_us;
        } else {
            self.t_us = self.t_us.clamp(range.min_us, range.max_us);
        }
    }

    pub fn lock_to_live(&mut self, range: TimeRange) {
        self.follow_live = true;
        self.t_us = range.max_us;
    }

    pub fn unlock_live(&mut self) {
        self.follow_live = false;
    }

    pub fn jump_start(&mut self, range: TimeRange) {
        self.scrub(range.min_us, range);
    }

    pub fn jump_end(&mut self, range: TimeRange) {
        self.scrub(range.max_us, range);
    }
}

const FALLBACK_STEP_US: i64 = 1_000_000 / 30;

/// Where `←`/`→` land: the adjacent sample of the focused plot's reference
/// trace, or ±1/30 s without a focus. Stays put at the trace ends.
pub fn step_target(
    snapshot: &delog_core::snapshot::StoreSnapshot,
    reference: Option<delog_core::identity::FieldId>,
    t_us: i64,
    forward: bool,
) -> i64 {
    use delog_core::field_view::{FieldView, SampleMode};

    let Some(field) = reference else {
        let step = if forward {
            FALLBACK_STEP_US
        } else {
            -FALLBACK_STEP_US
        };
        return t_us.saturating_add(step);
    };
    let Ok(view) = FieldView::new(snapshot, field) else {
        return t_us;
    };
    // Probe one µs past the playhead so landing exactly on a sample still
    // steps to the adjacent one.
    let (probe, mode) = if forward {
        (t_us.saturating_add(1), SampleMode::Next)
    } else {
        (t_us.saturating_sub(1), SampleMode::Prev)
    };
    view.sample_at(probe, mode)
        .map_or(t_us, |sample| sample.effective_time_us)
}

fn bar_time_at(x: f32, rect: egui::Rect, range: TimeRange) -> i64 {
    let frac = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0) as f64;
    range.min_us + (frac * (range.max_us - range.min_us) as f64).round() as i64
}

fn bar_x_at(t_us: i64, rect: egui::Rect, range: TimeRange) -> f32 {
    let span = (range.max_us - range.min_us).max(1) as f64;
    let frac = ((t_us - range.min_us) as f64 / span).clamp(0.0, 1.0);
    rect.left() + rect.width() * frac as f32
}

/// `t_us` relative to `origin_us` as `M:SS.mmm` (hours only when nonzero),
/// clamped at zero.
pub fn format_relative(t_us: i64, origin_us: i64) -> String {
    let rel_ms = (t_us - origin_us).max(0) / 1_000;
    let ms = rel_ms % 1_000;
    let s = (rel_ms / 1_000) % 60;
    let m = (rel_ms / 60_000) % 60;
    let h = rel_ms / 3_600_000;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

/// Unix microseconds → `YYYY-MM-DD HH:MM:SS.mmm UTC`. Hand-rolled so no date
/// crate enters the pinned dependency set.
pub fn format_utc(unix_us: i64) -> String {
    let ms_total = unix_us.div_euclid(1_000);
    let ms = ms_total.rem_euclid(1_000);
    let secs = ms_total.div_euclid(1_000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    format!(
        "{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02}.{ms:03} UTC",
        sod / 3_600,
        (sod / 60) % 60,
        sod % 60
    )
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's
/// `civil_from_days`; exact for the proleptic Gregorian calendar.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimelineAction {
    pub lock_live: bool,
    pub manual_scrub: bool,
    /// Window range slider moved; treated like a manual pan/zoom (disengages
    /// fit-all/live).
    pub view_changed: bool,
    pub marker_jump: Option<i64>,
    pub marker_move: Option<(u64, i64)>,
    pub marker_delete: Option<u64>,
    pub marker_edit: Option<(u64, MarkerEdit)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarkerEdit {
    pub label: Option<String>,
    pub color: Option<[f32; 4]>,
}

/// `utc_offset_us` maps canonical time to unix time when a source carries a UTC
/// reference; `any_live` shades the bar tail to mark a still-growing extent.
#[allow(clippy::too_many_arguments)]
pub fn ui(
    ui: &mut egui::Ui,
    playback: &mut Playback,
    fit_all: &mut bool,
    view: &mut Option<ViewX>,
    range: TimeRange,
    utc_offset_us: Option<i64>,
    any_live: bool,
    theme: crate::theme::ThemeChoice,
    markers: &crate::markers::Markers,
) -> TimelineAction {
    let mut action = TimelineAction::default();
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let button_size = egui::vec2(40.0, 40.0);
        // Status dot: grey = not streaming, yellow = streaming unlocked, red = locked.
        let (dot_color, dot_tip) = if !any_live {
            (theme.neutral(), "Not streaming")
        } else if playback.follow_live {
            (theme.error(), "Locked to live tail — click to unlock")
        } else {
            (
                theme.warning(),
                "Live (not locked) — click to lock to the tail",
            )
        };
        let sense = if any_live {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let (dot_rect, mut dot_resp) = ui.allocate_exact_size(button_size, sense);
        let draw_color = if any_live && dot_resp.hovered() {
            dot_color.gamma_multiply(1.3)
        } else {
            dot_color
        };
        ui.painter()
            .circle_filled(dot_rect.center(), 5.0, draw_color);
        if any_live {
            dot_resp = dot_resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            if dot_resp.clicked() {
                if playback.follow_live {
                    playback.unlock_live();
                } else {
                    playback.lock_to_live(range);
                    action.lock_live = true;
                }
            }
        }
        dot_resp.on_hover_text(dot_tip);

        let transport_icon = |src: egui::ImageSource<'static>, ui: &egui::Ui| {
            egui::Image::new(src)
                .fit_to_exact_size(egui::vec2(16.0, 16.0))
                .tint(ui.visuals().text_color())
        };
        if ui
            .add_sized(
                button_size,
                egui::Button::image(transport_icon(crate::icons::skip_back(), ui)),
            )
            .on_hover_text("Jump to start (Home)")
            .clicked()
        {
            playback.jump_start(range);
            action.manual_scrub = true;
        }
        let icon = if playback.playing {
            crate::icons::pause()
        } else {
            crate::icons::play()
        };
        if ui
            .add_sized(button_size, egui::Button::image(transport_icon(icon, ui)))
            .on_hover_text("Play/pause (Space)")
            .clicked()
        {
            playback.toggle();
        }
        let end_tip = if any_live {
            "Lock to live tail (End)"
        } else {
            "Jump to end (End)"
        };
        if ui
            .add_sized(
                button_size,
                egui::Button::image(transport_icon(crate::icons::skip_forward(), ui)),
            )
            .on_hover_text(end_tip)
            .clicked()
        {
            if any_live {
                playback.lock_to_live(range);
                action.lock_live = true;
            } else {
                playback.jump_end(range);
                action.manual_scrub = true;
            }
        }

        let fit_icon = egui::Image::new(crate::icons::maximize())
            .fit_to_exact_size(egui::vec2(16.0, 16.0))
            .tint(ui.visuals().text_color());
        if ui
            .add_sized(
                button_size,
                egui::Button::image(fit_icon).selected(*fit_all),
            )
            .on_hover_text("Fit to view")
            .clicked()
        {
            *fit_all = !*fit_all;
        }

        let mut speed = playback.speed;
        if ui
            .add(
                egui::DragValue::new(&mut speed)
                    .range(MIN_SPEED as f64..=MAX_SPEED as f64)
                    .speed(0.05)
                    .max_decimals(2)
                    .suffix("×"),
            )
            .on_hover_text("Playback speed (0.1–100×)")
            .changed()
        {
            playback.set_speed(speed);
        }

        ui.monospace(format!(
            "{} / {}",
            format_relative(playback.t_us, range.min_us),
            format_relative(range.max_us, range.min_us)
        ));
        if let Some(offset) = utc_offset_us {
            ui.weak(format_utc(playback.t_us + offset));
        }

        ui.vertical(|ui| {
            if scrubber(ui, playback, range, any_live, markers, &mut action) {
                action.manual_scrub = true;
            }
            ui.add_space(6.0);
            if let Some(v) = view.as_mut()
                && window_slider(ui, v, range)
            {
                action.view_changed = true;
            }
        });
    });
    ui.add_space(6.0);
    action
}

/// Full-range bar with draggable playhead and marker flags. A drag beginning on
/// a flag moves it, a click jumps to it, right-click edits/deletes; drags/clicks
/// elsewhere scrub. Marker interactions are reported via `action`.
fn scrubber(
    ui: &mut egui::Ui,
    playback: &mut Playback,
    range: TimeRange,
    any_live: bool,
    markers: &crate::markers::Markers,
    action: &mut TimelineAction,
) -> bool {
    let height = 16.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );
    if !ui.is_rect_visible(rect) {
        return false;
    }

    let hit_radius = 6.0;
    let nearest_flag = |x: f32| -> Option<u64> {
        markers
            .by_time()
            .iter()
            .map(|m| (m.id, (bar_x_at(m.t_us, rect, range) - x).abs()))
            .filter(|(_, d)| *d <= hit_radius)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    };

    // Marker being dragged, persisted across frames via egui memory.
    let drag_key = ui.id().with("marker_drag");
    let mut dragging: Option<u64> = ui
        .memory_mut(|m| m.data.get_temp::<Option<u64>>(drag_key))
        .flatten();
    if response.drag_started() {
        dragging = response
            .interact_pointer_pos()
            .and_then(|p| nearest_flag(p.x));
        ui.memory_mut(|m| m.data.insert_temp(drag_key, dragging));
    }

    let mut scrubbed = false;
    if let Some(id) = dragging {
        // Move the grabbed marker; the playhead stays put.
        if let Some(p) = response.interact_pointer_pos() {
            action.marker_move = Some((id, bar_time_at(p.x, rect, range)));
        }
        if response.drag_stopped() {
            ui.memory_mut(|m| m.data.insert_temp::<Option<u64>>(drag_key, None));
        }
    } else if response.clicked() {
        if let Some(p) = response.interact_pointer_pos() {
            match nearest_flag(p.x) {
                Some(id) => {
                    if let Some(m) = markers.by_time().into_iter().find(|m| m.id == id) {
                        action.marker_jump = Some(m.t_us);
                    }
                }
                None => {
                    playback.scrub(bar_time_at(p.x, rect, range), range);
                    scrubbed = true;
                }
            }
        }
    } else if response.dragged()
        && let Some(p) = response.interact_pointer_pos()
    {
        playback.scrub(bar_time_at(p.x, rect, range), range);
        scrubbed = true;
    }

    let visuals = ui.visuals().clone();
    let painter = ui.painter();
    let bar = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 6.0));
    painter.rect_filled(bar, 3.0, visuals.extreme_bg_color);

    let x = bar_x_at(playback.t_us, rect, range);
    let elapsed = egui::Rect::from_min_max(bar.min, egui::pos2(x, bar.max.y));
    painter.rect_filled(elapsed, 3.0, visuals.selection.bg_fill);

    // Tint the tail to mark a still-growing live extent.
    if any_live {
        let tail_w = (rect.width() * 0.03).clamp(4.0, 24.0);
        let tail = egui::Rect::from_min_max(egui::pos2(bar.right() - tail_w, bar.top()), bar.max);
        painter.rect_filled(tail, 3.0, visuals.warn_fg_color.gamma_multiply(0.5));
    }

    let stroke_color = if response.hovered() || response.dragged() {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    painter.vline(x, rect.y_range(), egui::Stroke::new(2.0, stroke_color));
    painter.circle_filled(egui::pos2(x, rect.center().y), 4.0, stroke_color);

    for m in markers.by_time() {
        let fx = bar_x_at(m.t_us, rect, range);
        let color = m.color32();
        painter.vline(fx, rect.y_range(), egui::Stroke::new(1.0, color));
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(fx - 4.0, rect.top()),
                egui::pos2(fx + 4.0, rect.top()),
                egui::pos2(fx, rect.top() + 6.0),
            ],
            color,
            egui::Stroke::NONE,
        ));
    }

    for m in markers.by_time() {
        let fx = bar_x_at(m.t_us, rect, range);
        let flag_rect = egui::Rect::from_min_max(
            egui::pos2(fx - hit_radius, rect.top()),
            egui::pos2(fx + hit_radius, rect.bottom()),
        );
        let tip = if m.note.is_empty() {
            m.label.clone()
        } else {
            format!("{}\n{}", m.label, m.note)
        };
        let fr = ui
            .interact(
                flag_rect,
                ui.id().with(("flag", m.id)),
                egui::Sense::click(),
            )
            .on_hover_text(tip);
        fr.context_menu(|ui| {
            let mut label = m.label.clone();
            if ui
                .add(egui::TextEdit::singleline(&mut label).hint_text("label"))
                .changed()
            {
                action.marker_edit = Some((
                    m.id,
                    MarkerEdit {
                        label: Some(label),
                        color: None,
                    },
                ));
            }
            let mut color = m.color32();
            if egui::color_picker::color_edit_button_srgba(
                ui,
                &mut color,
                egui::color_picker::Alpha::Opaque,
            )
            .changed()
            {
                action.marker_edit = Some((
                    m.id,
                    MarkerEdit {
                        label: None,
                        color: Some(crate::legend::color32_to_srgb(color)),
                    },
                ));
            }
            if ui.button("Delete").clicked() {
                action.marker_delete = Some(m.id);
                ui.close();
            }
        });
    }

    scrubbed
}

/// Two-ended range slider setting the visible X window. Dragging an end moves
/// that bound; dragging the band pans. Returns whether the view changed.
fn window_slider(ui: &mut egui::Ui, view: &mut ViewX, range: TimeRange) -> bool {
    let height = 16.0;
    let (rect, _resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    if !ui.is_rect_visible(rect) {
        return false;
    }

    let span = (range.max_us - range.min_us).max(1);
    // Keep ~8 px of window so both handles stay grabbable.
    let min_span_us = (span as f64 * (8.0 / rect.width().max(1.0)) as f64) as i64 + 1;

    // The view may extend beyond the data range; don't clamp here — handles
    // ride the bar edges (via `bar_x_at`'s clamp) and grey out when out of range.
    let lo_in_range = (range.min_us..=range.max_us).contains(&view.min_us);
    let hi_in_range = (range.min_us..=range.max_us).contains(&view.max_us);

    let hw = 5.0;
    let lo_rect = egui::Rect::from_center_size(
        egui::pos2(bar_x_at(view.min_us, rect, range), rect.center().y),
        egui::vec2(hw * 2.0, height),
    );
    let hi_rect = egui::Rect::from_center_size(
        egui::pos2(bar_x_at(view.max_us, rect, range), rect.center().y),
        egui::vec2(hw * 2.0, height),
    );
    let mid_rect = egui::Rect::from_min_max(
        egui::pos2(lo_rect.right().min(hi_rect.left()), rect.top()),
        egui::pos2(hi_rect.left().max(lo_rect.right()), rect.bottom()),
    );

    let id = ui.id();
    let lo_resp = ui.interact(lo_rect, id.with("win_lo"), egui::Sense::drag());
    let hi_resp = ui.interact(hi_rect, id.with("win_hi"), egui::Sense::drag());
    let mid_resp = ui.interact(mid_rect, id.with("win_mid"), egui::Sense::drag());

    let mut changed = false;
    if lo_resp.dragged()
        && let Some(p) = lo_resp.interact_pointer_pos()
    {
        let t = bar_time_at(p.x, rect, range).min(view.max_us - min_span_us);
        view.min_us = t.clamp(range.min_us, range.max_us);
        changed = true;
    }
    if hi_resp.dragged()
        && let Some(p) = hi_resp.interact_pointer_pos()
    {
        let t = bar_time_at(p.x, rect, range).max(view.min_us + min_span_us);
        view.max_us = t.clamp(range.min_us, range.max_us);
        changed = true;
    }
    if mid_resp.dragged() {
        let dx = mid_resp.drag_delta().x;
        if dx != 0.0 {
            // Pan without clamping; the window may move past either end.
            let win = view.max_us - view.min_us;
            let delta = (dx as f64 / rect.width().max(1.0) as f64 * span as f64).round() as i64;
            view.min_us = view.min_us.saturating_add(delta);
            view.max_us = view.min_us + win;
            changed = true;
        }
    }

    let painter = ui.painter();
    let visuals = ui.visuals();
    let bar = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 6.0));
    painter.rect_filled(bar, 3.0, visuals.extreme_bg_color);
    let band = egui::Rect::from_min_max(
        egui::pos2(bar_x_at(view.min_us, rect, range), bar.top()),
        egui::pos2(bar_x_at(view.max_us, rect, range), bar.bottom()),
    );
    painter.rect_filled(band, 3.0, visuals.selection.bg_fill);
    let handle = |t_us: i64, active: bool, in_range: bool| {
        let c = if !in_range {
            visuals.weak_text_color()
        } else if active {
            visuals.strong_text_color()
        } else {
            visuals.text_color()
        };
        let x = bar_x_at(t_us, rect, range);
        painter.vline(x, rect.y_range(), egui::Stroke::new(2.0, c));
        painter.circle_filled(egui::pos2(x, rect.center().y), 4.0, c);
    };
    handle(
        view.min_us,
        lo_resp.hovered() || lo_resp.dragged(),
        lo_in_range,
    );
    handle(
        view.max_us,
        hi_resp.hovered() || hi_resp.dragged(),
        hi_in_range,
    );

    changed
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::{FieldId, IdentityRegistry};
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;

    use super::*;

    fn range() -> TimeRange {
        TimeRange::new(1_000_000, 5_000_000).unwrap()
    }

    fn stepping_fixture() -> (StoreSnapshot, FieldId) {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "BARO").unwrap();
        let alt = identity.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0]))];
        let chunk = Arc::new(
            Chunk::try_new(Int64Array::from(vec![1_000, 2_000, 3_000]), cols, &schema).unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();
        (snapshot, alt)
    }

    #[test]
    fn step_moves_to_the_adjacent_sample_of_the_reference_trace() {
        let (snapshot, alt) = stepping_fixture();
        assert_eq!(step_target(&snapshot, Some(alt), 1_500, true), 2_000);
        assert_eq!(step_target(&snapshot, Some(alt), 2_000, true), 3_000);
        assert_eq!(step_target(&snapshot, Some(alt), 2_000, false), 1_000);
        assert_eq!(step_target(&snapshot, Some(alt), 3_000, true), 3_000);
        assert_eq!(step_target(&snapshot, Some(alt), 1_000, false), 1_000);
    }

    #[test]
    fn step_falls_back_to_a_thirtieth_of_a_second_without_a_focus() {
        let (snapshot, _) = stepping_fixture();
        assert_eq!(step_target(&snapshot, None, 1_000_000, true), 1_033_333);
        assert_eq!(step_target(&snapshot, None, 1_000_000, false), 966_667);
    }

    #[test]
    fn jump_moves_to_the_range_ends() {
        let mut p = Playback {
            playing: true,
            t_us: 3_000_000,
            speed: 1.0,
            follow_live: false,
        };
        p.jump_start(range());
        assert_eq!(p.t_us, 1_000_000);
        p.jump_end(range());
        assert_eq!(p.t_us, 5_000_000);
    }

    #[test]
    fn advance_moves_by_wall_clock_times_speed() {
        let mut p = Playback {
            playing: true,
            t_us: 1_000_000,
            speed: 2.0,
            follow_live: false,
        };
        p.advance(0.5, range());
        assert_eq!(p.t_us, 2_000_000);
        assert!(p.playing);
    }

    #[test]
    fn advance_does_nothing_while_paused() {
        let mut p = Playback {
            playing: false,
            t_us: 1_500_000,
            speed: 1.0,
            follow_live: false,
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
            follow_live: false,
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
    fn follow_live_pins_to_range_end_as_it_grows() {
        let mut p = Playback::default();
        p.lock_to_live(range());
        assert!(p.follow_live);
        assert_eq!(p.t_us, 5_000_000);

        let grown = TimeRange::new(1_000_000, 7_000_000).unwrap();
        p.clamp_to(grown);
        assert_eq!(p.t_us, 7_000_000);

        p.advance(10.0, TimeRange::new(1_000_000, 9_000_000).unwrap());
        assert_eq!(p.t_us, 9_000_000);
    }

    #[test]
    fn manual_scrub_disengages_follow_live() {
        let mut p = Playback::default();
        p.lock_to_live(range());
        p.scrub(2_000_000, range());
        assert!(!p.follow_live);
        assert_eq!(p.t_us, 2_000_000);
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
    fn relative_time_formats_as_minutes_seconds_millis() {
        assert_eq!(format_relative(1_000_000, 1_000_000), "0:00.000");
        assert_eq!(format_relative(84_456_000, 1_000_000), "1:23.456");
        assert_eq!(format_relative(3_725_500_000, 0), "1:02:05.500");
        assert_eq!(format_relative(0, 1_000_000), "0:00.000");
    }

    #[test]
    fn absolute_time_formats_as_utc_civil_date() {
        let unix_us = 1_781_181_296_789_000; // 2026-06-11 12:34:56.789 UTC
        assert_eq!(format_utc(unix_us), "2026-06-11 12:34:56.789 UTC");
        assert_eq!(format_utc(0), "1970-01-01 00:00:00.000 UTC");
        let feb29 = 1_709_164_800_000_000; // 2024-02-29 00:00:00 UTC
        assert_eq!(format_utc(feb29), "2024-02-29 00:00:00.000 UTC");
    }

    #[test]
    fn scrubber_mapping_round_trips_between_time_and_x() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 0.0), egui::vec2(200.0, 16.0));
        let range = range();

        assert_eq!(bar_time_at(100.0, rect, range), 1_000_000);
        assert_eq!(bar_time_at(300.0, rect, range), 5_000_000);
        assert_eq!(bar_time_at(200.0, rect, range), 3_000_000);
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
            follow_live: false,
        };
        p.clamp_to(range());
        assert_eq!(p.t_us, 5_000_000);
        assert!(p.playing, "clamp_to never touches play state");
    }
}
