use delog_core::identity::SourceId;
use delog_core::snapshot::StoreSnapshot;
use egui::Color32;

use super::draft::{
    Draft, OriMode, PosMode, general_summary, orientation_summary, position_summary, source_topics,
    topic_fields,
};
use super::profiles::handle_profile_action;
use super::widgets::{
    choose_custom_glb_path, combo_label, control_width, field_combo, form_grid, form_row, icon,
    searchable_combo, section, status_banner, swatch,
};
use super::{ProfileAction, VehicleDialog};
use crate::scene3d::vehicle::ModelKind;
use crate::ui::design_tokens::DesignTokens;

const RAIL_WIDTH: f32 = 190.0;
const ROW_HEIGHT: f32 = 42.0;

struct Row {
    name: String,
    color: Color32,
    status: String,
    incomplete: bool,
}

struct Pending {
    select: Option<usize>,
    add: bool,
    remove: Option<usize>,
    duplicate: Option<usize>,
    profile: Option<ProfileAction>,
}

impl Pending {
    fn new() -> Self {
        Self {
            select: None,
            add: false,
            remove: None,
            duplicate: None,
            profile: None,
        }
    }
}

pub(super) fn show_vehicle_config_tab(
    ui: &mut egui::Ui,
    state: &mut VehicleDialog,
    snapshot: &StoreSnapshot,
) {
    let mut pending = Pending::new();
    let rows: Vec<Row> = state
        .drafts
        .iter()
        .enumerate()
        .map(|(i, draft)| row_for(i, draft, snapshot))
        .collect();

    egui::Panel::left("vehicle_dialog_rail")
        .resizable(true)
        .default_size(RAIL_WIDTH)
        .size_range(160.0..=320.0)
        .show_inside(ui, |ui| {
            show_rail(ui, state.selected_vehicle, &rows, &mut pending);
        });

    egui::CentralPanel::default().show_inside(ui, |ui| {
        show_detail(ui, state, snapshot, &mut pending);
    });

    apply_pending(state, snapshot, pending);
}

fn row_for(index: usize, draft: &Draft, snapshot: &StoreSnapshot) -> Row {
    let missing = draft.missing();
    let status = if !missing.is_empty() {
        "incomplete".to_owned()
    } else if !draft.show {
        "hidden".to_owned()
    } else {
        source_name(snapshot, draft.source).unwrap_or_else(|| "No source".to_owned())
    };
    Row {
        name: if draft.label.trim().is_empty() {
            format!("Vehicle #{}", index + 1)
        } else {
            draft.label.clone()
        },
        color: draft.color,
        status,
        incomplete: !missing.is_empty(),
    }
}

fn source_name(snapshot: &StoreSnapshot, source: Option<SourceId>) -> Option<String> {
    let source = source?;
    snapshot
        .sources
        .iter()
        .find(|s| s.entry.id == source && !s.entry.removed)
        .map(|s| s.entry.label.clone())
}

fn show_rail(ui: &mut egui::Ui, selected: usize, rows: &[Row], pending: &mut Pending) {
    let tokens = DesignTokens::from_style(ui.style());
    ui.add_space(tokens.space_sm);
    if ui
        .add_sized(
            [ui.available_width(), tokens.control_height],
            egui::Button::image_and_text(icon(ui, crate::ui::icons::plus()), "Add Vehicle"),
        )
        .clicked()
    {
        pending.add = true;
    }
    ui.add_space(tokens.space_sm);
    ui.separator();

    if rows.is_empty() {
        ui.add_space(tokens.space_md);
        ui.label(egui::RichText::new("No vehicles yet").weak());
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                let response = rail_row(ui, row, i == selected);
                if response.clicked() {
                    pending.select = Some(i);
                }
                response.context_menu(|ui| {
                    crate::ui::components::dense_rows(ui);
                    action_menu(ui, i, pending);
                });
            }
        });
}

