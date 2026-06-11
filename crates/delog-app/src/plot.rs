//! Plot pane state and the shared X view (PLAN.md §10.2-§10.4, PLT-02/03/04).
//!
//! `ViewX` is the visible time window in canonical microseconds — the shared X
//! model every pane renders from (§10.3). Pan and zoom are pure transforms on
//! it so they are unit-testable without a GPU or window; the egui layer in
//! `app.rs` converts pointer input into these calls. `PlotPane` holds the
//! plotted [`TraceRef`]s with palette-assigned colours (PLT-02).

use delog_core::identity::FieldId;
use delog_core::time::TimeRange;
use delog_render::palette;

/// Smallest window we allow zooming to (1 µs) and a sane upper clamp.
const MIN_SPAN_US: f64 = 1.0;
const MAX_SPAN_US: f64 = 1e18;

/// The visible X window, in canonical microseconds (§10.3).
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

    pub fn span_us(&self) -> i64 {
        (self.max_us - self.min_us).max(1)
    }

    /// Shift the window by `delta_us` (drag pan, PLT-04).
    pub fn pan_us(&mut self, delta_us: i64) {
        self.min_us = self.min_us.saturating_add(delta_us);
        self.max_us = self.max_us.saturating_add(delta_us);
    }

    /// Zoom keeping `focus_us` fixed on screen. `factor < 1` zooms in (smaller
    /// window), `> 1` zooms out (wheel @ cursor, PLT-04).
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

    /// The visible window in cache seconds, rebased to `origin_us` (§8.3).
    pub fn seconds(&self, origin_us: i64) -> (f32, f32) {
        (
            ((self.min_us - origin_us) as f64 * 1e-6) as f32,
            ((self.max_us - origin_us) as f64 * 1e-6) as f32,
        )
    }
}

/// A plotted field with its render style (PLT-02).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TraceRef {
    pub field: FieldId,
    /// sRGB straight RGBA; the renderer converts to the target's colour space.
    pub color: [f32; 4],
    pub width_px: f32,
    pub mode: TraceMode,
    /// Drawn only when visible; the legend toggles this (PLT-08).
    pub visible: bool,
}

impl TraceRef {
    /// The trace colour as an egui `Color32` (the stored colour is sRGB).
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

/// One plot pane in the tiled workspace. The X view is deliberately global
/// app state so every plot pane stays synchronized (PLT-01/03). The Y axis
/// always auto-fits the visible window (pyramid min/max + pad, PLT-06).
#[derive(Debug, Default)]
pub struct PlotPane {
    pub traces: Vec<TraceRef>,
}

impl PlotPane {
    /// Add `field` if not already plotted, assigning the next palette colour
    /// (PLT-02). Returns whether it was newly added.
    pub fn add_trace(&mut self, field: FieldId) -> bool {
        if self.traces.iter().any(|t| t.field == field) {
            return false;
        }
        // Store sRGB (what egui's colour picker and the non-sRGB target both
        // expect); the renderer converts to the target's space.
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

    /// Remove a plotted trace (legend/context menu, PLT-11).
    pub fn remove_trace(&mut self, field: FieldId) {
        self.traces.retain(|t| t.field != field);
    }

    /// Remove every trace (context menu "Clear", PLT-11).
    pub fn clear(&mut self) {
        self.traces.clear();
    }

    /// Mutate one trace's style/visibility in place (legend controls).
    pub fn trace_mut(&mut self, field: FieldId) -> Option<&mut TraceRef> {
        self.traces.iter_mut().find(|t| t.field == field)
    }

    /// Traces drawn this frame (visible only).
    pub fn visible_traces(&self) -> impl Iterator<Item = &TraceRef> {
        self.traces.iter().filter(|t| t.visible)
    }

    pub fn is_empty(&self) -> bool {
        self.traces.is_empty()
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
        // Zoom in 2× around the centre (500): span 1000 → 500, centred on 500.
        v.zoom_at(500, 0.5);
        assert_eq!((v.min_us, v.max_us), (250, 750));

        // Zoom around the left edge keeps the left edge put.
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
        assert!(!pane.add_trace(FieldId(0))); // already plotted
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
}
