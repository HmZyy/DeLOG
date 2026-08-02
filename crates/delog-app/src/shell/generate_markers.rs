use egui_extras::{Column, TableBuilder};

use delog_core::analysis::{TransitionsError, field_value_transitions};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

/// Above this cap a field is treated as continuous and refused.
const MAX_DISTINCT: usize = 64;

struct ValueRow {
    label: String,
    transitions: Vec<i64>,
    include: bool,
    name: String,
    color: [f32; 4],
}

/// (time, name, colour)
pub type MarkerSpec = (i64, String, [f32; 4]);

pub struct GenerateMarkersDialog {
    field: FieldId,
    title: String,
    rows: Vec<ValueRow>,
    error: Option<String>,
}

impl GenerateMarkersDialog {
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

/// Hash the label into the palette so the same value keeps its colour across
/// regenerations and logs.
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

/// Clears `dialog` when closed or generated.
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
        .default_width(560.0)
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

            ui.set_min_width(520.0);
            if d.rows.is_empty() {
                ui.weak("No repeated values to turn into markers.");
                return;
            }
            let body_font = egui::TextStyle::Body.resolve(ui.style());
            let row_height = ui.spacing().interact_size.y.max(body_font.size);
            let include_w = 28.0;
            let value_w = 110.0;
            let color_w = 48.0;

            TableBuilder::new(ui)
                .id_salt(("generate-markers-table", d.field.0))
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .auto_shrink([false, false])
                .max_scroll_height(320.0)
                .column(Column::exact(include_w))
                .column(Column::initial(value_w).clip(true))
                .column(Column::remainder().at_least(140.0))
                .column(Column::exact(color_w))
                .header(row_height, |mut header| {
                    header.col(|_ui| {});
                    header.col(|ui| {
                        ui.strong("Value");
                    });
                    header.col(|ui| {
                        ui.strong("Name");
                    });
                    header.col(|ui| {
                        ui.strong("Color");
                    });
                })
                .body(|body| {
                    body.rows(row_height, d.rows.len(), |mut table_row| {
                        let row = &mut d.rows[table_row.index()];
                        table_row.col(|ui| {
                            ui.checkbox(&mut row.include, "");
                        });
                        table_row.col(|ui| {
                            ui.monospace(&row.label);
                        });
                        table_row.col(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut row.name)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                        table_row.col(|ui| {
                            let mut c = color32_of(row.color);
                            if egui::color_picker::color_edit_button_srgba(
                                ui,
                                &mut c,
                                egui::color_picker::Alpha::Opaque,
                            )
                            .changed()
                            {
                                row.color = crate::plotting::legend::color32_to_srgb(c);
                            }
                        });
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
    use super::{GenerateMarkersDialog, ValueRow, generate_markers_window, value_color};

    fn dialog_with_rows() -> GenerateMarkersDialog {
        GenerateMarkersDialog {
            field: delog_core::identity::FieldId(1),
            title: "MODE.Mode".to_owned(),
            rows: vec![
                ValueRow {
                    label: "0".to_owned(),
                    transitions: vec![10, 20],
                    include: true,
                    name: "Value 0".to_owned(),
                    color: [1.0, 0.0, 0.0, 1.0],
                },
                ValueRow {
                    label: "1".to_owned(),
                    transitions: vec![30],
                    include: true,
                    name: "Value 1".to_owned(),
                    color: [0.0, 1.0, 0.0, 1.0],
                },
            ],
            error: None,
        }
    }

    fn painted(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    out.push((text.galley.job.text.clone(), text.visual_bounding_rect()));
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn marker_table_columns_recover_after_a_narrow_first_layout() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let mut dialog = Some(dialog_with_rows());
        let render = |width: f32, dialog: &mut Option<GenerateMarkersDialog>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 700.0),
                )),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                generate_markers_window(ui.ctx(), dialog);
            })
        };

        render(320.0, &mut dialog);
        render(320.0, &mut dialog);
        render(1_200.0, &mut dialog);
        let output = render(1_200.0, &mut dialog);

        let texts = painted(&output);
        let find = |label: &str| {
            texts
                .iter()
                .find(|(text, _)| text == label)
                .unwrap_or_else(|| panic!("{label} header should be painted"))
                .1
        };
        let name_w = find("Color").left() - find("Name").left();
        assert!(
            name_w > 200.0,
            "after the window grows the name column should expand too, got {name_w} points"
        );
    }

    #[test]
    fn marker_table_gives_the_name_column_the_extra_width() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let mut dialog = Some(dialog_with_rows());
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_000.0, 700.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(input(), |ui| {
            generate_markers_window(ui.ctx(), &mut dialog);
        });
        let output = ctx.run_ui(input(), |ui| {
            generate_markers_window(ui.ctx(), &mut dialog);
        });

        let texts = painted(&output);
        let find = |label: &str| {
            texts
                .iter()
                .find(|(text, _)| text == label)
                .unwrap_or_else(|| panic!("{label} header should be painted, got {texts:?}"))
                .1
        };

        let value = find("Value");
        let name = find("Name");
        let color = find("Color");

        assert!(
            name.left() > value.left(),
            "columns should run Value then Name"
        );
        assert!(
            color.left() > name.left(),
            "Color should be the last column"
        );
        let value_w = name.left() - value.left();
        let name_w = color.left() - name.left();
        assert!(
            name_w > value_w * 2.0,
            "the name column should soak up the spare width (value {value_w}, name {name_w})"
        );
        assert!(
            texts.iter().any(|(text, _)| text == "Value 0"),
            "each row should render its editable name"
        );
    }


    #[test]
    fn value_color_is_stable_per_label() {
        assert_eq!(value_color("4"), value_color("4"));
        assert_eq!(value_color("AUTO"), value_color("AUTO"));
        for c in value_color("4") {
            assert!((0.0..=1.0).contains(&c));
        }
    }
}
