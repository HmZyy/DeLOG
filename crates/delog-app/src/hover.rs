//! Hover readout: cursor line, per-trace circles and a value tooltip (PLAN.md
//! §10.5, PLT-09).
//!
//! On hover the cursor's x maps to a canonical time; each visible trace is
//! sampled there via [`FieldView::sample_at`] (the canonical binary search,
//! CORE-07), and the raw value × the field multiplier is shown — precise and
//! independent of the f32 render cache (§4.5). A circle marks each trace's
//! sample; a tooltip lists the values.

use delog_core::field_view::{FieldView, SampleMode};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::gpu::PaneView;
use crate::legend::trace_label;
use crate::plot::PlotPane;

/// Draw the hover cursor/circles/tooltip if the pointer is over the plot.
pub fn draw(
    ui: &egui::Ui,
    view: PaneView,
    response: &egui::Response,
    snapshot: &StoreSnapshot,
    pane: &PlotPane,
    origin_us: i64,
    mode: SampleMode,
) {
    let Some(pos) = response.hover_pos() else {
        return;
    };
    let rect = view.rect;
    if !rect.contains(pos) {
        return;
    }
    let (x0, x1) = view.x_range;
    let (y0, y1) = view.y_range;
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let painter = ui.painter();
    // Vertical cursor line at the pointer.
    painter.vline(
        pos.x,
        rect.y_range(),
        egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
    );

    let cursor_x_sec = x0 + (pos.x - rect.left()) / rect.width() * (x1 - x0);
    let cursor_us = origin_us + (cursor_x_sec as f64 * 1e6) as i64;

    let to_x = |x_sec: f32| rect.left() + (x_sec - x0) / (x1 - x0) * rect.width();
    let to_y = |y: f32| rect.bottom() - (y - y0) / (y1 - y0) * rect.height();

    let mut rows: Vec<(String, f64, Option<String>, egui::Color32)> = Vec::new();
    for trace in pane.visible_traces() {
        let Ok(fv) = FieldView::new(snapshot, trace.field) else {
            continue;
        };
        let Some(sample) = fv.sample_at(cursor_us, mode) else {
            continue;
        };
        let Some(raw) = sample.value.as_f64() else {
            continue;
        };
        let (mult, unit) = field_meta(snapshot, trace.field);
        let value = raw * mult;

        let sx = to_x((sample.effective_time_us - origin_us) as f32 * 1e-6);
        let sy = to_y(value as f32);
        let color = trace.color32();
        if rect.contains(egui::pos2(sx, sy)) {
            painter.circle_stroke(egui::pos2(sx, sy), 3.5, egui::Stroke::new(1.5, color));
        }
        rows.push((trace_label(snapshot, trace.field), value, unit, color));
    }

    if rows.is_empty() {
        return;
    }

    egui::Area::new(egui::Id::new("plot_hover"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos + egui::vec2(12.0, 12.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(egui::RichText::new(format!("t = {cursor_x_sec:.3} s")).weak());
                for (label, value, unit, color) in &rows {
                    ui.horizontal(|ui| {
                        ui.colored_label(*color, "■");
                        let unit = unit.as_deref().unwrap_or("");
                        ui.label(format!("{label}: {} {unit}", format_value(*value)));
                    });
                }
            });
        });
}

/// `(multiplier, unit)` for a field from the topic schema (core API, no Arrow).
fn field_meta(snapshot: &StoreSnapshot, field: FieldId) -> (f64, Option<String>) {
    let Some(entry) = snapshot.fields.get(field.index()).filter(|f| f.id == field) else {
        return (1.0, None);
    };
    let Some(store) = snapshot.topic(entry.topic).and_then(|t| t.store.as_ref()) else {
        return (1.0, None);
    };
    match store.schema.field_by_name(&entry.name) {
        Some(fs) => (fs.multiplier, fs.unit.clone()),
        None => (1.0, None),
    }
}

/// Compact value formatting: scientific for very large/small magnitudes.
fn format_value(v: f64) -> String {
    let a = v.abs();
    if v != 0.0 && !(1e-3..1e6).contains(&a) {
        format!("{v:.3e}")
    } else {
        format!("{v:.4}")
    }
}
