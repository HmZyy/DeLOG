use std::collections::HashMap;

use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::plot::{PlotPane, TraceMode};
use crate::settings::LegendPosition;

pub fn with_bg_opacity(color: egui::Color32, opacity: f32) -> egui::Color32 {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    let a = (a as f32 * opacity.clamp(0.0, 1.0)).round() as u8;
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Inset from the plot edge so the legend never touches the axes.
const LEGEND_INSET: f32 = 8.0;

/// Minimum positive dimension passed to egui sizing APIs for degenerate plots.
const MIN_LEGEND_CONTENT_EXTENT: f32 = 1.0;

fn legend_bounds(plot_rect: egui::Rect) -> egui::Rect {
    let inset = egui::vec2(
        LEGEND_INSET.min((plot_rect.width() * 0.5).max(0.0)),
        LEGEND_INSET.min((plot_rect.height() * 0.5).max(0.0)),
    );
    plot_rect.shrink2(inset)
}

fn legend_content_max_size(bounds: egui::Rect, frame: &egui::Frame) -> egui::Vec2 {
    let frame_margin = frame.total_margin().sum();
    egui::vec2(
        (bounds.width() - frame_margin.x).max(MIN_LEGEND_CONTENT_EXTENT),
        (bounds.height() - frame_margin.y).max(MIN_LEGEND_CONTENT_EXTENT),
    )
}

fn legend_anchor(position: LegendPosition, bounds: egui::Rect) -> (egui::Pos2, egui::Align2) {
    match position {
        LegendPosition::TopLeft => (bounds.left_top(), egui::Align2::LEFT_TOP),
        LegendPosition::TopRight => (bounds.right_top(), egui::Align2::RIGHT_TOP),
        LegendPosition::BottomLeft => (bounds.left_bottom(), egui::Align2::LEFT_BOTTOM),
        LegendPosition::BottomRight => (bounds.right_bottom(), egui::Align2::RIGHT_BOTTOM),
    }
}

pub fn trace_label(snapshot: &StoreSnapshot, field: FieldId) -> String {
    let Some(entry) = snapshot.fields.get(field.index()).filter(|f| f.id == field) else {
        return format!("field {}", field.0);
    };
    match snapshot.topic(entry.topic) {
        Some(topic) => format!("{}.{}", topic.entry.name, entry.name),
        None => entry.name.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn ui(
    ui: &egui::Ui,
    id: egui::Id,
    plot_rect: egui::Rect,
    position: LegendPosition,
    opacity: f32,
    pane: &mut PlotPane,
    labels: &[(FieldId, String)],
    deltas: &HashMap<FieldId, String>,
    snapshot: &StoreSnapshot,
) -> Option<FieldId> {
    if labels.is_empty() && pane.ghosts.is_empty() {
        return None;
    }
    let mut removed = None;
    // Applied after the Area closure releases its borrow of `pane`.
    let mut filter_edits: Vec<(FieldId, String)> = Vec::new();

    let bounds = legend_bounds(plot_rect);
    let (pos, pivot) = legend_anchor(position, bounds);
    egui::Area::new(id)
        .fixed_pos(pos)
        .pivot(pivot)
        .order(egui::Order::Middle)
        .show(ui.ctx(), |ui| {
            let base = egui::Frame::popup(ui.style());
            let frame = egui::Frame {
                shadow: egui::Shadow::NONE,
                fill: with_bg_opacity(base.fill, opacity),
                ..base
            };
            let content_max_size = legend_content_max_size(bounds, &frame);
            ui.set_max_size(bounds.size());
            frame.show(ui, |ui| {
                ui.set_max_size(content_max_size);
                egui::ScrollArea::vertical()
                    .max_width(content_max_size.x)
                    .max_height(content_max_size.y)
                    .show(ui, |ui| {
                        for (field, label) in labels {
                            let is_text = crate::text_overlay::field_is_string(snapshot, *field);
                            let mut filter = if is_text {
                                pane.text_filters.get(field).cloned().unwrap_or_default()
                            } else {
                                String::new()
                            };
                            let Some(trace) = pane.trace_mut(*field) else {
                                continue;
                            };
                            ui.horizontal(|ui| {
                                let mut color = trace.color32();
                                if egui::color_picker::color_edit_button_srgba(
                                    ui,
                                    &mut color,
                                    egui::color_picker::Alpha::Opaque,
                                )
                                .changed()
                                {
                                    trace.color = color32_to_srgb(color);
                                }

                                let text_color = if trace.visible {
                                    ui.visuals().text_color()
                                } else {
                                    ui.visuals().weak_text_color()
                                };
                                let label_widget =
                                    egui::Label::new(egui::RichText::new(label).color(text_color))
                                        .truncate()
                                        .sense(egui::Sense::click());
                                let resp = ui.add(label_widget);
                                if resp.clicked() {
                                    trace.visible = !trace.visible;
                                }

                                if let Some(delta) = deltas.get(field) {
                                    ui.label(
                                        egui::RichText::new(format!("d {delta}"))
                                            .color(ui.visuals().hyperlink_color)
                                            .weak(),
                                    );
                                }

                                if is_text
                                    && ui
                                        .add(
                                            egui::TextEdit::singleline(&mut filter)
                                                .hint_text("filter…")
                                                .desired_width(90.0),
                                        )
                                        .on_hover_text("Show only messages containing this text")
                                        .changed()
                                {
                                    filter_edits.push((*field, filter.clone()));
                                }
                                resp.context_menu(|ui| {
                                    ui.menu_button("Mode", |ui| {
                                        for mode in TraceMode::ALL {
                                            ui.radio_value(&mut trace.mode, mode, mode.label());
                                        }
                                    });
                                    ui.add(
                                        egui::Slider::new(&mut trace.width_px, 1.0..=12.0)
                                            .text("Width")
                                            .suffix(" px"),
                                    );
                                    ui.separator();
                                    if ui.button("Remove").clicked() {
                                        removed = Some(*field);
                                        ui.close();
                                    }
                                });
                            });
                        }
                        for ghost in &pane.ghosts {
                            ui.horizontal(|ui| {
                                let mut color = ghost_color(ghost.color);
                                let _ = egui::color_picker::color_edit_button_srgba(
                                    ui,
                                    &mut color,
                                    egui::color_picker::Alpha::Opaque,
                                );
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{}.{} (missing)",
                                        ghost.topic, ghost.field
                                    ))
                                    .color(ui.visuals().weak_text_color()),
                                );
                            });
                        }
                    });
            })
        });

    for (field, filter) in filter_edits {
        if filter.trim().is_empty() {
            pane.text_filters.remove(&field);
        } else {
            pane.text_filters.insert(field, filter);
        }
    }

    removed
}

