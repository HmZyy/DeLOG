use std::collections::HashMap;

use delog_core::identity::FieldId;
use delog_core::time::TimeRange;
use delog_render::palette;

const MIN_SPAN_US: f64 = 1.0;
const MAX_SPAN_US: f64 = 1e18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewX {
    pub min_us: i64,
    pub max_us: i64,
}

impl ViewX {
    pub fn new(min_us: i64, max_us: i64) -> Self {
        if max_us > min_us {
            let span = ((max_us as i128 - min_us as i128).min(i64::MAX as i128)) as i64;
            Self::from_min_and_span(min_us as i128, span)
        } else {
            Self::from_min_and_span(min_us as i128, 1)
        }
    }

    pub fn from_range(range: TimeRange) -> Self {
        Self::new(range.min_us, range.max_us)
    }

    /// Builds a view only when its positive span fits the signed view math.
    /// A point at the upper timestamp boundary expands downward so the sample
    /// remains visible without overflowing.
    pub fn try_from_range(range: TimeRange) -> Option<Self> {
        if range.min_us == range.max_us {
            return Some(Self::from_min_and_span(range.min_us as i128, 1));
        }
        let span = range.max_us as i128 - range.min_us as i128;
        (span > 0 && span <= i64::MAX as i128)
            .then(|| Self::from_min_and_span(range.min_us as i128, span as i64))
    }

    pub fn locked_to_tail(range: TimeRange, span_us: i64) -> Self {
        let span_us = span_us.max(1);
        let min_us = range.max_us.saturating_sub(span_us).max(range.min_us);
        Self::new(min_us, range.max_us)
    }

    pub fn span_us(&self) -> i64 {
        (self.max_us as i128 - self.min_us as i128).clamp(1, i64::MAX as i128) as i64
    }

    pub fn pan_us(&mut self, delta_us: i64) {
        *self = Self::from_min_and_span(self.min_us as i128 + delta_us as i128, self.span_us());
    }

    /// `factor < 1` zooms in, `> 1` zooms out; `focus_us` stays fixed on screen.
    pub fn zoom_at(&mut self, focus_us: i64, factor: f64) {
        let span = self.span_us() as f64;
        let new_span = (span * factor).clamp(MIN_SPAN_US, MAX_SPAN_US);
        let rel = ((focus_us as i128 - self.min_us as i128) as f64 / span).clamp(0.0, 1.0);
        let new_span = new_span.round() as i64;
        let left_span = (rel * new_span as f64).round().clamp(0.0, new_span as f64) as i64;
        *self = Self::from_min_and_span(focus_us as i128 - left_span as i128, new_span);
    }

    pub fn seconds(&self, origin_us: i64) -> (f32, f32) {
        (
            ((self.min_us as i128 - origin_us as i128) as f64 * 1e-6) as f32,
            ((self.max_us as i128 - origin_us as i128) as f64 * 1e-6) as f32,
        )
    }

    fn from_min_and_span(min_us: i128, span_us: i64) -> Self {
        let span_us = span_us.max(1);
        let min_limit = i64::MIN as i128;
        let max_limit = i64::MAX as i128 - span_us as i128;
        let min_us = min_us.clamp(min_limit, max_limit);
        Self {
            min_us: min_us as i64,
            max_us: (min_us + span_us as i128) as i64,
        }
    }
}

