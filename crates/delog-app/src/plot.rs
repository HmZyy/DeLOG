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
    pub color: [f32; 4],
    pub width_px: f32,
}

/// One plot pane. M3 ships a single pane; the tile workspace is PLT-01.
#[derive(Debug, Default)]
pub struct PlotPane {
    pub traces: Vec<TraceRef>,
    view: Option<ViewX>,
}

impl PlotPane {
    /// Add `field` if not already plotted, assigning the next palette colour
    /// (PLT-02). Returns whether it was newly added.
    pub fn add_trace(&mut self, field: FieldId) -> bool {
        if self.traces.iter().any(|t| t.field == field) {
            return false;
        }
        let color = palette::trace_color(self.traces.len()).to_linear_f32();
        self.traces.push(TraceRef {
            field,
            color,
            width_px: 1.5,
        });
        true
    }

    pub fn is_empty(&self) -> bool {
        self.traces.is_empty()
    }

    /// The current X view, or `None` until initialised from the data range.
    pub fn view(&self) -> Option<ViewX> {
        self.view
    }

    pub fn set_view(&mut self, view: ViewX) {
        self.view = Some(view);
    }

    /// Initialise the view to the full data range on first data (PLT-04
    /// reset-to-full default). No-op once a view exists.
    pub fn init_view(&mut self, range: TimeRange) {
        self.view.get_or_insert_with(|| ViewX::from_range(range));
    }

    /// Reset the view to the full data range (double-click, PLT-04).
    pub fn reset_view(&mut self, range: TimeRange) {
        self.view = Some(ViewX::from_range(range));
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
    }

    #[test]
    fn view_initialises_once_then_resets_explicitly() {
        let mut pane = PlotPane::default();
        assert!(pane.view().is_none());
        pane.init_view(TimeRange::new(0, 1000).unwrap());
        assert_eq!(pane.view().unwrap(), ViewX::new(0, 1000));
        // init_view is a no-op once set; reset_view replaces it.
        pane.init_view(TimeRange::new(0, 5000).unwrap());
        assert_eq!(pane.view().unwrap(), ViewX::new(0, 1000));
        pane.reset_view(TimeRange::new(0, 5000).unwrap());
        assert_eq!(pane.view().unwrap(), ViewX::new(0, 5000));
    }
}