fn rail_row(ui: &mut egui::Ui, row: &Row, selected: bool) -> egui::Response {
    let tokens = DesignTokens::from_style(ui.style());
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_HEIGHT),
        egui::Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() {
        ui.painter()
            .rect_filled(rect, tokens.radius, visuals.weak_bg_fill);
    }

    let swatch_rect = egui::Rect::from_min_size(
        rect.left_center() + egui::vec2(tokens.space_sm, -5.0),
        egui::vec2(10.0, 10.0),
    );
    swatch(ui, swatch_rect, row.color);

    let text_left = swatch_rect.right() + tokens.space_sm;
    ui.painter().text(
        egui::pos2(text_left, rect.top() + 7.0),
        egui::Align2::LEFT_TOP,
        &row.name,
        egui::TextStyle::Body.resolve(ui.style()),
        visuals.text_color(),
    );
    let status_color = if row.incomplete {
        ui.visuals().warn_fg_color
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().text(
        egui::pos2(text_left, rect.bottom() - 7.0),
        egui::Align2::LEFT_BOTTOM,
        &row.status,
        egui::TextStyle::Small.resolve(ui.style()),
        status_color,
    );
    response
}

fn action_menu(ui: &mut egui::Ui, index: usize, pending: &mut Pending) {
    if ui
        .add(egui::Button::image_and_text(
            icon(ui, crate::ui::icons::copy()),
            "Duplicate Vehicle",
        ))
        .clicked()
    {
        pending.duplicate = Some(index);
        ui.close();
    }
    if ui
        .add(egui::Button::image_and_text(
            icon(ui, crate::ui::icons::save()),
            "Save As Profile",
        ))
        .clicked()
    {
        pending.profile = Some(ProfileAction::SaveAs { draft: index });
        ui.close();
    }
    if ui
        .add(egui::Button::image_and_text(
            icon(ui, crate::ui::icons::trash()),
            "Remove Vehicle",
        ))
        .clicked()
    {
        pending.remove = Some(index);
        ui.close();
    }
}

fn show_detail(
    ui: &mut egui::Ui,
    state: &mut VehicleDialog,
    snapshot: &StoreSnapshot,
    pending: &mut Pending,
) {
    let tokens = DesignTokens::from_style(ui.style());
    let index = state.selected_vehicle;
    if state.drafts.get(index).is_none() {
        ui.add_space(tokens.space_md);
        ui.vertical_centered(|ui| {
            ui.add_space(tokens.space_md);
            ui.label(egui::RichText::new("No vehicle selected").weak());
        });
        return;
    }

    let profile_names = state.profiles.clone();
    let draft = &mut state.drafts[index];
    let title = if draft.label.trim().is_empty() {
        format!("Vehicle #{}", index + 1)
    } else {
        draft.label.clone()
    };

    ui.add_space(tokens.space_sm);
    ui.horizontal(|ui| {
        let (swatch_rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
        swatch(ui, swatch_rect, draft.color);
        ui.label(egui::RichText::new(&title).strong().size(15.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_image_button(icon(ui, crate::ui::icons::ellipsis()), |ui| {
                crate::ui::components::dense_rows(ui);
                action_menu(ui, index, pending);
            })
            .response
            .on_hover_text("Vehicle actions");
        });
    });
    ui.add_space(tokens.space_xs);
    ui.separator();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(tokens.space_sm);
            status_banner(ui, &draft.missing());
            ui.add_space(tokens.space_sm);

            show_profile_picker(ui, index, &profile_names, draft, &mut pending.profile);
            ui.add_space(tokens.space_sm);

            let source_label = source_name(snapshot, draft.source);
            section(
                ui,
                ("vehicle-general", index),
                "General",
                &general_summary(draft, source_label.as_deref()),
                |ui| general_section(ui, draft, snapshot),
            );
            ui.add_space(tokens.space_sm);

            let Some(source) = draft.source else {
                return;
            };
            let topics = source_topics(snapshot, source);
            let pos_topic = topic_name(&topics, draft.pos_topic);
            section(
                ui,
                ("vehicle-position", index),
                "Position",
                &position_summary(draft, pos_topic.as_deref()),
                |ui| position_section(ui, draft, snapshot, &topics),
            );
            ui.add_space(tokens.space_sm);

            let ori_topic = topic_name(&topics, draft.ori_topic);
            section(
                ui,
                ("vehicle-orientation", index),
                "Orientation",
                &orientation_summary(draft, ori_topic.as_deref()),
                |ui| orientation_section(ui, draft, snapshot, &topics),
            );
            ui.add_space(tokens.space_md);
        });
}

