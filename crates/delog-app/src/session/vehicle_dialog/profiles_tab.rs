use delog_core::snapshot::StoreSnapshot;

use crate::scene3d::vehicle::ModelKind;
use crate::ui::design_tokens::DesignTokens;

use super::draft::{OriMode, PosMode};
use super::profile_draft::{
    ProfileDraft, profile_general_summary, profile_orientation_summary, profile_position_summary,
};
use super::profiles::{handle_profile_action, load_profile_editor, save_profile_from_editor};
use super::widgets::{choose_custom_glb_path, control_width, form_grid, form_row, icon, section};
use super::{ProfileAction, VehicleDialog};

const RAIL_WIDTH: f32 = 190.0;
const ROW_HEIGHT: f32 = 28.0;

pub(super) fn show_profiles_tab(
    ui: &mut egui::Ui,
    state: &mut VehicleDialog,
    snapshot: &StoreSnapshot,
) {
    let mut select: Option<Option<String>> = None;
    let mut delete: Option<String> = None;

    egui::Panel::left("vehicle_profiles_rail")
        .resizable(true)
        .default_size(RAIL_WIDTH)
        .size_range(160.0..=320.0)
        .show_inside(ui, |ui| {
            show_rail(ui, state, &mut select, &mut delete);
        });

    egui::CentralPanel::default().show_inside(ui, |ui| {
        show_editor(ui, state, &mut delete);
    });

    if let Some(selected) = select {
        state.profile_editor_selected = selected;
        load_profile_editor(state);
    }
    if let Some(name) = delete {
        handle_profile_action(ProfileAction::Delete(name), state, snapshot);
    }
}

fn show_rail(
    ui: &mut egui::Ui,
    state: &VehicleDialog,
    select: &mut Option<Option<String>>,
    delete: &mut Option<String>,
) {
    let tokens = DesignTokens::from_style(ui.style());
    ui.add_space(tokens.space_sm);
    if ui
        .add_sized(
            [ui.available_width(), tokens.control_height],
            egui::Button::image_and_text(icon(ui, crate::ui::icons::plus()), "New Profile"),
        )
        .clicked()
    {
        *select = Some(None);
    }
    ui.add_space(tokens.space_sm);
    ui.separator();

    if state.profiles.is_empty() {
        ui.add_space(tokens.space_md);
        ui.label(egui::RichText::new("No saved profiles").weak());
        ui.label(
            egui::RichText::new("Save a configured vehicle as a profile to reuse it.")
                .weak()
                .small(),
        );
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for name in &state.profiles {
                let selected = state.profile_editor_selected.as_deref() == Some(name.as_str());
                let response = ui.add_sized(
                    [ui.available_width(), ROW_HEIGHT],
                    egui::Button::selectable(selected, name),
                );
                if response.clicked() {
                    *select = Some(Some(name.clone()));
                }
                response.context_menu(|ui| {
                    crate::ui::components::dense_rows(ui);
                    if ui
                        .add(egui::Button::image_and_text(
                            icon(ui, crate::ui::icons::trash()),
                            "Delete Profile",
                        ))
                        .clicked()
                    {
                        *delete = Some(name.clone());
                        ui.close();
                    }
                });
            }
        });
}

fn show_editor(ui: &mut egui::Ui, state: &mut VehicleDialog, delete: &mut Option<String>) {
    let tokens = DesignTokens::from_style(ui.style());
    let editing = state.profile_editor_selected.clone();

    ui.add_space(tokens.space_sm);
    ui.horizontal(|ui| {
        let title = editing.as_deref().unwrap_or("New profile");
        ui.label(egui::RichText::new(title).strong().size(15.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    editing.is_some(),
                    egui::Button::image(icon(ui, crate::ui::icons::trash())),
                )
                .on_hover_text("Delete profile")
                .clicked()
                && let Some(name) = editing.clone()
            {
                *delete = Some(name);
            }
            let label = if editing.is_some() {
                "Update Profile"
            } else {
                "Add Profile"
            };
            if ui
                .add(egui::Button::image_and_text(
                    icon(ui, crate::ui::icons::save()),
                    label,
                ))
                .clicked()
            {
                save_profile_from_editor(state);
            }
        });
    });
    ui.add_space(tokens.space_xs);
    ui.separator();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(tokens.space_sm);
            form_grid(ui, "vehicle_profile_name", |ui| {
                form_row(ui, "Name", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.profile_editor_name)
                            .hint_text("Profile name")
                            .desired_width(control_width(ui)),
                    );
                });
            });
            ui.add_space(tokens.space_sm);
            profile_editor_form(ui, &mut state.profile_editor_draft);
            ui.add_space(tokens.space_md);
        });
}

