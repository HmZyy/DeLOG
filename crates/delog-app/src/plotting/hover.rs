//! Hover readout: cursor line, per-trace circles and a value tooltip.

use std::collections::HashMap;

use delog_core::field_view::{FieldView, SampleMode};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::plotting::gpu::PaneView;
use crate::plotting::legend::trace_label;
use crate::plotting::plot::PlotPane;

const READOUT_ORDER: egui::Order = egui::Order::Background;

pub const PLAYHEAD_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 59, 59);
const PLAYHEAD_CASING: egui::Color32 = egui::Color32::from_black_alpha(110);
const MARKER_LABEL_GAP: f32 = 6.0;

pub struct HoverTarget {
    pub id: egui::Id,
    pub view: PaneView,
}

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
) -> Option<i64> {
    let pos = response.hover_pos()?;
    let view = target.view;
    let rect = view.rect;
    if !rect.contains(pos) {
        return None;
    }
    let (x0, x1) = view.x_range;
    let (y0, y1) = view.y_range;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }

    let painter = ui.painter();
    painter.vline(
        pos.x,
        rect.y_range(),
        egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
    );

    let (cursor_x_sec, cursor_us) = cursor_position(rect, (x0, x1), pos, origin_us)?;

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
    Some(cursor_us)
}

fn cursor_position(
    rect: egui::Rect,
    x_range: (f32, f32),
    pos: egui::Pos2,
    origin_us: i64,
) -> Option<(f32, i64)> {
    let (x0, x1) = x_range;
    if !rect.contains(pos) || x1 <= x0 || rect.width() <= 0.0 {
        return None;
    }
    let cursor_x_sec = x0 + (pos.x - rect.left()) / rect.width() * (x1 - x0);
    Some((cursor_x_sec, origin_us + (cursor_x_sec as f64 * 1e6) as i64))
}

fn draw_sample_circles(ui: &egui::Ui, view: PaneView, origin_us: i64, rows: &[Row]) {
    let rect = view.rect;
    let (x0, x1) = view.x_range;
    let (y0, y1) = view.y_range;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let painter = ui.painter();
    for row in rows {
        let x = (row.effective_time_us - origin_us) as f64 * 1e-6;
        let p = plot_to_screen(rect, (x0 as f64, x1 as f64), (y0, y1), x, row.value);
        if rect.contains(p) {
            painter.circle_stroke(p, 3.5, egui::Stroke::new(1.5, row.color));
        }
    }
}

/// Map a data point to screen in f64 against the absolute view range, matching
/// the (now rebased, precise) GPU line at large coordinate magnitudes.
fn plot_to_screen(rect: egui::Rect, x: (f64, f64), y: (f64, f64), px: f64, py: f64) -> egui::Pos2 {
    let fx = ((px - x.0) / (x.1 - x.0)) as f32;
    let fy = ((py - y.0) / (y.1 - y.0)) as f32;
    egui::pos2(
        rect.left() + fx * rect.width(),
        rect.bottom() - fy * rect.height(),
    )
}

