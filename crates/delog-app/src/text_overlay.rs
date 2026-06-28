use std::collections::HashMap;

use delog_core::field_view::FieldView;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;
use delog_core::time::TimeRange;

use crate::gpu::PaneView;
use crate::plot::TraceRef;

const FONT_SIZE: f32 = 11.0;
const ROW_GAP: f32 = 6.0;

pub struct TextLabelStyle {
    pub cap: usize,
    pub bottom_up: bool,
    pub spacing_px: f32,
    pub line_width: f32,
    pub line_opacity: f32,
}

pub fn field_is_string(snapshot: &StoreSnapshot, field: FieldId) -> bool {
    let Some(entry) = snapshot.fields.get(field.index()).filter(|f| f.id == field) else {
        return false;
    };
    let Some(store) = snapshot.topic(entry.topic).and_then(|t| t.store.as_ref()) else {
        return false;
    };
    store
        .schema
        .field_by_name(&entry.name)
        .is_some_and(|fs| fs.is_string())
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    ui: &egui::Ui,
    response: &egui::Response,
    view: PaneView,
    origin_us: i64,
    snapshot: &StoreSnapshot,
    traces: &[TraceRef],
    offsets: &mut HashMap<(FieldId, i64), f32>,
    filters: &HashMap<FieldId, String>,
    style: TextLabelStyle,
) {
    let rect = view.rect;
    let (x0, x1) = view.x_range;
    if x1 <= x0 || rect.height() <= 0.0 {
        return;
    }
    let Some(range) = TimeRange::new(
        origin_us + (x0 as f64 * 1e6) as i64,
        origin_us + (x1 as f64 * 1e6) as i64,
    ) else {
        return;
    };
    let to_x = |t_us: i64| {
        let t_sec = ((t_us - origin_us) as f64 * 1e-6) as f32;
        rect.left() + (t_sec - x0) / (x1 - x0) * rect.width()
    };
    let font = egui::FontId::proportional(FONT_SIZE);
    let text_h = ui.text_style_height(&egui::TextStyle::Small).max(FONT_SIZE);
    let row_frac = (text_h + style.spacing_px.max(0.0)) / rect.height();
    let margin_frac = 4.0 / rect.height();

    offsets.retain(|(field, _), _| {
        traces
            .iter()
            .any(|t| t.field == *field && field_is_string(snapshot, *field))
    });

    // Drag via the pane response (not per-label widgets) so horizontal drags
    // still pan and the wheel still zooms; labels consume only the vertical delta.
    let drag_key = ui.id().with("text_label_drag");
    let mut grabbed: Option<(FieldId, i64)> = ui
        .memory_mut(|m| m.data.get_temp::<Option<(FieldId, i64)>>(drag_key))
        .flatten();
    let drag_started = response.drag_started_by(egui::PointerButton::Primary);
    let dragging = response.dragged_by(egui::PointerButton::Primary);
    let press_pos = response.interact_pointer_pos();

    for trace in traces.iter().filter(|t| t.visible) {
        if !field_is_string(snapshot, trace.field) {
            continue;
        }
        let Ok(fv) = FieldView::new(snapshot, trace.field) else {
            continue;
        };
        let filter = filters.get(&trace.field).map(String::as_str);
        let mut samples = fv.string_samples_in_range(range, style.cap.saturating_add(1), filter);
        let truncated = samples.len() > style.cap;
        samples.truncate(style.cap);
        samples.sort_by_key(|(t, _)| *t);

        let color = trace.color32();
        let mut row_right: Vec<f32> = Vec::new();
        for (t_us, text) in samples {
            let x = to_x(t_us);
            let galley = ui
                .painter()
                .layout_no_wrap(text.clone(), font.clone(), color);
            let width = galley.size().x;

            let row = row_right
                .iter()
                .position(|&right| x >= right + ROW_GAP)
                .unwrap_or_else(|| {
                    row_right.push(0.0);
                    row_right.len() - 1
                });
            row_right[row] = x + width;
            let auto_frac = if style.bottom_up {
                1.0 - margin_frac - (row as f32 + 1.0) * row_frac
            } else {
                margin_frac + row as f32 * row_frac
            };

            let y_frac = offsets
                .get(&(trace.field, t_us))
                .copied()
                .unwrap_or(auto_frac)
                .clamp(0.0, 1.0);
            let y = rect.top() + y_frac * rect.height();

            ui.painter().vline(
                x,
                rect.y_range(),
                egui::Stroke::new(
                    style.line_width,
                    color.gamma_multiply(style.line_opacity.clamp(0.0, 1.0)),
                ),
            );
            let text_pos = egui::pos2(x + 3.0, y);
            ui.painter().galley(text_pos, galley, color);

            let label_rect =
                egui::Rect::from_min_size(text_pos, egui::vec2(width, row_frac * rect.height()));
            if drag_started
                && grabbed.is_none()
                && press_pos.is_some_and(|p| label_rect.contains(p))
            {
                grabbed = Some((trace.field, t_us));
                ui.memory_mut(|m| m.data.insert_temp(drag_key, grabbed));
            }
            if dragging && grabbed == Some((trace.field, t_us)) {
                let new = (y_frac + response.drag_delta().y / rect.height()).clamp(0.0, 1.0);
                offsets.insert((trace.field, t_us), new);
            }
        }

        if truncated {
            ui.painter().text(
                rect.right_top() + egui::vec2(-4.0, 4.0),
                egui::Align2::RIGHT_TOP,
                format!("+ more (showing {})", style.cap),
                font.clone(),
                ui.visuals().weak_text_color(),
            );
        }
    }

    if response.drag_stopped() {
        ui.memory_mut(|m| m.data.insert_temp::<Option<(FieldId, i64)>>(drag_key, None));
    }
}

#[cfg(test)]
mod tests {
    fn pack_rows(items: &[(f32, f32)], gap: f32) -> Vec<usize> {
        let mut row_right: Vec<f32> = Vec::new();
        let mut out = Vec::with_capacity(items.len());
        for &(x, w) in items {
            let row = row_right
                .iter()
                .position(|&right| x >= right + gap)
                .unwrap_or_else(|| {
                    row_right.push(0.0);
                    row_right.len() - 1
                });
            row_right[row] = x + w;
            out.push(row);
        }
        out
    }

    #[test]
    fn non_overlapping_labels_share_row_zero() {
        let rows = pack_rows(&[(0.0, 0.5), (1.0, 0.5), (2.0, 0.5)], 0.1);
        assert_eq!(rows, [0, 0, 0]);
    }

    #[test]
    fn overlapping_labels_drop_to_next_rows() {
        let rows = pack_rows(&[(0.0, 1.0), (0.2, 1.0), (0.4, 1.0)], 0.1);
        assert_eq!(rows, [0, 1, 2]);
    }

    #[test]
    fn freed_row_is_reused_top_down() {
        let rows = pack_rows(&[(0.0, 1.0), (0.2, 0.5), (1.5, 0.2)], 0.1);
        assert_eq!(rows, [0, 1, 0]);
    }
}
