use delog_core::snapshot::StoreSnapshot;

use crate::scene3d::vehicle::ModelKind;

use super::draft::{OriMode, PosMode};
use super::profile_draft::ProfileDraft;
use super::profiles::{handle_profile_action, load_profile_editor, save_profile_from_editor};
use super::widgets::{choose_custom_glb_path, icon, section_heading};
use super::{ProfileAction, VehicleDialog};

pub(super) fn show_profiles_tab(
    ui: &mut egui::Ui,
    state: &mut VehicleDialog,
    snapshot: &StoreSnapshot,
) {
    ui.horizontal(|ui| {
        ui.label("Profile");
        let before = state.profile_editor_selected.clone();
        egui::ComboBox::from_id_salt("vehicle-profile-editor")
            .selected_text(
                state
                    .profile_editor_selected
                    .as_deref()
                    .unwrap_or("New profile"),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.profile_editor_selected, None, "New profile");
                for name in &state.profiles {
                    ui.selectable_value(
                        &mut state.profile_editor_selected,
                        Some(name.clone()),
                        name,
                    );
                }
            });
        if state.profile_editor_selected != before {
            load_profile_editor(state);
        }
    });

    ui.horizontal(|ui| {
        ui.label("Name");
        ui.add(
            egui::TextEdit::singleline(&mut state.profile_editor_name)
                .hint_text("Profile name")
                .desired_width(150.0),
        );
    });

    ui.horizontal(|ui| {
        let label = if state.profile_editor_selected.is_some() {
            "Update Profile"
        } else {
            "Add Profile"
        };
        if ui.button(label).clicked() {
            save_profile_from_editor(state);
        }
        if ui
            .add_enabled(
                state.profile_editor_selected.is_some(),
                egui::Button::image(icon(ui, crate::ui::icons::trash())),
            )
            .on_hover_text("Delete profile")
            .clicked()
            && let Some(name) = state.profile_editor_selected.clone()
        {
            handle_profile_action(ProfileAction::Delete(name), state, snapshot);
        }
    });

    ui.add_space(8.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        profile_editor_form(ui, &mut state.profile_editor_draft);
    });
}

