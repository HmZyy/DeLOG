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
        Self {
            min_us,
            max_us: max_us.max(min_us + 1),
        }
    }

    pub fn from_range(range: TimeRange) -> Self {
        Self::new(range.min_us, range.max_us)
    }

    pub fn locked_to_tail(range: TimeRange, span_us: i64) -> Self {
        let span_us = span_us.max(1);
        let min_us = range.max_us.saturating_sub(span_us).max(range.min_us);
        Self::new(min_us, range.max_us)
    }

    pub fn span_us(&self) -> i64 {
        (self.max_us - self.min_us).max(1)
    }

    pub fn pan_us(&mut self, delta_us: i64) {
        self.min_us = self.min_us.saturating_add(delta_us);
        self.max_us = self.max_us.saturating_add(delta_us);
    }

    /// `factor < 1` zooms in, `> 1` zooms out; `focus_us` stays fixed on screen.
    pub fn zoom_at(&mut self, focus_us: i64, factor: f64) {
        let span = self.span_us() as f64;
        let new_span = (span * factor).clamp(MIN_SPAN_US, MAX_SPAN_US);
        let rel = (((focus_us - self.min_us) as f64) / span).clamp(0.0, 1.0);
        let new_min = focus_us as f64 - rel * new_span;
        self.min_us = new_min.round() as i64;
        self.max_us = (new_min + new_span).round() as i64;
        if self.max_us <= self.min_us {
            self.max_us = self.min_us + 1;
        }
    }

    pub fn seconds(&self, origin_us: i64) -> (f32, f32) {
        (
            ((self.min_us - origin_us) as f64 * 1e-6) as f32,
            ((self.max_us - origin_us) as f64 * 1e-6) as f32,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TraceRef {
    pub field: FieldId,
    /// sRGB straight RGBA; the renderer converts to the target's colour space.
    pub color: [f32; 4],
    pub width_px: f32,
    pub mode: TraceMode,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GhostTrace {
    pub topic: String,
    pub field: String,
    pub color: [f32; 4],
    pub width_px: f32,
    pub mode: TraceMode,
    pub visible: bool,
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

#[derive(Debug)]
pub struct PlotPane {
    pub traces: Vec<TraceRef>,
    pub ghosts: Vec<GhostTrace>,
    pub show_legend: bool,
    pub show_tooltip: bool,
    pub show_info: bool,
    /// `None` when no marker is placed.
    pub marker_us: Option<i64>,
    pub marker_drag: bool,
    /// Keyed by `(field, sample t_us)`; value is a y-fraction (0 = top .. 1 = bottom).
    pub text_offsets: HashMap<(FieldId, i64), f32>,
    /// Empty/absent = show all.
    pub text_filters: HashMap<FieldId, String>,
}

impl Default for PlotPane {
    fn default() -> Self {
        Self {
            traces: Vec::new(),
            ghosts: Vec::new(),
            show_legend: true,
            show_tooltip: true,
            show_info: false,
            marker_us: None,
            marker_drag: false,
            text_offsets: HashMap::new(),
            text_filters: HashMap::new(),
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
        });
        true
    }

    pub fn remove_trace(&mut self, field: FieldId) {
        self.traces.retain(|t| t.field != field);
    }

    pub fn add_ghost(&mut self, ghost: GhostTrace) {
        if !self
            .ghosts
            .iter()
            .any(|g| g.topic == ghost.topic && g.field == ghost.field)
        {
            self.ghosts.push(ghost);
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
}