pub fn draw_zoom_drag_overlay(ui: &egui::Ui, plot_rect: egui::Rect, anchor_x: f32, cursor_x: f32) {
    let anchor_x = anchor_x.clamp(plot_rect.left(), plot_rect.right());
    let cursor_x = cursor_x.clamp(plot_rect.left(), plot_rect.right());
    let (lo, hi) = (anchor_x.min(cursor_x), anchor_x.max(cursor_x));
    let painter = ui.painter();
    let shade = egui::Color32::from_black_alpha(120);
    painter.rect_filled(
        egui::Rect::from_min_max(plot_rect.left_top(), egui::pos2(lo, plot_rect.bottom())),
        0.0,
        shade,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(hi, plot_rect.top()), plot_rect.right_bottom()),
        0.0,
        shade,
    );
    let edge = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(160));
    painter.vline(lo, plot_rect.y_range(), edge);
    painter.vline(hi, plot_rect.y_range(), edge);
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceRef {
    pub field: FieldId,
    /// sRGB straight RGBA; the renderer converts to the target's colour space.
    pub color: [f32; 4],
    pub width_px: f32,
    pub mode: TraceMode,
    pub visible: bool,
    /// Session-only, per-plot rename. `None` = derived `topic.field` label.
    pub label_override: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GhostTrace {
    pub source: Option<String>,
    pub topic: String,
    pub field: String,
    pub color: [f32; 4],
    pub width_px: f32,
    pub mode: TraceMode,
    pub visible: bool,
    pub text_filter: Option<String>,
    pub text_offsets: Vec<(i64, f32)>,
}

impl TraceRef {
    pub fn color32(&self) -> egui::Color32 {
        let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        egui::Color32::from_rgba_unmultiplied(
            u(self.color[0]),
            u(self.color[1]),
            u(self.color[2]),
            u(self.color[3]),
        )
    }

    pub fn display_label<'a>(&'a self, canonical: &'a str) -> &'a str {
        self.label_override.as_deref().unwrap_or(canonical)
    }
}

impl GhostTrace {
    pub fn color32(&self) -> egui::Color32 {
        let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        egui::Color32::from_rgba_unmultiplied(
            u(self.color[0]),
            u(self.color[1]),
            u(self.color[2]),
            u(self.color[3]),
        )
    }

    /// Dimmed swatch color matching how the ghost renders in the legend.
    pub fn display_color32(&self) -> egui::Color32 {
        self.color32().gamma_multiply(0.45)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceMode {
    Line,
    Scatter,
    Step,
}

impl TraceMode {
    pub const ALL: [Self; 3] = [Self::Line, Self::Scatter, Self::Step];

    pub fn label(self) -> &'static str {
        match self {
            Self::Line => "Line",
            Self::Scatter => "Scatter",
            Self::Step => "Step",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenameDialog {
    pub field: FieldId,
    pub text: String,
}

pub fn rename_value(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[derive(Debug)]
pub struct PlotPane {
    pub traces: Vec<TraceRef>,
    pub ghosts: Vec<GhostTrace>,
    pub show_legend: bool,
    pub show_tooltip: bool,
    pub show_info: bool,
    pub marker_drag: bool,
    /// Keyed by `(field, sample t_us)`; value is a y-fraction (0 = top .. 1 = bottom).
    pub text_offsets: HashMap<(FieldId, i64), f32>,
    /// Empty/absent = show all.
    pub text_filters: HashMap<FieldId, String>,
    /// Anchor time (µs) of an in-progress right-drag zoom; None when not dragging.
    pub zoom_drag_anchor_us: Option<i64>,
    /// Transient rename-dialog state; `Some` while the dialog is open.
    pub rename: Option<RenameDialog>,
    pub annotations: crate::plotting::annotations::AnnotationLayer,
}

impl Default for PlotPane {
    fn default() -> Self {
        Self {
            traces: Vec::new(),
            ghosts: Vec::new(),
            show_legend: true,
            show_tooltip: true,
            show_info: false,
            marker_drag: false,
            text_offsets: HashMap::new(),
            text_filters: HashMap::new(),
            zoom_drag_anchor_us: None,
            rename: None,
            annotations: crate::plotting::annotations::AnnotationLayer::default(),
        }
    }
}

impl PlotPane {
    pub fn add_trace(&mut self, field: FieldId) -> bool {
        if self.traces.iter().any(|t| t.field == field) {
            return false;
        }
        let color = palette::trace_color(self.traces.len()).to_srgb_f32();
        self.traces.push(TraceRef {
            field,
            color,
            width_px: 1.5,
            mode: TraceMode::Line,
            visible: true,
            label_override: None,
        });
        true
    }

    pub fn add_trace_ref(&mut self, trace: TraceRef) -> bool {
        if self.traces.iter().any(|t| t.field == trace.field) {
            return false;
        }
        self.traces.push(trace);
        true
    }

    pub fn remove_trace(&mut self, field: FieldId) {
        self.traces.retain(|t| t.field != field);
    }

    pub fn add_ghost(&mut self, ghost: GhostTrace) {
        if !self
            .ghosts
            .iter()
            .any(|g| g.source == ghost.source && g.topic == ghost.topic && g.field == ghost.field)
        {
            self.ghosts.push(ghost);
        }
    }

    /// Drop the ghost (missing) trace at `index`. Out-of-range indices are ignored.
    pub fn remove_ghost(&mut self, index: usize) {
        if index < self.ghosts.len() {
            self.ghosts.remove(index);
        }
    }

    pub fn clear(&mut self) {
        self.traces.clear();
        self.ghosts.clear();
    }

    pub fn trace_mut(&mut self, field: FieldId) -> Option<&mut TraceRef> {
        self.traces.iter_mut().find(|t| t.field == field)
    }

    pub fn visible_traces(&self) -> impl Iterator<Item = &TraceRef> {
        self.traces.iter().filter(|t| t.visible)
    }

    pub fn is_empty(&self) -> bool {
        self.traces.is_empty() && self.ghosts.is_empty()
    }

    pub fn fields(&self) -> impl Iterator<Item = FieldId> + '_ {
        self.traces.iter().map(|t| t.field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_shifts_both_bounds() {
        let mut v = ViewX::new(0, 1000);
        v.pan_us(250);
        assert_eq!((v.min_us, v.max_us), (250, 1250));
        v.pan_us(-500);
        assert_eq!((v.min_us, v.max_us), (-250, 750));
        assert_eq!(v.span_us(), 1000);
    }

    #[test]
    fn zoom_keeps_focus_fixed() {
        let mut v = ViewX::new(0, 1000);
        v.zoom_at(500, 0.5);
        assert_eq!((v.min_us, v.max_us), (250, 750));

        let mut v = ViewX::new(0, 1000);
        v.zoom_at(0, 0.5);
        assert_eq!(v.min_us, 0);
        assert_eq!(v.span_us(), 500);
    }

    #[test]
    fn zoom_clamps_to_a_minimum_span() {
        let mut v = ViewX::new(0, 1000);
        for _ in 0..200 {
            v.zoom_at(500, 0.5);
        }
        assert!(v.span_us() >= 1);
    }

    #[test]
    fn seconds_rebases_to_origin() {
        let v = ViewX::new(5_000_000, 7_000_000);
        let (a, b) = v.seconds(5_000_000);
        assert_eq!(a, 0.0);
        assert_eq!(b, 2.0);
    }

    #[test]
    fn seconds_uses_widened_differences_at_opposite_timestamp_boundaries() {
        let view = ViewX {
            min_us: i64::MAX - 1,
            max_us: i64::MAX,
        };
        let expected_min = (((i64::MAX - 1) as i128 - i64::MIN as i128) as f64 * 1e-6) as f32;
        let expected_max = ((i64::MAX as i128 - i64::MIN as i128) as f64 * 1e-6) as f32;

        assert_eq!(view.seconds(i64::MIN), (expected_min, expected_max));
    }

    #[test]
    fn zoom_at_max_keeps_a_representable_positive_span() {
        let mut view = ViewX::new(i64::MAX - 1_000, i64::MAX);
        view.zoom_at(i64::MAX, 0.5);

        assert_eq!(view, ViewX::new(i64::MAX - 500, i64::MAX));
        assert_eq!(view.span_us(), 500);
    }

    #[test]
    fn pan_clamps_the_pair_at_both_boundaries_without_collapsing_its_span() {
        let mut upper = ViewX::new(i64::MAX - 100, i64::MAX - 50);
        upper.pan_us(1_000);
        assert_eq!(upper, ViewX::new(i64::MAX - 50, i64::MAX));
        assert_eq!(upper.span_us(), 50);

        let mut lower = ViewX::new(i64::MIN + 50, i64::MIN + 100);
        lower.pan_us(-1_000);
        assert_eq!(lower, ViewX::new(i64::MIN, i64::MIN + 50));
        assert_eq!(lower.span_us(), 50);
    }

    #[test]
    fn constructor_at_max_keeps_the_view_invariant() {
        assert_eq!(
            ViewX::new(i64::MAX, i64::MAX),
            ViewX {
                min_us: i64::MAX - 1,
                max_us: i64::MAX,
            }
        );
    }

    #[test]
    fn add_trace_dedups_and_assigns_distinct_palette_colors() {
        let mut pane = PlotPane::default();
        assert!(pane.add_trace(FieldId(0)));
        assert!(pane.add_trace(FieldId(1)));
        assert!(!pane.add_trace(FieldId(0)));
        assert_eq!(pane.traces.len(), 2);
        assert_ne!(pane.traces[0].color, pane.traces[1].color);
        assert_eq!(pane.traces[0].mode, TraceMode::Line);
    }

    #[test]
    fn add_trace_has_no_label_override_and_display_label_prefers_override() {
        let mut pane = PlotPane::default();
        pane.add_trace(FieldId(0));
        assert_eq!(pane.traces[0].label_override, None);
        assert_eq!(pane.traces[0].display_label("topic.field"), "topic.field");
        pane.traces[0].label_override = Some("renamed".to_string());
        assert_eq!(pane.traces[0].display_label("topic.field"), "renamed");
    }

    #[test]
    fn remove_ghost_drops_only_the_indexed_entry_and_ignores_out_of_range() {
        let ghost = |field: &str| GhostTrace {
            source: None,
            topic: "TOPIC".to_string(),
            field: field.to_string(),
            color: [1.0, 1.0, 1.0, 1.0],
            width_px: 1.5,
            mode: TraceMode::Line,
            visible: true,
            text_filter: None,
            text_offsets: Vec::new(),
        };
        let mut pane = PlotPane::default();
        pane.add_ghost(ghost("a"));
        pane.add_ghost(ghost("b"));

        pane.remove_ghost(0);
        assert_eq!(pane.ghosts.len(), 1);
        assert_eq!(pane.ghosts[0].field, "b");

        pane.remove_ghost(5);
        assert_eq!(pane.ghosts.len(), 1);
    }

    #[test]
    fn traces_default_visible_and_toggle_via_trace_mut() {
        let mut pane = PlotPane::default();
        pane.add_trace(FieldId(0));
        pane.add_trace(FieldId(1));
        assert!(pane.traces.iter().all(|t| t.visible));
        assert_eq!(pane.visible_traces().count(), 2);

        pane.trace_mut(FieldId(0)).unwrap().visible = false;
        assert_eq!(pane.visible_traces().count(), 1);

        pane.remove_trace(FieldId(0));
        assert_eq!(pane.traces.len(), 1);
        assert_eq!(pane.traces[0].field, FieldId(1));
        pane.clear();
        assert!(pane.is_empty());
    }

    #[test]
    fn view_initialises_from_range() {
        let view = ViewX::from_range(TimeRange::new(0, 1000).unwrap());
        assert_eq!(view, ViewX::new(0, 1000));
    }

    #[test]
    fn checked_range_normalizes_a_max_boundary_point_without_excluding_it() {
        let view = ViewX::try_from_range(TimeRange::point(i64::MAX)).unwrap();
        assert_eq!(
            view,
            ViewX {
                min_us: i64::MAX - 1,
                max_us: i64::MAX,
            }
        );
    }

    #[test]
    fn checked_range_rejects_a_span_that_signed_view_math_cannot_represent() {
        assert_eq!(
            ViewX::try_from_range(TimeRange::new(i64::MIN, i64::MAX).unwrap()),
            None
        );
    }

    #[test]
    fn tail_lock_preserves_span_when_possible() {
        let range = TimeRange::new(0, 10_000).unwrap();
        assert_eq!(
            ViewX::locked_to_tail(range, 2_000),
            ViewX::new(8_000, 10_000)
        );
    }

    #[test]
    fn tail_lock_clamps_to_full_range_when_span_is_too_large() {
        let range = TimeRange::new(1_000, 3_000).unwrap();
        assert_eq!(
            ViewX::locked_to_tail(range, 10_000),
            ViewX::new(1_000, 3_000)
        );
    }

    #[test]
    fn add_trace_ref_inserts_full_trace_and_dedups_without_overwrite() {
        let mut pane = PlotPane::default();
        let t = TraceRef {
            field: FieldId(7),
            color: [0.1, 0.2, 0.3, 1.0],
            width_px: 4.0,
            mode: TraceMode::Step,
            visible: false,
            label_override: Some("v".to_string()),
        };
        assert!(pane.add_trace_ref(t.clone()));
        assert_eq!(pane.traces.len(), 1);
        assert_eq!(pane.traces[0].width_px, 4.0);
        assert_eq!(pane.traces[0].label_override.as_deref(), Some("v"));

        let dup = TraceRef { width_px: 9.0, ..t };
        assert!(!pane.add_trace_ref(dup));
        assert_eq!(pane.traces.len(), 1);
        assert_eq!(pane.traces[0].width_px, 4.0);
    }

    #[test]
    fn rename_value_trims_and_clears_on_empty() {
        assert_eq!(rename_value("  hi "), Some("hi".to_string()));
        assert_eq!(rename_value("   "), None);
        assert_eq!(rename_value(""), None);
    }
}