fn profile_editor_form(ui: &mut egui::Ui, draft: &mut ProfileDraft) {
    section_heading(ui, "General");
    egui::Grid::new("vehicle_profile_general")
        .num_columns(2)
        .spacing([18.0, 8.0])
        .show(ui, |ui| {
            ui.label("Vehicle Label");
            ui.text_edit_singleline(&mut draft.label);
            ui.end_row();

            ui.label("Visible");
            ui.checkbox(&mut draft.show, "");
            ui.end_row();

            ui.label("Path Visible");
            ui.checkbox(&mut draft.show_path, "");
            ui.end_row();

            ui.label("Type");
            egui::ComboBox::from_id_salt("vehicle-profile-model")
                .selected_text(draft.model.label())
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
            ui.end_row();

            if matches!(draft.model, ModelKind::CustomGlb(_)) {
                ui.label("GLB path");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.custom_path)
                            .hint_text("model.glb")
                            .desired_width(150.0),
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
                ui.end_row();
            }

            ui.label("Vehicle Color");
            ui.color_edit_button_srgba(&mut draft.color);
            ui.end_row();

            ui.label("Path Color");
            ui.color_edit_button_srgba(&mut draft.path_color);
            ui.end_row();

            ui.label("Scale");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut draft.scale)
                        .speed(0.05)
                        .range(0.05..=50.0),
                );
                ui.add(egui::Slider::new(&mut draft.scale, 0.05..=50.0).show_value(false));
            });
            ui.end_row();
        });

    ui.add_space(6.0);
    ui.separator();
    section_heading(ui, "Position");
    egui::Grid::new("vehicle_profile_position")
        .num_columns(2)
        .spacing([18.0, 8.0])
        .show(ui, |ui| {
            ui.label("Frame");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut draft.pos_mode, PosMode::Ned, "Local (NED)");
                ui.selectable_value(&mut draft.pos_mode, PosMode::Gps, "Global (GPS)");
            });
            ui.end_row();

            profile_text_field(ui, "Topic", &mut draft.pos_topic, "topic name");
            match draft.pos_mode {
                PosMode::Ned => {
                    profile_text_field(ui, "North (X)", &mut draft.north, "field name");
                    profile_text_field(ui, "East (Y)", &mut draft.east, "field name");
                    profile_text_field(ui, "Down (Z)", &mut draft.down, "field name");
                    ui.label("Reference origin");
                    ui.checkbox(&mut draft.ned_has_ref, "");
                    ui.end_row();
                    if draft.ned_has_ref {
                        ui.label("Fixed values");
                        ui.checkbox(&mut draft.ned_ref_manual, "");
                        ui.end_row();
                        if draft.ned_ref_manual {
                            ui.label("Ref lat/lon/alt");
                            ui.horizontal(|ui| {
                                ui.add(egui::DragValue::new(&mut draft.ref_lat).speed(0.0001));
                                ui.add(egui::DragValue::new(&mut draft.ref_lon).speed(0.0001));
                                ui.add(egui::DragValue::new(&mut draft.ref_alt).speed(0.1));
                            });
                            ui.end_row();
                        } else {
                            profile_text_field(
                                ui,
                                "Ref Latitude",
                                &mut draft.ref_lat_f,
                                "field name",
                            );
                            profile_text_field(
                                ui,
                                "Ref Longitude",
                                &mut draft.ref_lon_f,
                                "field name",
                            );
                            profile_text_field(
                                ui,
                                "Ref Altitude",
                                &mut draft.ref_alt_f,
                                "field name",
                            );
                        }
                    }
                }
                PosMode::Gps => {
                    profile_text_field(ui, "Latitude", &mut draft.lat, "field name");
                    profile_text_field(ui, "Longitude", &mut draft.lon, "field name");
                    profile_text_field(ui, "Altitude", &mut draft.alt, "field name");
                    ui.label("Lat/Lon units");
                    ui.checkbox(&mut draft.lat_lon_dege7, "degE7");
                    ui.end_row();
                    ui.label("Altitude units");
                    ui.checkbox(&mut draft.alt_mm, "mm");
                    ui.end_row();
                    ui.label("Altitude offset");
                    ui.add(
                        egui::DragValue::new(&mut draft.alt_offset_m)
                            .speed(1.0)
                            .suffix(" m"),
                    );
                    ui.end_row();
                }
            }
        });

    ui.add_space(6.0);
    ui.separator();
    section_heading(ui, "Orientation");
    egui::Grid::new("vehicle_profile_orientation")
        .num_columns(2)
        .spacing([18.0, 8.0])
        .show(ui, |ui| {
            ui.label("Mode");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut draft.ori_mode, OriMode::Static, "Static");
                ui.selectable_value(&mut draft.ori_mode, OriMode::Euler, "Euler");
                ui.selectable_value(&mut draft.ori_mode, OriMode::Quat, "Quaternion");
            });
            ui.end_row();

            if draft.ori_mode != OriMode::Static {
                profile_text_field(ui, "Topic", &mut draft.ori_topic, "topic name");
                match draft.ori_mode {
                    OriMode::Static => {}
                    OriMode::Euler => {
                        profile_text_field(ui, "Roll", &mut draft.roll, "field name");
                        profile_text_field(ui, "Pitch", &mut draft.pitch, "field name");
                        profile_text_field(ui, "Yaw", &mut draft.yaw, "field name");
                        ui.label("Angle Unit");
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut draft.euler_degrees, true, "Degrees");
                            ui.selectable_value(&mut draft.euler_degrees, false, "Radians");
                        });
                        ui.end_row();
                    }
                    OriMode::Quat => {
                        profile_text_field(ui, "QW", &mut draft.qw, "field name");
                        profile_text_field(ui, "QX", &mut draft.qx, "field name");
                        profile_text_field(ui, "QY", &mut draft.qy, "field name");
                        profile_text_field(ui, "QZ", &mut draft.qz, "field name");
                    }
                }
            }
        });
}

fn profile_text_field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(150.0),
    );
    ui.end_row();
}