fn ghost_color(color: [f32; 4]) -> egui::Color32 {
    let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(u(color[0]), u(color[1]), u(color[2]), u(color[3]))
        .gamma_multiply(0.45)
}

pub fn color32_to_srgb(c: egui::Color32) -> [f32; 4] {
    [
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
        c.a() as f32 / 255.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f32, top: f32, right: f32, bottom: f32) -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom))
    }

    #[test]
    fn legend_bounds_are_inset_inside_plot_rect() {
        let plot = rect(10.0, 20.0, 210.0, 120.0);

        let bounds = legend_bounds(plot);

        assert!(plot.contains_rect(bounds));
        assert_eq!(bounds.left(), 18.0);
        assert_eq!(bounds.top(), 28.0);
        assert_eq!(bounds.right(), 202.0);
        assert_eq!(bounds.bottom(), 112.0);
    }

    #[test]
    fn tiny_legend_bounds_produce_positive_content_size() {
        let plot = rect(0.0, 0.0, 6.0, 4.0);
        let bounds = legend_bounds(plot);
        let frame = egui::Frame::default().inner_margin(8);

        let content_size = legend_content_max_size(bounds, &frame);

        assert!(plot.contains_rect(bounds));
        assert!(content_size.x > 0.0);
        assert!(content_size.y > 0.0);
    }

    #[test]
    fn legend_anchor_uses_bounded_rect_for_every_position() {
        let plot = rect(10.0, 20.0, 210.0, 120.0);
        let bounds = legend_bounds(plot);

        let cases = [
            (
                LegendPosition::TopLeft,
                bounds.left_top(),
                egui::Align2::LEFT_TOP,
            ),
            (
                LegendPosition::TopRight,
                bounds.right_top(),
                egui::Align2::RIGHT_TOP,
            ),
            (
                LegendPosition::BottomLeft,
                bounds.left_bottom(),
                egui::Align2::LEFT_BOTTOM,
            ),
            (
                LegendPosition::BottomRight,
                bounds.right_bottom(),
                egui::Align2::RIGHT_BOTTOM,
            ),
        ];

        for (position, expected_pos, expected_pivot) in cases {
            let (pos, pivot) = legend_anchor(position, bounds);
            assert_eq!(pos, expected_pos);
            assert_eq!(pivot, expected_pivot);
        }
    }
}