struct Row {
    field: FieldId,
    label: String,
    value: f64,
    unit: Option<String>,
    color: egui::Color32,
    effective_time_us: i64,
}

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
        .order(READOUT_ORDER)
        .pivot(pivot)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            let base = egui::Frame::popup(ui.style());
            egui::Frame {
                shadow: egui::Shadow::NONE,
                fill: crate::plotting::legend::with_bg_opacity(base.fill, opacity),
                ..base
            }
            .show(ui, |ui| {
                crate::ui::components::dense_rows(ui);
                if show_time {
                    ui.label(egui::RichText::new(format!("{t_sec:.3} s")).weak());
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
    let color = PLAYHEAD_COLOR;
    painter.vline(x, rect.y_range(), egui::Stroke::new(4.0, PLAYHEAD_CASING));
    painter.vline(x, rect.y_range(), egui::Stroke::new(2.0, color));

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

    // Anchor at the top: the playhead readout anchors at the bottom, so the two never collide.
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

/// Per-trace ΔY (marker − playhead) for the legend. NaN is a gap, never
/// interpolated across, so it yields "n/a".
pub fn marker_deltas(
    snapshot: &StoreSnapshot,
    pane: &PlotPane,
    marker_us: i64,
    playhead_us: i64,
    mode: SampleMode,
) -> HashMap<FieldId, String> {
    let mut out = HashMap::new();
    for trace in &pane.traces {
        out.insert(
            trace.field,
            marker_delta_for_field(snapshot, trace.field, marker_us, playhead_us, mode)
                .unwrap_or_else(|| "n/a".to_owned()),
        );
    }
    out
}

pub(crate) fn marker_delta_for_field(
    snapshot: &StoreSnapshot,
    field: FieldId,
    marker_us: i64,
    playhead_us: i64,
    mode: SampleMode,
) -> Option<String> {
    let fv = FieldView::new(snapshot, field).ok()?;
    let at_marker = fv.sample_at(marker_us, mode).and_then(|s| s.value.as_f64());
    let at_playhead = fv
        .sample_at(playhead_us, mode)
        .and_then(|s| s.value.as_f64());
    let (mult, unit) = field_meta(snapshot, field);
    let delta = format_delta(at_marker, at_playhead, mult, unit.as_deref());
    (delta != "n/a").then_some(delta)
}

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

pub fn draw_marker_regions(
    ui: &egui::Ui,
    view: PaneView,
    origin_us: i64,
    markers: &[crate::plotting::markers::Marker],
    data_end_us: i64,
    opacity: f32,
) {
    let rect = view.rect;
    let (x0, x1) = view.x_range;
    if x1 <= x0 || markers.is_empty() {
        return;
    }
    let mut sorted: Vec<&crate::plotting::markers::Marker> = markers.iter().collect();
    sorted.sort_by_key(|m| m.t_us);
    let to_x = |t_us: i64| {
        let t_sec = ((t_us - origin_us) as f64 * 1e-6) as f32;
        rect.left() + (t_sec - x0) / (x1 - x0) * rect.width()
    };
    let opacity = opacity.clamp(0.0, 1.0);
    let painter = ui.painter();
    for (i, m) in sorted.iter().enumerate() {
        let start = to_x(m.t_us);
        // Last region ends at the log's final timestamp, not the pane edge.
        let end = sorted
            .get(i + 1)
            .map_or_else(|| to_x(data_end_us), |n| to_x(n.t_us));
        let a = start.clamp(rect.left(), rect.right());
        let b = end.clamp(rect.left(), rect.right());
        if b <= a {
            continue;
        }
        let fill = m.color32().gamma_multiply(opacity);
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(a, rect.top()), egui::pos2(b, rect.bottom())),
            0.0,
            fill,
        );
    }
}

pub fn draw_session_markers(
    ui: &egui::Ui,
    view: PaneView,
    origin_us: i64,
    markers: &[crate::plotting::markers::Marker],
    display: &crate::config::settings::PlotDisplay,
) {
    let rect = view.rect;
    let (x0, x1) = view.x_range;
    if x1 <= x0 {
        return;
    }
    let painter = ui.painter();
    let mut sorted: Vec<_> = markers.iter().collect();
    sorted.sort_by_key(|m| (m.t_us, m.id));
    let mut placed_labels: Vec<egui::Rect> = Vec::new();
    for m in sorted {
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
            egui::Stroke::new(
                display.marker_line_width,
                color.gamma_multiply(display.marker_line_opacity.clamp(0.0, 1.0)),
            ),
        );
        if !display.marker_show_label {
            continue;
        }
        let galley = painter.layout_no_wrap(
            m.label.clone(),
            egui::FontId::proportional(display.marker_label_font_size.clamp(4.0, 40.0)),
            color,
        );
        let mut label =
            egui::epaint::TextShape::new(egui::pos2(x + 3.0, rect.top() + 2.0), galley, color);
        if display.marker_label_orientation
            == crate::config::settings::MarkerLabelOrientation::Vertical
        {
            // Keep the rotated label right of the line at any font size.
            label.pos.x += label.galley.size().y;
            label = label.with_angle(std::f32::consts::FRAC_PI_2);
        }
        if display.marker_label_avoid_overlap {
            let mut bounds = label.visual_bounding_rect();
            if bounds.is_positive() {
                let original_top = bounds.top();
                // Scan occupied space from top to bottom, reusing gaps just as
                // string labels reuse rows. Rotated names can have different heights.
                for previous in &placed_labels {
                    if bounds
                        .intersect(previous.expand(MARKER_LABEL_GAP))
                        .is_positive()
                    {
                        bounds = bounds.translate(egui::vec2(
                            0.0,
                            previous.bottom() + MARKER_LABEL_GAP - bounds.top(),
                        ));
                    }
                }
                label.pos.y += bounds.top() - original_top;
                let slot = placed_labels.partition_point(|r| r.top() <= bounds.top());
                placed_labels.insert(slot, bounds);
            }
        }
        painter.with_clip_rect(rect).add(label);
    }
}

