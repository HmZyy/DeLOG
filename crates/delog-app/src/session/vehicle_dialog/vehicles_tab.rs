use delog_core::identity::SourceId;
use delog_core::snapshot::StoreSnapshot;

use crate::scene3d::vehicle::ModelKind;

use super::draft::{Draft, OriMode, PosMode, source_topics, topic_fields};
use super::profiles::handle_profile_action;
use super::widgets::{
    choose_custom_glb_path, combo_label, grid_field, icon, section_heading, topic_combo,
};
use super::{ProfileAction, VehicleDialog};

pub(super) fn show_vehicle_config_tab(
    ui: &mut egui::Ui,
    state: &mut VehicleDialog,
    snapshot: &StoreSnapshot,
) {
    if ui
        .add(egui::Button::image_and_text(
            icon(ui, crate::ui::icons::plus()),
            "Add Vehicle",
        ))
        .clicked()
    {
        let n = state.drafts.len() + 1;
        state.drafts.push(Draft {
            label: format!("Vehicle #{n}"),
            ..Draft::default()
        });
    }
    ui.add_space(8.0);

    let mut remove: Option<usize> = None;
    let mut duplicate: Option<usize> = None;
    let mut profile_action: Option<ProfileAction> = None;
    let profile_names = state.profiles.clone();
    egui::ScrollArea::vertical().show(ui, |ui| {
        for (i, draft) in state.drafts.iter_mut().enumerate() {
            let title = if draft.label.trim().is_empty() {
                format!("Vehicle #{}", i + 1)
            } else {
                draft.label.clone()
            };
            egui::CollapsingHeader::new(title)
                .id_salt(("vehicle", i))
                .default_open(true)
                .show(ui, |ui| {
                    show_vehicle_profile_dropdown(
                        ui,
                        i,
                        &profile_names,
                        draft,
                        &mut profile_action,
                    );
                    ui.add_space(8.0);
                    draft_editor(ui, draft, snapshot);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Button::image_and_text(
                                icon(ui, crate::ui::icons::trash()),
                                "Remove Vehicle",
                            ))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                        if ui
                            .add(egui::Button::image_and_text(
                                icon(ui, crate::ui::icons::copy()),
                                "Duplicate",
                            ))
                            .clicked()
                        {
                            duplicate = Some(i);
                        }
                    });
                });
            ui.add_space(6.0);
        }
    });
    if let Some(i) = duplicate {
        let mut copy = state.drafts[i].clone();
        copy.label = format!("{} copy", copy.label);
        state.drafts.insert(i + 1, copy);
    }
    if let Some(i) = remove {
        state.drafts.remove(i);
    }
    if let Some(action) = profile_action {
        handle_profile_action(action, state, snapshot);
    }
}

fn show_vehicle_profile_dropdown(
    ui: &mut egui::Ui,
    draft_index: usize,
    profiles: &[String],
    draft: &mut Draft,
    action: &mut Option<ProfileAction>,
) {
    ui.horizontal(|ui| {
        ui.label("Profile");
        let before = draft.selected_profile.clone();
        egui::ComboBox::from_id_salt(("vehicle-profile", draft_index))
            .selected_text(draft.selected_profile.as_deref().unwrap_or("-"))
            .show_ui(ui, |ui| {
                for name in profiles {
                    ui.selectable_value(&mut draft.selected_profile, Some(name.clone()), name);
                }
            });
        if draft.selected_profile != before
            && let Some(name) = draft.selected_profile.clone()
        {
            *action = Some(ProfileAction::Apply {
                draft: draft_index,
                name,
            });
        }
    });
}

