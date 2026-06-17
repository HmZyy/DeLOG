//! Plot legend: per-trace visibility, colour and width editing (PLAN.md §10.4,
//! PLT-08).
//!
//! An overlay in the plot's top-left listing each trace with a colour editor
//! and clickable label. Right-clicking a trace opens style controls for draw
//! mode, width and removal.

use std::collections::HashMap;

use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::plot::{PlotPane, TraceMode};
use crate::settings::LegendPosition;

/// Scale a background colour's alpha by `opacity` (1 = unchanged, 0 = fully
/// transparent), keeping its RGB. Shared look for the legend and hover panels.
pub fn with_bg_opacity(color: egui::Color32, opacity: f32) -> egui::Color32 {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    let a = (a as f32 * opacity.clamp(0.0, 1.0)).round() as u8;
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Anchor point + pivot for the legend area at `position` inside `plot_rect`,
/// inset by 8 px from the chosen corner so it never touches the axes.
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

/// `topic.field` label for a trace, resolved through core (no Arrow in the app).
pub fn trace_label(snapshot: &StoreSnapshot, field: FieldId) -> String {
    let Some(entry) = snapshot.fields.get(field.index()).filter(|f| f.id == field) else {
        return format!("field {}", field.0);
    };
    match snapshot.topic(entry.topic) {
        Some(topic) => format!("{}.{}", topic.entry.name, entry.name),
        None => entry.name.clone(),
    }
}

/// Draw the legend overlay and apply edits to `pane`. Each row is a colour
/// editor plus a clickable label: clicking toggles the trace's visibility, a
/// hidden trace's label is greyed out (PLT-08), and right-click / Remove returns
/// the field (PLT-11) so the caller can drop its cache. When a measurement
/// marker is placed, `deltas` carries the per-trace ΔY string shown after the
/// label (§10.8, ANA-10); an empty map shows none.
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
) -> Option<FieldId> {
    if labels.is_empty() && pane.ghosts.is_empty() {
        return None;
    }
    let mut removed = None;

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
                    let Some(trace) = pane.trace_mut(*field) else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        // sRGB colour editor (matches the rendered trace).
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

                        // Clickable label; greyed when the trace is hidden.
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

                        // Per-trace ΔY against the marker, weak so it reads as a
                        // secondary annotation next to the trace name (ANA-10).
                        if let Some(delta) = deltas.get(field) {
                            ui.label(
                                egui::RichText::new(format!("Δ {delta}"))
                                    .color(ui.visuals().hyperlink_color)
                                    .weak(),
                            );
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