/// The visible-trace sample time nearest `t_us` across `pane` (snap).
pub fn nearest_sample_us(snapshot: &StoreSnapshot, pane: &PlotPane, t_us: i64) -> Option<i64> {
    let mut best: Option<i64> = None;
    for trace in pane.visible_traces() {
        let Ok(fv) = FieldView::new(snapshot, trace.field) else {
            continue;
        };
        for mode in [SampleMode::Prev, SampleMode::Next] {
            if let Some(sample) = fv.sample_at(t_us, mode) {
                let cand = sample.effective_time_us;
                if best.is_none_or(|b| (cand - t_us).abs() < (b - t_us).abs()) {
                    best = Some(cand);
                }
            }
        }
    }
    best
}

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
    use super::{Row, format_delta, plot_to_screen, show_tooltip};
    use std::collections::HashMap;

    fn marker_label_bounds(
        markers: &crate::plotting::markers::Markers,
        orientation: &str,
        avoid_overlap: bool,
        font_size: f32,
    ) -> HashMap<String, egui::Rect> {
        let display = serde_json::from_value(serde_json::json!({
            "marker_label_orientation": orientation,
            "marker_label_font_size": font_size,
            "marker_label_avoid_overlap": avoid_overlap,
        }))
        .unwrap();
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            super::draw_session_markers(
                ui,
                super::PaneView {
                    rect: egui::Rect::from_min_size(
                        egui::pos2(50.0, 50.0),
                        egui::vec2(400.0, 400.0),
                    ),
                    x_range: (0.0, 1.0),
                    y_range: (0.0, 1.0),
                },
                0,
                markers.as_slice(),
                &display,
            );
        });
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => {
                    Some((text.galley.job.text.clone(), text.visual_bounding_rect()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn marker_label_stacking_preserves_time_order_and_first_position() {
        let mut markers = crate::plotting::markers::Markers::new();
        // Insert out of time order, including an offscreen predecessor.
        markers.add_at(201_000);
        markers.add_at(200_000);
        markers.add_at(-10_000);
        markers.add_at(202_000);
        markers.add_at(900_000);
        for orientation in ["horizontal", "vertical"] {
            for font_size in [11.0, 24.0] {
                let original = marker_label_bounds(&markers, orientation, false, font_size);
                let stacked = marker_label_bounds(&markers, orientation, true, font_size);
                assert_eq!(stacked.len(), 4);
                assert_eq!(stacked["Marker 2"], original["Marker 2"]);
                assert_eq!(stacked["Marker 5"], original["Marker 5"]);
                assert!(original["Marker 2"].intersects(original["Marker 1"]));
                assert!(stacked["Marker 1"].top() > stacked["Marker 2"].bottom());
                assert!(stacked["Marker 4"].top() > stacked["Marker 1"].bottom());
                for name in ["Marker 1", "Marker 2", "Marker 4", "Marker 5"] {
                    assert_eq!(stacked[name].left(), original[name].left());
                    assert_eq!(stacked[name].right(), original[name].right());
                }
            }
        }
    }

    #[test]
    fn marker_label_stacking_keeps_creation_order_for_equal_timestamps() {
        let mut markers = crate::plotting::markers::Markers::new();
        for _ in 0..3 {
            markers.add_at(500_000);
        }
        for orientation in ["horizontal", "vertical"] {
            let original = marker_label_bounds(&markers, orientation, false, 11.0);
            let stacked = marker_label_bounds(&markers, orientation, true, 11.0);
            assert_eq!(stacked["Marker 1"], original["Marker 1"]);
            assert!(stacked["Marker 2"].top() > stacked["Marker 1"].bottom());
            assert!(stacked["Marker 3"].top() > stacked["Marker 2"].bottom());
        }
    }

    #[test]
    fn marker_label_stacking_reuses_space_above_a_shifted_label() {
        let mut markers = crate::plotting::markers::Markers::new();
        markers.push_loaded(100_000, "First".into(), [1.0; 4], String::new());
        markers.push_loaded(
            101_000,
            "A much longer second marker name".into(),
            [1.0; 4],
            String::new(),
        );
        markers.push_loaded(300_000, "Third".into(), [1.0; 4], String::new());
        markers.push_loaded(301_000, "Fourth".into(), [1.0; 4], String::new());
        let original = marker_label_bounds(&markers, "horizontal", false, 11.0);
        let stacked = marker_label_bounds(&markers, "horizontal", true, 11.0);
        assert_eq!(stacked["Third"], original["Third"]);
        let second = stacked["A much longer second marker name"];
        assert!(stacked["Fourth"].top() > second.bottom());
        let bounds: Vec<_> = stacked.values().collect();
        for (index, label) in bounds.iter().enumerate() {
            for other in &bounds[index + 1..] {
                assert!(!label.intersects(**other));
            }
        }
    }

    #[test]
    fn marker_labels_render_with_saved_orientation_and_font_size() {
        let rect = egui::Rect::from_min_size(egui::pos2(50.0, 50.0), egui::vec2(400.0, 400.0));
        let mut markers = crate::plotting::markers::Markers::new();
        markers.add_at(500_000);
        for (orientation, font_size, visible) in [
            ("horizontal", 11.0, true),
            ("horizontal", 24.0, true),
            ("vertical", 11.0, true),
            ("vertical", 24.0, true),
            ("vertical", 24.0, false),
        ] {
            let display = serde_json::from_value(serde_json::json!({
                "marker_label_orientation": orientation,
                "marker_label_font_size": font_size,
                "marker_show_label": visible,
            }))
            .unwrap();
            let ctx = egui::Context::default();
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                super::draw_session_markers(
                    ui,
                    super::PaneView {
                        rect,
                        x_range: (0.0, 1.0),
                        y_range: (0.0, 1.0),
                    },
                    0,
                    markers.as_slice(),
                    &display,
                );
            });
            let labels: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) if text.galley.job.text == "Marker 1" => {
                        Some((clipped.clip_rect, text))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(labels.len(), usize::from(visible));
            if !visible {
                continue;
            }
            let (clip_rect, label) = labels[0];
            let bounds = label.visual_bounding_rect();
            assert_eq!(
                bounds.height() > bounds.width(),
                orientation == "vertical",
                "marker label should render {orientation}, got {bounds:?}"
            );
            assert_eq!(label.galley.job.sections[0].format.font_id.size, font_size);
            assert!(bounds.left() >= 253.0, "label must stay right of its line");
            assert!(bounds.top() >= 52.0, "label must stay below the plot top");
            assert_eq!(clip_rect, rect);
        }
    }

    fn tooltip_row(field: u32, label: &str, value: f64) -> Row {
        Row {
            field: delog_core::identity::FieldId(field),
            label: label.to_owned(),
            value,
            unit: Some("m/s".to_owned()),
            color: egui::Color32::RED,
            effective_time_us: 0,
        }
    }

    #[test]
    fn tooltip_time_is_shown_without_a_t_prefix() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let rows = vec![tooltip_row(1, "alpha", 1.0)];
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let render = || {
            ctx.run_ui(input.clone(), |ui| {
                show_tooltip(
                    ui,
                    egui::Id::new("tooltip-time"),
                    egui::pos2(100.0, 100.0),
                    egui::Align2::LEFT_TOP,
                    1.25,
                    &rows,
                    &HashMap::new(),
                    true,
                    true,
                    1.0,
                );
            })
        };
        render();
        let output = render();

        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(text) => out.push(text.galley.job.text.clone()),
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }

        assert!(
            texts.iter().any(|text| text == "1.250 s"),
            "the readout should show the bare timestamp, got {texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("t =")),
            "the readout should not prefix the timestamp, got {texts:?}"
        );
    }

    #[test]
    fn tooltip_rows_use_the_dense_row_gap() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let tokens = crate::ui::design_tokens::DesignTokens::default();
        let rows = vec![
            tooltip_row(1, "alpha", 1.0),
            tooltip_row(2, "beta", 2.0),
            tooltip_row(3, "gamma", 3.0),
        ];
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };

        let render = || {
            ctx.run_ui(input.clone(), |ui| {
                show_tooltip(
                    ui,
                    egui::Id::new("tooltip-density"),
                    egui::pos2(100.0, 100.0),
                    egui::Align2::LEFT_TOP,
                    1.0,
                    &rows,
                    &HashMap::new(),
                    true,
                    false,
                    1.0,
                );
            })
        };
        render();
        let output = render();

        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    out.push((text.galley.job.text.clone(), text.visual_bounding_rect()));
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }
        let row_top = |label: &str| {
            texts
                .iter()
                .find(|(text, _)| text.starts_with(label))
                .unwrap_or_else(|| panic!("{label} row should be painted, got {texts:?}"))
                .1
                .top()
        };

        let first = row_top("alpha");
        let second = row_top("beta");
        let third = row_top("gamma");
        let pitch = second - first;

        assert!(
            (third - second - pitch).abs() < 0.5,
            "tooltip rows should be evenly spaced ({first}, {second}, {third})"
        );
        assert!(
            pitch <= tokens.dense_row_height + tokens.dense_row_gap + 0.5,
            "tooltip rows are {pitch} apart, denser layout expects at most {}",
            tokens.dense_row_height + tokens.dense_row_gap
        );
    }

    #[test]
    fn sample_circle_maps_in_f64_at_large_magnitude() {
        // Latitude-scale value at the true midpoint of the view maps to the
        // middle of the rect; f64 keeps it exact where f32 would collapse.
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 500.0));
        let p = plot_to_screen(
            rect,
            (0.0, 1.0),
            (437_129_280.0, 437_129_380.0),
            0.5,
            437_129_330.0,
        );
        let frac = (rect.bottom() - p.y) / rect.height();
        assert!((frac - 0.5).abs() < 1e-4, "frac was {frac}");
    }

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