fn topic_name(
    topics: &[(delog_core::identity::TopicId, String)],
    selected: Option<delog_core::identity::TopicId>,
) -> Option<String> {
    let selected = selected?;
    topics
        .iter()
        .find(|(id, _)| *id == selected)
        .map(|(_, name)| name.clone())
}

fn show_profile_picker(
    ui: &mut egui::Ui,
    draft_index: usize,
    profiles: &[String],
    draft: &mut Draft,
    action: &mut Option<ProfileAction>,
) {
    form_grid(ui, "vehicle_profile_picker", |ui| {
        form_row(ui, "Profile", |ui| {
            let before = draft.selected_profile.clone();
            egui::ComboBox::from_id_salt(("vehicle-profile", draft_index))
                .selected_text(draft.selected_profile.as_deref().unwrap_or("-"))
                .width(control_width(ui))
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
    });
}

fn general_section(ui: &mut egui::Ui, draft: &mut Draft, snapshot: &StoreSnapshot) {
    let sources: Vec<(SourceId, String)> = snapshot
        .sources
        .iter()
        .filter(|s| !s.entry.removed)
        .map(|s| (s.entry.id, s.entry.label.clone()))
        .collect();

    form_grid(ui, "vehicle_grid_general", |ui| {
        form_row(ui, "Name", |ui| {
            ui.add(egui::TextEdit::singleline(&mut draft.label).desired_width(control_width(ui)));
        });
        form_row(ui, "Source", |ui| {
            egui::ComboBox::from_id_salt("veh-source")
                .selected_text(combo_label(&sources, &draft.source))
                .width(control_width(ui))
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
        });
        form_row(ui, "Type", |ui| {
            egui::ComboBox::from_id_salt("veh-model")
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
                    let has_path = !draft.custom_path.trim().is_empty();
                    let text = if has_path {
                        draft.custom_path.as_str()
                    } else {
                        "No GLB selected"
                    };
                    let width = (control_width(ui) - 64.0).max(80.0);
                    let label =
                        ui.add_sized(egui::vec2(width, 18.0), egui::Label::new(text).truncate());
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

fn position_section(
    ui: &mut egui::Ui,
    draft: &mut Draft,
    snapshot: &StoreSnapshot,
    topics: &[(delog_core::identity::TopicId, String)],
) {
    form_grid(ui, "vehicle_grid_position", |ui| {
        form_row(ui, "Frame", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut draft.pos_mode, PosMode::Ned, "Local (NED)");
                ui.selectable_value(&mut draft.pos_mode, PosMode::Gps, "Global (GPS)");
            });
        });
        form_row(ui, "Topic", |ui| {
            if searchable_combo(ui, "veh-pos-topic", &mut draft.pos_topic, topics) {
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
        });

        let Some(topic) = draft.pos_topic else {
            return;
        };
        let cols = topic_fields(snapshot, topic);
        match draft.pos_mode {
            PosMode::Ned => {
                form_row(ui, "North (X)", |ui| {
                    field_combo(ui, "veh-n", &mut draft.north, &cols);
                });
                form_row(ui, "East (Y)", |ui| {
                    field_combo(ui, "veh-e", &mut draft.east, &cols);
                });
                form_row(ui, "Down (Z)", |ui| {
                    field_combo(ui, "veh-d", &mut draft.down, &cols);
                });
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
                        form_row(ui, "Ref Lat", |ui| {
                            field_combo(ui, "veh-rlat", &mut draft.ref_lat_f, &cols);
                        });
                        form_row(ui, "Ref Lon", |ui| {
                            field_combo(ui, "veh-rlon", &mut draft.ref_lon_f, &cols);
                        });
                        form_row(ui, "Ref Alt", |ui| {
                            field_combo(ui, "veh-ralt", &mut draft.ref_alt_f, &cols);
                        });
                    }
                }
            }
            PosMode::Gps => {
                form_row(ui, "Latitude", |ui| {
                    field_combo(ui, "veh-lat", &mut draft.lat, &cols);
                });
                form_row(ui, "Longitude", |ui| {
                    field_combo(ui, "veh-lon", &mut draft.lon, &cols);
                });
                form_row(ui, "Lat/Lon units", |ui| {
                    ui.checkbox(&mut draft.lat_lon_dege7, "degE7");
                });
                form_row(ui, "Altitude", |ui| {
                    field_combo(ui, "veh-alt", &mut draft.alt, &cols);
                });
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

fn orientation_section(
    ui: &mut egui::Ui,
    draft: &mut Draft,
    snapshot: &StoreSnapshot,
    topics: &[(delog_core::identity::TopicId, String)],
) {
    form_grid(ui, "vehicle_grid_orientation", |ui| {
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
        form_row(ui, "Topic", |ui| {
            if searchable_combo(ui, "veh-ori-topic", &mut draft.ori_topic, topics) {
                draft.roll = None;
                draft.pitch = None;
                draft.yaw = None;
                draft.qw = None;
                draft.qx = None;
                draft.qy = None;
                draft.qz = None;
            }
        });

        let Some(topic) = draft.ori_topic else {
            return;
        };
        let cols = topic_fields(snapshot, topic);
        match draft.ori_mode {
            OriMode::Static => {}
            OriMode::Euler => {
                form_row(ui, "Angle Unit", |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut draft.euler_degrees, true, "Degrees");
                        ui.selectable_value(&mut draft.euler_degrees, false, "Radians");
                    });
                });
                form_row(ui, "Roll", |ui| {
                    field_combo(ui, "veh-roll", &mut draft.roll, &cols);
                });
                form_row(ui, "Pitch", |ui| {
                    field_combo(ui, "veh-pitch", &mut draft.pitch, &cols);
                });
                form_row(ui, "Yaw", |ui| {
                    field_combo(ui, "veh-yaw", &mut draft.yaw, &cols);
                });
            }
            OriMode::Quat => {
                form_row(ui, "QW", |ui| {
                    field_combo(ui, "veh-qw", &mut draft.qw, &cols);
                });
                form_row(ui, "QX", |ui| {
                    field_combo(ui, "veh-qx", &mut draft.qx, &cols);
                });
                form_row(ui, "QY", |ui| {
                    field_combo(ui, "veh-qy", &mut draft.qy, &cols);
                });
                form_row(ui, "QZ", |ui| {
                    field_combo(ui, "veh-qz", &mut draft.qz, &cols);
                });
            }
        }
    });
}

fn apply_pending(state: &mut VehicleDialog, snapshot: &StoreSnapshot, pending: Pending) {
    if let Some(i) = pending.select {
        state.selected_vehicle = i;
    }
    if pending.add {
        let n = state.drafts.len() + 1;
        state.drafts.push(Draft {
            label: format!("Vehicle #{n}"),
            ..Draft::default()
        });
        state.selected_vehicle = state.drafts.len() - 1;
    }
    if let Some(i) = pending.duplicate {
        let mut copy = state.drafts[i].clone();
        copy.label = format!("{} copy", copy.label);
        state.drafts.insert(i + 1, copy);
        state.selected_vehicle = i + 1;
    }
    if let Some(i) = pending.remove {
        state.drafts.remove(i);
        state.clamp_selection();
    }
    if let Some(action) = pending.profile {
        handle_profile_action(action, state, snapshot);
    }
}
