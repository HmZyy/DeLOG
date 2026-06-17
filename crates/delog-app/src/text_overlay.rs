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

/// Cap on labels drawn per string trace in the visible window — keeps dense
/// fields from flooding the pane (and the per-frame cost bounded).
const MAX_LABELS: usize = 256;
const FONT_SIZE: f32 = 11.0;
/// Horizontal gap (px) required between two labels sharing a row.
const ROW_GAP: f32 = 6.0;

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
pub fn draw(
    ui: &egui::Ui,
    view: PaneView,
    origin_us: i64,
    snapshot: &StoreSnapshot,
    traces: &[TraceRef],
    offsets: &mut HashMap<(FieldId, i64), f32>,
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
    let row_frac =
        (ui.text_style_height(&egui::TextStyle::Small).max(FONT_SIZE) + 2.0) / rect.height();
    let top_frac = 4.0 / rect.height();

    // Prune offsets whose field is no longer a string trace in this pane.
    offsets.retain(|(field, _), _| {
        traces
            .iter()
            .any(|t| t.field == *field && field_is_string(snapshot, *field))
    });

    for trace in traces.iter().filter(|t| t.visible) {
        if !field_is_string(snapshot, trace.field) {
            continue;
        }
        let Ok(fv) = FieldView::new(snapshot, trace.field) else {
            continue;
        };
        let mut samples = fv.string_samples_in_range(range, MAX_LABELS + 1);
        let truncated = samples.len() > MAX_LABELS;
        samples.truncate(MAX_LABELS);
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
            let auto_frac = top_frac + row as f32 * row_frac;

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

            // Vertical drag to reposition (x stays locked to the timestamp).
            let label_rect =
                egui::Rect::from_min_size(text_pos, egui::vec2(width, row_frac * rect.height()));
            let resp = ui.interact(
                label_rect,
                ui.id().with(("text_label", trace.field.0, t_us)),
                egui::Sense::drag(),
            );
            if resp.dragged() {
                let new = (y_frac + resp.drag_delta().y / rect.height()).clamp(0.0, 1.0);
                offsets.insert((trace.field, t_us), new);
            }
        }

        if truncated {
            ui.painter().text(
                rect.right_top() + egui::vec2(-4.0, 4.0),
                egui::Align2::RIGHT_TOP,
                format!("+ more (showing {MAX_LABELS})"),
                font.clone(),
                ui.visuals().weak_text_color(),
            );
        }
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
