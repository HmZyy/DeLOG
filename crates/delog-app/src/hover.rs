//! Hover readout: cursor line, per-trace circles and a value tooltip (PLAN.md
//! §10.5, PLT-09).
//!
//! On hover the cursor's x maps to a canonical time; each visible trace is
//! sampled there via [`FieldView::sample_at`] (the canonical binary search,
//! CORE-07), and the raw value × the field multiplier is shown — precise and
//! independent of the f32 render cache (§4.5). A circle marks each trace's
//! sample; a tooltip lists the values.

use std::collections::HashMap;

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
    deltas: &HashMap<FieldId, String>,
    show_field_name: bool,
    show_time: bool,
    opacity: f32,
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

    let rows = sampled_rows(snapshot, pane, cursor_us, mode);
    draw_sample_circles(ui, view, origin_us, &rows);

    if tooltip {
        show_tooltip(
            ui,
            target.id,
            pos + egui::vec2(12.0, 12.0),
            egui::Align2::LEFT_TOP,
            cursor_x_sec,
            &rows,
            deltas,
            show_field_name,
            show_time,
            opacity,
        );
    }
}

/// Mark each visible trace with a small circle at its sampled point, mapping
/// the canonical sample time + value into screen space. Shared by the hover
/// readout (PLT-09) and the playhead readout (PLT-10) so the non-hovered panes
/// show where the cursor/playhead intersects each line, not just a value.
fn draw_sample_circles(ui: &egui::Ui, view: PaneView, origin_us: i64, rows: &[Row]) {
    let rect = view.rect;
    let (x0, x1) = view.x_range;
    let (y0, y1) = view.y_range;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let to_x = |x_sec: f32| rect.left() + (x_sec - x0) / (x1 - x0) * rect.width();
    let to_y = |y: f32| rect.bottom() - (y - y0) / (y1 - y0) * rect.height();
    let painter = ui.painter();
    for row in rows {
        let sx = to_x((row.effective_time_us - origin_us) as f32 * 1e-6);
        let sy = to_y(row.value as f32);
        if rect.contains(egui::pos2(sx, sy)) {
            painter.circle_stroke(egui::pos2(sx, sy), 3.5, egui::Stroke::new(1.5, row.color));
        }
    }
}

/// One tooltip row: a trace's canonical value at the probed time.
struct Row {
    field: FieldId,
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
            field: trace.field,
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
#[allow(clippy::too_many_arguments)]
fn show_tooltip(
    ui: &egui::Ui,
    id: egui::Id,
    pos: egui::Pos2,
    pivot: egui::Align2,
    t_sec: f32,
    rows: &[Row],
    deltas: &HashMap<FieldId, String>,
    show_field_name: bool,
    show_time: bool,
    opacity: f32,
) {
    if rows.is_empty() {
        return;
    }
    egui::Area::new(id)
        .order(egui::Order::Tooltip)
        .pivot(pivot)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            let base = egui::Frame::popup(ui.style());
            egui::Frame {
                shadow: egui::Shadow::NONE,
                fill: crate::legend::with_bg_opacity(base.fill, opacity),
                ..base
            }
            .show(ui, |ui| {
                if show_time {
                    ui.label(egui::RichText::new(format!("t = {t_sec:.3} s")).weak());
                }
                for row in rows {
                    ui.horizontal(|ui| {
                        color_swatch(ui, row.color);
                        let unit = row.unit.as_deref().unwrap_or("");
                        let value = format_value(row.value);
                        if show_field_name {
                            ui.label(format!("{}: {value} {unit}", row.label));
                        } else {
                            ui.label(format!("{value} {unit}"));
                        }
                        // Measuring-marker value delta for this trace, when
                        // routed to the readout (ANA-10); absent when shown in
                        // the legend instead.
                        if let Some(delta) = deltas.get(&row.field) {
                            ui.label(
                                egui::RichText::new(format!("d {delta}"))
                                    .color(ui.visuals().hyperlink_color)
                                    .weak(),
                            );
                        }
                    });
                }
            });
        });
}

