//! Text-annotation traces (PLT-15, §10): string fields drawn as text labels at
//! each sample's timestamp, overlaid in screen space over the pane. Auto-packed
//! top-down so labels don't collide; each is draggable vertically (x is locked
//! to its time) to declutter, with manual positions persisted per pane.

use std::collections::HashMap;

use delog_core::field_view::FieldView;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;
use delog_core::time::TimeRange;

use crate::gpu::PaneView;
use crate::plot::TraceRef;

const FONT_SIZE: f32 = 11.0;
/// Horizontal gap (px) required between two labels sharing a row.
const ROW_GAP: f32 = 6.0;

/// Layout style for text-annotation labels (PLT-15), from `PlotDisplay`.
pub struct TextLabelStyle {
    /// Max labels per string trace in the visible window.
    pub cap: usize,
    /// Stack rows from the bottom up (vs top down).
    pub bottom_up: bool,
    /// Vertical spacing between stacked rows, in px.
    pub spacing_px: f32,
}

/// Whether `field`'s dtype is a string (text-annotation trace, PLT-15).
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

/// Draw every visible string trace as text annotations and apply vertical
/// drags. `offsets` carries per-label manual y-fractions; only dragged labels
/// are stored.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    ui: &egui::Ui,
    response: &egui::Response,
    view: PaneView,
    origin_us: i64,
    snapshot: &StoreSnapshot,
    traces: &[TraceRef],
    offsets: &mut HashMap<(FieldId, i64), f32>,
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

    // Prune offsets whose field is no longer a string trace in this pane.
    offsets.retain(|(field, _), _| {
        traces
            .iter()
            .any(|t| t.field == *field && field_is_string(snapshot, *field))
    });

    // Label dragging is handled through the pane's own response (not per-label
    // interact widgets) so horizontal drags still pan and the wheel still zooms
    // the pane — labels only take the *vertical* component (PLT-15). The grabbed
    // label persists across frames in egui temp memory.
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
        let mut samples = fv.string_samples_in_range(range, style.cap.saturating_add(1));
        let truncated = samples.len() > style.cap;
        samples.truncate(style.cap);
        samples.sort_by_key(|(t, _)| *t);

        let color = trace.color32();
        // Right edge x of the last label placed in each auto-packed row.
        let mut row_right: Vec<f32> = Vec::new();
        for (t_us, text) in samples {
            let x = to_x(t_us);
            let galley = ui
                .painter()
                .layout_no_wrap(text.clone(), font.clone(), color);
            let width = galley.size().x;

            // Auto y: first row whose last label ends left of this x (+ gap).
            let row = row_right
                .iter()
                .position(|&right| x >= right + ROW_GAP)
                .unwrap_or_else(|| {
                    row_right.push(0.0);
                    row_right.len() - 1
                });
            row_right[row] = x + width;
            // Row 0 nearest the chosen edge; later rows stack toward the centre.
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

            // Faint full-height line marking the exact timestamp.
            ui.painter().vline(
                x,
                rect.y_range(),
                egui::Stroke::new(1.0, color.gamma_multiply(0.3)),
            );
            let text_pos = egui::pos2(x + 3.0, y);
            ui.painter().galley(text_pos, galley, color);

            // Grab this label if a drag began on it; then track its vertical
            // movement. The pane response still pans (x) / zooms in parallel —
            // labels are x-locked and only consume the vertical delta.
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
    /// Greedy top-down row packing: each item takes the first row whose last
    /// label ends left of its x (plus a gap), else a new row. Items must be
    /// sorted by x. Mirrors the layout used in `draw`.
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
        // Well-separated labels all fit on the top row.
        let rows = pack_rows(&[(0.0, 0.5), (1.0, 0.5), (2.0, 0.5)], 0.1);
        assert_eq!(rows, [0, 0, 0]);
    }

    #[test]
    fn overlapping_labels_drop_to_next_rows() {
        // Wide labels close in x collide and stack downward.
        let rows = pack_rows(&[(0.0, 1.0), (0.2, 1.0), (0.4, 1.0)], 0.1);
        assert_eq!(rows, [0, 1, 2]);
    }

    #[test]
    fn freed_row_is_reused_top_down() {
        // Third label clears the first row again (x past its right edge + gap).
        let rows = pack_rows(&[(0.0, 1.0), (0.2, 0.5), (1.5, 0.2)], 0.1);
        assert_eq!(rows, [0, 1, 0]);
    }
}