fn profile_editor_form(ui: &mut egui::Ui, draft: &mut ProfileDraft) {
    let tokens = DesignTokens::from_style(ui.style());
    section(
        ui,
        "vehicle-profile-general",
        "General",
        &profile_general_summary(draft),
        |ui| general_section(ui, draft),
    );
    ui.add_space(tokens.space_sm);
    section(
        ui,
        "vehicle-profile-position",
        "Position",
        &profile_position_summary(draft),
        |ui| position_section(ui, draft),
    );
    ui.add_space(tokens.space_sm);
    section(
        ui,
        "vehicle-profile-orientation",
        "Orientation",
        &profile_orientation_summary(draft),
        |ui| orientation_section(ui, draft),
    );
}

fn general_section(ui: &mut egui::Ui, draft: &mut ProfileDraft) {
    form_grid(ui, "vehicle_profile_general", |ui| {
        form_row(ui, "Vehicle Label", |ui| {
            ui.add(egui::TextEdit::singleline(&mut draft.label).desired_width(control_width(ui)));
        });
        form_row(ui, "Type", |ui| {
            egui::ComboBox::from_id_salt("vehicle-profile-model")
                .selected_text(draft.model.label())
                .width(control_width(ui))
                .show_ui(ui, |ui| {
                    for kind in ModelKind::PRESETS {
                        let label = kind.label().to_string();
                        ui.selectable_value(&mut draft.model, kind, label);
                    }
                    ui.selectable_value(
                        &mut draft.model,
                        ModelKind::CustomGlb(std::path::PathBuf::new()),
                        "Custom GLB",
                    );
                });
        });
        if matches!(draft.model, ModelKind::CustomGlb(_)) {
            form_row(ui, "GLB path", |ui| {
                ui.horizontal(|ui| {
                    let width = (control_width(ui) - 36.0).max(80.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.custom_path)
                            .hint_text("model.glb")
                            .desired_width(width),
                    );
                    if ui
                        .add_sized(
                            egui::vec2(28.0, 24.0),
                            egui::Button::image(icon(ui, crate::ui::icons::folder_open())),
                        )
                        .on_hover_text("Choose custom GLB")
                        .clicked()
                        && let Some(path) = choose_custom_glb_path(&draft.custom_path)
                    {
                        draft.custom_path = path;
                    }
                });
            });
        }
        form_row(ui, "Visible", |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut draft.show, "Vehicle");
                ui.checkbox(&mut draft.show_path, "Path");
            });
        });
        form_row(ui, "Colors", |ui| {
            ui.horizontal(|ui| {
                ui.color_edit_button_srgba(&mut draft.color)
                    .on_hover_text("Vehicle color");
                ui.color_edit_button_srgba(&mut draft.path_color)
                    .on_hover_text("Path color");
            });
        });
        form_row(ui, "Scale", |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut draft.scale)
                        .speed(0.05)
                        .range(0.05..=50.0),
                );
                ui.add(egui::Slider::new(&mut draft.scale, 0.05..=50.0).show_value(false));
            });
        });
    });
}