fn color_swatch(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color);
}

/// Playhead cursor (§10.5/§11, PLT-10): a vertical line at the playback time
/// on every pane; with `readout` set, a circle marks each trace's sample at the
/// playhead and the shared hover tooltip shows the values, anchored to the
/// bottom of the line (flipping side near the right edge). The caller passes
/// `readout: None` on the hovered pane — the hover readout already draws there —
/// and outside alt-scrub/playback.
#[allow(clippy::too_many_arguments)]
pub fn draw_playhead(
    ui: &egui::Ui,
    target: HoverTarget,
    snapshot: &StoreSnapshot,
    pane: &PlotPane,
    origin_us: i64,
    t_us: i64,
    readout: Option<SampleMode>,
    deltas: &HashMap<FieldId, String>,
    show_field_name: bool,
    show_time: bool,
    opacity: f32,
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
    draw_sample_circles(ui, view, origin_us, &rows);
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
    show_tooltip(
        ui,
        target.id,
        pos,
        pivot,
        t_sec,
        &rows,
        deltas,
        show_field_name,
        show_time,
        opacity,
    );
}

/// Measurement marker (delta cursor, §10.8, ANA-10): a second vertical at
/// `marker_us`, painted dashed in a distinct accent so it never reads as the
/// playhead, with a ΔT readout (`marker − playhead`) anchored at the top of the
/// line. The per-trace ΔY is shown in the legend via [`marker_deltas`].
pub fn draw_marker(
    ui: &egui::Ui,
    view: PaneView,
    origin_us: i64,
    marker_us: i64,
    playhead_us: i64,
) {
    let rect = view.rect;
    let (x0, x1) = view.x_range;
    if x1 <= x0 {
        return;
    }
    let t_sec = ((marker_us - origin_us) as f64 * 1e-6) as f32;
    let frac = (t_sec - x0) / (x1 - x0);
    if !(0.0..=1.0).contains(&frac) {
        return;
    }
    let x = rect.left() + frac * rect.width();

    let color = ui.visuals().hyperlink_color;
    let dashes = egui::Shape::dashed_line(
        &[egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(1.5, color),
        6.0,
        4.0,
    );
    ui.painter().extend(dashes);

    // ΔT vs the playhead, anchored at the top of the line (the playhead readout
    // anchors at the bottom, so the two never collide). Flip side near the edge.
    let dt_sec = (marker_us - playhead_us) as f64 * 1e-6;
    let text = format!("dt {dt_sec:+.3} s");
    let on_left = x > rect.right() - 80.0;
    let (anchor, align) = if on_left {
        (
            egui::pos2(x - 4.0, rect.top() + 2.0),
            egui::Align2::RIGHT_TOP,
        )
    } else {
        (
            egui::pos2(x + 4.0, rect.top() + 2.0),
            egui::Align2::LEFT_TOP,
        )
    };
    ui.painter()
        .text(anchor, align, text, egui::FontId::proportional(11.0), color);
}

/// Per-trace ΔY between the marker and the playhead for the legend (§10.8,
/// ANA-10): `value(marker) − value(playhead)` sampled with the active hover
/// interpolation `mode`, the field multiplier and unit applied. Either endpoint
/// missing or non-finite (NaN is a gap, §8.2) yields "n/a". Keyed by `FieldId`.
pub fn marker_deltas(
    snapshot: &StoreSnapshot,
    pane: &PlotPane,
    marker_us: i64,
    playhead_us: i64,
    mode: SampleMode,
) -> HashMap<FieldId, String> {
    let mut out = HashMap::new();
    for trace in &pane.traces {
        let Ok(fv) = FieldView::new(snapshot, trace.field) else {
            continue;
        };
        let at_marker = fv.sample_at(marker_us, mode).and_then(|s| s.value.as_f64());
        let at_playhead = fv
            .sample_at(playhead_us, mode)
            .and_then(|s| s.value.as_f64());
        let (mult, unit) = field_meta(snapshot, trace.field);
        out.insert(
            trace.field,
            format_delta(at_marker, at_playhead, mult, unit.as_deref()),
        );
    }
    out
}

/// Format one trace's ΔY for the legend: `(marker − playhead) × multiplier`
/// with the optional unit, or "n/a" when either endpoint is missing or non-finite
/// (NaN is a gap, never interpolated across — §8.2, ANA-10).
fn format_delta(
    marker: Option<f64>,
    playhead: Option<f64>,
    mult: f64,
    unit: Option<&str>,
) -> String {
    match (marker, playhead) {
        (Some(m), Some(p)) if m.is_finite() && p.is_finite() => {
            let d = (m - p) * mult;
            let body = format_value(d);
            let signed = if d > 0.0 { format!("+{body}") } else { body };
            match unit {
                Some(u) if !u.is_empty() => format!("{signed} {u}"),
                _ => signed,
            }
        }
        _ => "n/a".to_string(),
    }
}

/// Manual session markers (§17.4, ANA-05): a full-height vertical in each
/// marker's colour on every plot pane, with the label at the top. Read-only;
/// distinct from the amber playhead and the ANA-10 dashed delta cursor. The
/// line opacity, width and label visibility are user settings (PlotDisplay).
pub fn draw_session_markers(
    ui: &egui::Ui,
    view: PaneView,
    origin_us: i64,
    markers: &[crate::markers::Marker],
    opacity: f32,
    width: f32,
    show_label: bool,
) {
    let rect = view.rect;
    let (x0, x1) = view.x_range;
    if x1 <= x0 {
        return;
    }
    let painter = ui.painter();
    for m in markers {
        let t_sec = ((m.t_us - origin_us) as f64 * 1e-6) as f32;
        let frac = (t_sec - x0) / (x1 - x0);
        if !(0.0..=1.0).contains(&frac) {
            continue;
        }
        let x = rect.left() + frac * rect.width();
        let color = m.color32();
        painter.vline(
            x,
            rect.y_range(),
            egui::Stroke::new(width, color.gamma_multiply(opacity.clamp(0.0, 1.0))),
        );
        if !show_label {
            continue;
        }
        painter.text(
            egui::pos2(x + 3.0, rect.top() + 2.0),
            egui::Align2::LEFT_TOP,
            &m.label,
            egui::FontId::proportional(11.0),
            color,
        );
    }
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

#[cfg(test)]
mod tests {
    use super::format_delta;

    #[test]
    fn delta_applies_multiplier_and_signs_positive() {
        // (12.0 − 8.0) × 0.5 = +2.0, with the unit appended.
        assert_eq!(
            format_delta(Some(12.0), Some(8.0), 0.5, Some("m")),
            "+2.0000 m"
        );
    }

    #[test]
    fn delta_negative_keeps_minus_and_no_unit_when_blank() {
        assert_eq!(format_delta(Some(1.0), Some(4.0), 1.0, None), "-3.0000");
        assert_eq!(format_delta(Some(1.0), Some(4.0), 1.0, Some("")), "-3.0000");
    }

    #[test]
    fn delta_is_na_when_either_endpoint_missing_or_non_finite() {
        assert_eq!(format_delta(None, Some(1.0), 1.0, Some("m")), "n/a");
        assert_eq!(format_delta(Some(1.0), None, 1.0, Some("m")), "n/a");
        assert_eq!(
            format_delta(Some(f64::NAN), Some(1.0), 1.0, Some("m")),
            "n/a"
        );
        assert_eq!(
            format_delta(Some(1.0), Some(f64::INFINITY), 1.0, Some("m")),
            "n/a"
        );
    }
}
