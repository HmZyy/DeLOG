//! Plot legend: per-trace visibility, colour and width editing (PLAN.md §10.4,
//! PLT-08).
//!
//! An overlay in the plot's top-left listing each trace with a visibility
//! checkbox, a colour editor, its label, a width control and a remove button.
//! Returns the field removed this frame, if any, so the caller can unpin its
//! cache. (Plot/trace *mode* editing waits on the scatter/step pipelines,
//! GPU-07/08.)

use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::plot::PlotPane;

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
/// hidden trace's label is greyed out (PLT-08), and right-click ▸ Remove returns
/// the field (PLT-11) so the caller can drop its cache.
pub fn ui(
    ui: &egui::Ui,
    plot_rect: egui::Rect,
    pane: &mut PlotPane,
    labels: &[(FieldId, String)],
) -> Option<FieldId> {
    if labels.is_empty() {
        return None;
    }
    let mut removed = None;

    egui::Area::new(egui::Id::new("plot_legend"))
        .fixed_pos(plot_rect.left_top() + egui::vec2(8.0, 8.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
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
                        resp.context_menu(|ui| {
                            if ui.button("Remove").clicked() {
                                removed = Some(*field);
                                ui.close();
                            }
                        });
                    });
                }
            });
        });

    removed
}

fn color32_to_srgb(c: egui::Color32) -> [f32; 4] {
    [
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
        c.a() as f32 / 255.0,
    ]
}