fn position_section(ui: &mut egui::Ui, draft: &mut ProfileDraft) {
    form_grid(ui, "vehicle_profile_position", |ui| {
        form_row(ui, "Frame", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut draft.pos_mode, PosMode::Ned, "Local (NED)");
                ui.selectable_value(&mut draft.pos_mode, PosMode::Gps, "Global (GPS)");
            });
        });
        profile_text_field(ui, "Topic", &mut draft.pos_topic, "topic name");
        match draft.pos_mode {
            PosMode::Ned => {
                profile_text_field(ui, "North (X)", &mut draft.north, "field name");
                profile_text_field(ui, "East (Y)", &mut draft.east, "field name");
                profile_text_field(ui, "Down (Z)", &mut draft.down, "field name");
                form_row(ui, "Reference origin", |ui| {
                    ui.checkbox(&mut draft.ned_has_ref, "");
                });
                if draft.ned_has_ref {
                    form_row(ui, "Fixed values", |ui| {
                        ui.checkbox(&mut draft.ned_ref_manual, "");
                    });
                    if draft.ned_ref_manual {
                        form_row(ui, "Ref lat/lon/alt", |ui| {
                            ui.horizontal(|ui| {
                                ui.add(egui::DragValue::new(&mut draft.ref_lat).speed(0.0001));
                                ui.add(egui::DragValue::new(&mut draft.ref_lon).speed(0.0001));
                                ui.add(egui::DragValue::new(&mut draft.ref_alt).speed(0.1));
                            });
                        });
                    } else {
                        profile_text_field(ui, "Ref Latitude", &mut draft.ref_lat_f, "field name");
                        profile_text_field(ui, "Ref Longitude", &mut draft.ref_lon_f, "field name");
                        profile_text_field(ui, "Ref Altitude", &mut draft.ref_alt_f, "field name");
                    }
                }
            }
            PosMode::Gps => {
                profile_text_field(ui, "Latitude", &mut draft.lat, "field name");
                profile_text_field(ui, "Longitude", &mut draft.lon, "field name");
                form_row(ui, "Lat/Lon units", |ui| {
                    ui.checkbox(&mut draft.lat_lon_dege7, "degE7");
                });
                profile_text_field(ui, "Altitude", &mut draft.alt, "field name");
                form_row(ui, "Altitude units", |ui| {
                    ui.checkbox(&mut draft.alt_mm, "mm");
                });
                form_row(ui, "Altitude offset", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut draft.alt_offset_m)
                            .speed(1.0)
                            .suffix(" m"),
                    );
                });
            }
        }
    });
}

fn orientation_section(ui: &mut egui::Ui, draft: &mut ProfileDraft) {
    form_grid(ui, "vehicle_profile_orientation", |ui| {
        form_row(ui, "Mode", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut draft.ori_mode, OriMode::Static, "Static");
                ui.selectable_value(&mut draft.ori_mode, OriMode::Euler, "Euler");
                ui.selectable_value(&mut draft.ori_mode, OriMode::Quat, "Quaternion");
            });
        });
        if draft.ori_mode == OriMode::Static {
            return;
        }
        profile_text_field(ui, "Topic", &mut draft.ori_topic, "topic name");
        match draft.ori_mode {
            OriMode::Static => {}
            OriMode::Euler => {
                form_row(ui, "Angle Unit", |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut draft.euler_degrees, true, "Degrees");
                        ui.selectable_value(&mut draft.euler_degrees, false, "Radians");
                    });
                });
                profile_text_field(ui, "Roll", &mut draft.roll, "field name");
                profile_text_field(ui, "Pitch", &mut draft.pitch, "field name");
                profile_text_field(ui, "Yaw", &mut draft.yaw, "field name");
            }
            OriMode::Quat => {
                profile_text_field(ui, "QW", &mut draft.qw, "field name");
                profile_text_field(ui, "QX", &mut draft.qx, "field name");
                profile_text_field(ui, "QY", &mut draft.qy, "field name");
                profile_text_field(ui, "QZ", &mut draft.qz, "field name");
            }
        }
    });
}

fn profile_text_field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    form_row(ui, label, |ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width(control_width(ui)),
        );
    });
}
