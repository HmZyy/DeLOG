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

pub struct HoverTarget {
    pub id: egui::Id,
    pub view: PaneView,
}

/// Draw the hover cursor/circles/tooltip if the pointer is over the plot. With
/// `tooltip: false` the cursor line and sample circles still draw but the value
/// tooltip is suppressed — used during playback, where the playhead readout
/// (PLT-10) is the value source instead.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    ui: &egui::Ui,
    target: HoverTarget,
    response: &egui::Response,
    snapshot: &StoreSnapshot,
    pane: &PlotPane,
    origin_us: i64,
    mode: SampleMode,
    tooltip: bool,
) {
    let Some(pos) = response.hover_pos() else {
        return;
    };
    let view = target.view;
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

    let rows = sampled_rows(snapshot, pane, cursor_us, mode);
    for row in &rows {
        let sx = to_x((row.effective_time_us - origin_us) as f32 * 1e-6);
        let sy = to_y(row.value as f32);
        if rect.contains(egui::pos2(sx, sy)) {
            painter.circle_stroke(egui::pos2(sx, sy), 3.5, egui::Stroke::new(1.5, row.color));
        }
    }

    if tooltip {
        show_tooltip(
            ui,
            target.id,
            pos + egui::vec2(12.0, 12.0),
            egui::Align2::LEFT_TOP,
            cursor_x_sec,
            &rows,
        );
    }
}

/// One tooltip row: a trace's canonical value at the probed time.
struct Row {
    label: String,
    value: f64,
    unit: Option<String>,
    color: egui::Color32,
    effective_time_us: i64,
}

/// Sample every visible trace at `t_us` (canonical binary search, multiplier
/// applied) — shared by the hover and playhead readouts.
fn sampled_rows(
    snapshot: &StoreSnapshot,
    pane: &PlotPane,
    t_us: i64,
    mode: SampleMode,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for trace in pane.visible_traces() {
        let Ok(fv) = FieldView::new(snapshot, trace.field) else {
            continue;
        };
        let Some(sample) = fv.sample_at(t_us, mode) else {
            continue;
        };
        let Some(raw) = sample.value.as_f64() else {
            continue;
        };
        let (mult, unit) = field_meta(snapshot, trace.field);
        rows.push(Row {
            label: trace_label(snapshot, trace.field),
            value: raw * mult,
            unit,
            color: trace.color32(),
            effective_time_us: sample.effective_time_us,
        });
    }
    rows
}

/// The shared value tooltip: time header + one colored `label: value unit`
/// row per trace. Used by hover (PLT-09) and the playhead readout (PLT-10).
fn show_tooltip(
    ui: &egui::Ui,
    id: egui::Id,
    pos: egui::Pos2,
    pivot: egui::Align2,
    t_sec: f32,
    rows: &[Row],
) {
    if rows.is_empty() {
        return;
    }
    egui::Area::new(id)
        .order(egui::Order::Tooltip)
        .pivot(pivot)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(egui::RichText::new(format!("t = {t_sec:.3} s")).weak());
                for row in rows {
                    ui.horizontal(|ui| {
                        ui.colored_label(row.color, "■");
                        let unit = row.unit.as_deref().unwrap_or("");
                        ui.label(format!("{}: {} {unit}", row.label, format_value(row.value)));
                    });
                }
            });
        });
}

/// Playhead cursor (§10.5/§11, PLT-10): a vertical line at the playback time
/// on every pane; with `readout` set, the shared hover tooltip shows the
/// values, anchored to the bottom of the line (flipping side near the right
/// edge). The caller passes `readout: None` on the hovered pane — the hover
/// tooltip is already there — and outside alt-scrub/playback.
pub fn draw_playhead(
    ui: &egui::Ui,
    target: HoverTarget,
    snapshot: &StoreSnapshot,
    pane: &PlotPane,
    origin_us: i64,
    t_us: i64,
    readout: Option<SampleMode>,
) {
    let view = target.view;
    let rect = view.rect;
    let (x0, x1) = view.x_range;
    if x1 <= x0 {
        return;
    }
    let t_sec = ((t_us - origin_us) as f64 * 1e-6) as f32;
    let frac = (t_sec - x0) / (x1 - x0);
    if !(0.0..=1.0).contains(&frac) {
        return;
    }
    let x = rect.left() + frac * rect.width();

    let painter = ui.painter();
    let color = ui.visuals().warn_fg_color;
    painter.vline(x, rect.y_range(), egui::Stroke::new(1.5, color));

    let Some(mode) = readout else {
        return;
    };
    let rows = sampled_rows(snapshot, pane, t_us, mode);
    let on_left = x > rect.right() - 160.0;
    let (pos, pivot) = if on_left {
        (
            egui::pos2(x - 8.0, rect.bottom() - 4.0),
            egui::Align2::RIGHT_BOTTOM,
        )
    } else {
        (
            egui::pos2(x + 8.0, rect.bottom() - 4.0),
            egui::Align2::LEFT_BOTTOM,
        )
    };
    show_tooltip(ui, target.id, pos, pivot, t_sec, &rows);
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