fn draft_editor(ui: &mut egui::Ui, draft: &mut Draft, snapshot: &StoreSnapshot) {
    let sources: Vec<(SourceId, String)> = snapshot
        .sources
        .iter()
        .filter(|s| !s.entry.removed)
        .map(|s| (s.entry.id, s.entry.label.clone()))
        .collect();

    egui::Grid::new("vehicle_grid_general")
        .num_columns(2)
        .spacing([18.0, 8.0])
        .show(ui, |ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut draft.label);
            ui.end_row();

            ui.label("Visible");
            ui.checkbox(&mut draft.show, "");
            ui.end_row();

            ui.label("Path Visible");
            ui.checkbox(&mut draft.show_path, "");
            ui.end_row();

            ui.label("Source");
            egui::ComboBox::from_id_salt("veh-source")
                .selected_text(combo_label(&sources, &draft.source))
                .show_ui(ui, |ui| {
                    for (id, label) in &sources {
                        if ui
                            .selectable_value(&mut draft.source, Some(*id), label)
                            .clicked()
                        {
                            // New source ⇒ clear stale topic/column selections.
                            *draft = Draft {
                                source: Some(*id),
                                label: draft.label.clone(),
                                show: draft.show,
                                show_path: draft.show_path,
                                model: draft.model.clone(),
                                custom_path: draft.custom_path.clone(),
                                color: draft.color,
                                path_color: draft.path_color,
                                scale: draft.scale,
                                ..Draft::default()
                            };
                        }
                    }
                });
            ui.end_row();

            ui.label("Type");
            egui::ComboBox::from_id_salt("veh-model")
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
                    let has_path = !draft.custom_path.trim().is_empty();
                    let text = if has_path {
                        draft.custom_path.as_str()
                    } else {
                        "No GLB selected"
                    };
                    let label =
                        ui.add_sized(egui::vec2(150.0, 18.0), egui::Label::new(text).truncate());
                    if has_path {
                        label.on_hover_text(draft.custom_path.as_str());
                    }
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
                    if ui
                        .add_enabled(
                            has_path,
                            egui::Button::image(icon(ui, crate::ui::icons::close())),
                        )
                        .on_hover_text("Clear custom GLB")
                        .clicked()
                    {
                        draft.custom_path.clear();
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

    let Some(source) = draft.source else {
        return;
    };
    let topics = source_topics(snapshot, source);

    ui.add_space(4.0);
    ui.separator();
    section_heading(ui, "Orientation");
    egui::Grid::new("vehicle_grid_orientation")
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
                ui.label("Topic");
                if topic_combo(ui, "veh-ori-topic", &mut draft.ori_topic, &topics) {
                    draft.roll = None;
                    draft.pitch = None;
                    draft.yaw = None;
                    draft.qw = None;
                    draft.qx = None;
                    draft.qy = None;
                    draft.qz = None;
                }
                ui.end_row();
                if let Some(topic) = draft.ori_topic {
                    let cols = topic_fields(snapshot, topic);
                    match draft.ori_mode {
                        OriMode::Static => {}
                        OriMode::Euler => {
                            ui.label("Angle Unit");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut draft.euler_degrees, true, "Degrees");
                                ui.selectable_value(&mut draft.euler_degrees, false, "Radians");
                            });
                            ui.end_row();
                            grid_field(ui, "veh-roll", "Roll", &mut draft.roll, &cols);
                            grid_field(ui, "veh-pitch", "Pitch", &mut draft.pitch, &cols);
                            grid_field(ui, "veh-yaw", "Yaw", &mut draft.yaw, &cols);
                        }
                        OriMode::Quat => {
                            grid_field(ui, "veh-qw", "QW", &mut draft.qw, &cols);
                            grid_field(ui, "veh-qx", "QX", &mut draft.qx, &cols);
                            grid_field(ui, "veh-qy", "QY", &mut draft.qy, &cols);
                            grid_field(ui, "veh-qz", "QZ", &mut draft.qz, &cols);
                        }
                    }
                }
            }
        });

    ui.add_space(4.0);
    ui.separator();
    section_heading(ui, "Position");
    egui::Grid::new("vehicle_grid_position")
        .num_columns(2)
        .spacing([18.0, 8.0])
        .show(ui, |ui| {
            ui.label("Frame");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut draft.pos_mode, PosMode::Ned, "Local (NED)");
                ui.selectable_value(&mut draft.pos_mode, PosMode::Gps, "Global (GPS)");
            });
            ui.end_row();

            ui.label("Topic");
            if topic_combo(ui, "veh-pos-topic", &mut draft.pos_topic, &topics) {
                draft.north = None;
                draft.east = None;
                draft.down = None;
                draft.lat = None;
                draft.lon = None;
                draft.alt = None;
                draft.ref_lat_f = None;
                draft.ref_lon_f = None;
                draft.ref_alt_f = None;
            }
            ui.end_row();

            if let Some(topic) = draft.pos_topic {
                let cols = topic_fields(snapshot, topic);
                match draft.pos_mode {
                    PosMode::Ned => {
                        grid_field(ui, "veh-n", "North (X)", &mut draft.north, &cols);
                        grid_field(ui, "veh-e", "East (Y)", &mut draft.east, &cols);
                        grid_field(ui, "veh-d", "Down (Z)", &mut draft.down, &cols);
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
                                grid_field(ui, "veh-rlat", "Ref Lat", &mut draft.ref_lat_f, &cols);
                                grid_field(ui, "veh-rlon", "Ref Lon", &mut draft.ref_lon_f, &cols);
                                grid_field(ui, "veh-ralt", "Ref Alt", &mut draft.ref_alt_f, &cols);
                            }
                        }
                    }
                    PosMode::Gps => {
                        grid_field(ui, "veh-lat", "Latitude", &mut draft.lat, &cols);
                        grid_field(ui, "veh-lon", "Longitude", &mut draft.lon, &cols);
                        ui.label("Lat/Lon units");
                        ui.checkbox(&mut draft.lat_lon_dege7, "degE7");
                        ui.end_row();
                        grid_field(ui, "veh-alt", "Altitude", &mut draft.alt, &cols);
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
            }
        });
}
