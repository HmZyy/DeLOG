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

/// Inset from the corner so the legend never touches the axes.
fn legend_anchor(position: LegendPosition, plot_rect: egui::Rect) -> (egui::Pos2, egui::Align2) {
    const INSET: f32 = 8.0;
    match position {
        LegendPosition::TopLeft => (
            plot_rect.left_top() + egui::vec2(INSET, INSET),
            egui::Align2::LEFT_TOP,
        ),
        LegendPosition::TopRight => (
            plot_rect.right_top() + egui::vec2(-INSET, INSET),
            egui::Align2::RIGHT_TOP,
        ),
        LegendPosition::BottomLeft => (
            plot_rect.left_bottom() + egui::vec2(INSET, -INSET),
            egui::Align2::LEFT_BOTTOM,
        ),
        LegendPosition::BottomRight => (
            plot_rect.right_bottom() + egui::vec2(-INSET, -INSET),
            egui::Align2::RIGHT_BOTTOM,
        ),
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

    let (pos, pivot) = legend_anchor(position, plot_rect);
    egui::Area::new(id)
        .fixed_pos(pos)
        .pivot(pivot)
        .order(egui::Order::Middle)
        .show(ui.ctx(), |ui| {
            let base = egui::Frame::popup(ui.style());
            egui::Frame {
                shadow: egui::Shadow::NONE,
                fill: with_bg_opacity(base.fill, opacity),
                ..base
            }
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
