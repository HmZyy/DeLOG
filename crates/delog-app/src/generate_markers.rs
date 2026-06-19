//! Generate session markers from a field's distinct values (ANA-11, §17.4).
//!
//! A discrete field's value transitions (from [`field_value_transitions`]) are
//! listed in a popup; each value gets an include checkbox, an editable name
//! (default `Value <v>`) and a stable colour. Generating appends an ANA-05
//! marker at every transition into each included value.

use delog_core::analysis::{TransitionsError, field_value_transitions};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

/// Distinct-value cap: above this a field is treated as continuous and refused.
const MAX_DISTINCT: usize = 64;

/// One distinct value row in the popup.
struct ValueRow {
    /// Canonical value display, e.g. "4".
    label: String,
    transitions: Vec<i64>,
    include: bool,
    name: String,
    color: [f32; 4],
}

/// A generated marker to append: (time, name, colour).
pub type MarkerSpec = (i64, String, [f32; 4]);

/// State of the open "Generate markers" popup, keyed by the source field.
pub struct GenerateMarkersDialog {
    field: FieldId,
    title: String,
    rows: Vec<ValueRow>,
    error: Option<String>,
}

impl GenerateMarkersDialog {
    /// Scan `field`'s value transitions and build the popup rows.
    pub fn open(snapshot: &StoreSnapshot, field: FieldId, title: String) -> Self {
        match field_value_transitions(snapshot, field, MAX_DISTINCT) {
            Ok(groups) => {
                let rows = groups
                    .into_iter()
                    .map(|g| ValueRow {
                        name: format!("Value {}", g.value_label),
                        color: value_color(&g.value_label),
                        include: true,
                        label: g.value_label,
                        transitions: g.transitions,
                    })
                    .collect();
                Self {
                    field,
                    title,
                    rows,
                    error: None,
                }
            }
            Err(TransitionsError::TooManyValues(n)) => Self {
                field,
                title,
                rows: Vec::new(),
                error: Some(format!(
                    "{n}+ distinct values - too many to generate markers (limit {MAX_DISTINCT})."
                )),
            },
            Err(TransitionsError::FieldView(_)) => Self {
                field,
                title,
                rows: Vec::new(),
                error: Some("Could not read this field.".to_string()),
            },
        }
    }
}

/// FNV-1a of the value label into the trace palette, so the same value is
/// always the same colour across regenerations and logs (ANA-11).
fn value_color(label: &str) -> [f32; 4] {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in label.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    delog_render::palette::trace_color(h as usize).to_srgb_f32()
}

fn color32_of(c: [f32; 4]) -> egui::Color32 {
    let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(u(c[0]), u(c[1]), u(c[2]), u(c[3]))
}

/// Render the popup. Returns the markers to append when the user clicks
/// generate (otherwise empty), and clears `dialog` when closed or generated.
pub fn generate_markers_window(
    ctx: &egui::Context,
    dialog: &mut Option<GenerateMarkersDialog>,
) -> Vec<MarkerSpec> {
    let Some(d) = dialog.as_mut() else {
        return Vec::new();
    };
    let mut open = true;
    let mut generated: Option<Vec<MarkerSpec>> = None;
    egui::Window::new(format!("Generate markers - {}", d.title))
        .id(egui::Id::new(("generate_markers", d.field.0)))
        .open(&mut open)
        .collapsible(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .resizable(true)
        .default_width(440.0)
        .show(ctx, |ui| {
            if let Some(err) = &d.error {
                ui.label(err);
                return;
            }
            let total: usize = d
                .rows
                .iter()
                .filter(|r| r.include)
                .map(|r| r.transitions.len())
                .sum();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(320.0)
                .show(ui, |ui| {
                    egui::Grid::new(("gen-markers-grid", d.field.0))
                        .num_columns(4)
                        .striped(true)
                        .spacing([10.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("");
                            ui.strong("Value");
                            ui.strong("Name");
                            ui.strong("Color");
                            ui.end_row();
                            for row in &mut d.rows {
                                ui.checkbox(&mut row.include, "");
                                ui.monospace(&row.label);
                                ui.add(
                                    egui::TextEdit::singleline(&mut row.name).desired_width(180.0),
                                );
                                let mut c = color32_of(row.color);
                                if egui::color_picker::color_edit_button_srgba(
                                    ui,
                                    &mut c,
                                    egui::color_picker::Alpha::Opaque,
                                )
                                .changed()
                                {
                                    row.color = crate::legend::color32_to_srgb(c);
                                }
                                ui.end_row();
                            }
                        });
                });

            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        total > 0,
                        egui::Button::new(format!("Generate {total} markers")),
                    )
                    .clicked()
                {
                    let mut specs = Vec::with_capacity(total);
                    for row in d.rows.iter().filter(|r| r.include) {
                        for &t in &row.transitions {
                            specs.push((t, row.name.clone(), row.color));
                        }
                    }
                    generated = Some(specs);
                }
                ui.weak(format!("{} value(s)", d.rows.len()));
            });
        });

    if generated.is_some() || !open {
        *dialog = None;
    }
    generated.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::value_color;

    #[test]
    fn value_color_is_stable_per_label() {
        // The same value label always maps to the same colour (ANA-11).
        assert_eq!(value_color("4"), value_color("4"));
        assert_eq!(value_color("AUTO"), value_color("AUTO"));
        // A colour is well-formed sRGBA in 0..=1.
        for c in value_color("4") {
            assert!((0.0..=1.0).contains(&c));
        }
    }
}
