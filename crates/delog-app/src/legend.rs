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

/// Draw the legend overlay and apply edits to `pane`. Returns a field if its ×
/// button was clicked this frame.
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
                        ui.checkbox(&mut trace.visible, "");

                        let mut rgba = egui::Rgba::from_rgba_unmultiplied(
                            trace.color[0],
                            trace.color[1],
                            trace.color[2],
                            trace.color[3],
                        );
                        if egui::color_picker::color_edit_button_rgba(
                            ui,
                            &mut rgba,
                            egui::color_picker::Alpha::Opaque,
                        )
                        .changed()
                        {
                            trace.color = rgba.to_rgba_unmultiplied();
                        }

                        ui.label(label);
                        ui.add(
                            egui::DragValue::new(&mut trace.width_px)
                                .range(0.5..=6.0)
                                .speed(0.1)
                                .prefix("w "),
                        );
                        if ui.small_button("×").clicked() {
                            removed = Some(*field);
                        }
                    });
                }
            });
        });

    if let Some(field) = removed {
        pane.remove_trace(field);
    }
    removed
}
