//! Vehicle configuration dialog. There is no separate draft/Add/Save step —
//! one [`Draft`] per vehicle is edited live and the `vehicles` vector is
//! rebuilt from the drafts that resolve to a complete mapping.

use delog_core::identity::{FieldId, SourceId, TopicId};
use delog_core::snapshot::StoreSnapshot;
use egui::Color32;

use crate::vehicle::{GeoRef, ModelKind, NedReference, OriMapping, PosMapping, VehicleConfig};

/// Fixed dialog width, applied even when no vehicles are configured.
const DIALOG_WIDTH: f32 = 240.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PosMode {
    Ned,
    Gps,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OriMode {
    Static,
    Euler,
    Quat,
}

/// An editable vehicle form.
struct Draft {
    label: String,
    show: bool,
    source: Option<SourceId>,
    pos_topic: Option<TopicId>,
    pos_mode: PosMode,
    north: Option<FieldId>,
    east: Option<FieldId>,
    down: Option<FieldId>,
    lat: Option<FieldId>,
    lon: Option<FieldId>,
    alt: Option<FieldId>,
    /// Interpret the lat/lon columns as `degE7` integers (×1e-7 → degrees).
    lat_lon_dege7: bool,
    /// Interpret the altitude column as millimetres (×1e-3 → metres).
    alt_mm: bool,
    /// Fixed vertical offset in metres (up-positive) for the GPS track.
    alt_offset_m: f64,
    /// Optional geodetic reference annotation for a NED/local frame.
    ned_has_ref: bool,
    /// Reference from fixed values (true) or from columns (false).
    ned_ref_manual: bool,
    ref_lat: f64,
    ref_lon: f64,
    ref_alt: f64,
    ref_lat_f: Option<FieldId>,
    ref_lon_f: Option<FieldId>,
    ref_alt_f: Option<FieldId>,
    ori_topic: Option<TopicId>,
    ori_mode: OriMode,
    roll: Option<FieldId>,
    pitch: Option<FieldId>,
    yaw: Option<FieldId>,
    euler_degrees: bool,
    qw: Option<FieldId>,
    qx: Option<FieldId>,
    qy: Option<FieldId>,
    qz: Option<FieldId>,
    model: ModelKind,
    custom_path: String,
    color: Color32,
    path_color: Color32,
    scale: f32,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            label: "Vehicle".into(),
            show: true,
            source: None,
            pos_topic: None,
            pos_mode: PosMode::Ned,
            north: None,
            east: None,
            down: None,
            lat: None,
            lon: None,
            alt: None,
            lat_lon_dege7: false,
            alt_mm: false,
            alt_offset_m: 0.0,
            ned_has_ref: false,
            ned_ref_manual: false,
            ref_lat: 0.0,
            ref_lon: 0.0,
            ref_alt: 0.0,
            ref_lat_f: None,
            ref_lon_f: None,
            ref_alt_f: None,
            ori_topic: None,
            ori_mode: OriMode::Static,
            roll: None,
            pitch: None,
            yaw: None,
            euler_degrees: true,
            qw: None,
            qx: None,
            qy: None,
            qz: None,
            model: ModelKind::Cone,
            custom_path: String::new(),
            color: Color32::from_rgb(90, 170, 255),
            path_color: Color32::from_rgb(255, 170, 60),
            scale: 1.0,
        }
    }
}

impl Draft {
    /// Load an existing config for editing (FieldIds map back to their topic).
    fn from_config(cfg: &VehicleConfig, snapshot: &StoreSnapshot) -> Self {
        let topic_of = |f: FieldId| field_topic(snapshot, f);
        let mut d = Draft {
            label: cfg.label.clone(),
            show: cfg.show,
            source: Some(cfg.source),
            model: cfg.model.clone(),
            custom_path: match &cfg.model {
                ModelKind::CustomGlb(p) => p.to_string_lossy().into_owned(),
                _ => String::new(),
            },
            color: cfg.color,
            path_color: cfg.path_color,
            scale: cfg.scale,
            ..Draft::default()
        };
        match &cfg.pos {
            PosMapping::Ned {
                north,
                east,
                down,
                reference,
            } => {
                d.pos_mode = PosMode::Ned;
                d.pos_topic = topic_of(*north);
                d.north = Some(*north);
                d.east = Some(*east);
                d.down = Some(*down);
                match reference {
                    None => {}
                    Some(NedReference::Manual(r)) => {
                        d.ned_has_ref = true;
                        d.ned_ref_manual = true;
                        d.ref_lat = r.lat_deg;
                        d.ref_lon = r.lon_deg;
                        d.ref_alt = r.alt_m;
                    }
                    Some(NedReference::Fields { lat, lon, alt }) => {
                        d.ned_has_ref = true;
                        d.ned_ref_manual = false;
                        d.ref_lat_f = Some(*lat);
                        d.ref_lon_f = Some(*lon);
                        d.ref_alt_f = Some(*alt);
                    }
                }
            }
            PosMapping::Gps {
                lat,
                lon,
                alt,
                lat_lon_dege7,
                alt_mm,
                alt_offset_m,
            } => {
                d.pos_mode = PosMode::Gps;
                d.pos_topic = topic_of(*lat);
                d.lat = Some(*lat);
                d.lon = Some(*lon);
                d.alt = Some(*alt);
                d.lat_lon_dege7 = *lat_lon_dege7;
                d.alt_mm = *alt_mm;
                d.alt_offset_m = *alt_offset_m;
            }
        }
        match &cfg.ori {
            OriMapping::Static => d.ori_mode = OriMode::Static,
            OriMapping::Euler {
                roll,
                pitch,
                yaw,
                degrees,
            } => {
                d.ori_mode = OriMode::Euler;
                d.ori_topic = topic_of(*roll);
                d.roll = Some(*roll);
                d.pitch = Some(*pitch);
                d.yaw = Some(*yaw);
                d.euler_degrees = *degrees;
            }
            OriMapping::Quat { w, x, y, z } => {
                d.ori_mode = OriMode::Quat;
                d.ori_topic = topic_of(*w);
                d.qw = Some(*w);
                d.qx = Some(*x);
                d.qy = Some(*y);
                d.qz = Some(*z);
            }
        }
        d
    }

    /// Build a `VehicleConfig` if the selected mapping has all its fields.
    fn build(&self) -> Option<VehicleConfig> {
        let source = self.source?;
        let pos = match self.pos_mode {
            PosMode::Ned => PosMapping::Ned {
                north: self.north?,
                east: self.east?,
                down: self.down?,
                reference: if !self.ned_has_ref {
                    None
                } else if self.ned_ref_manual {
                    Some(NedReference::Manual(GeoRef {
                        lat_deg: self.ref_lat,
                        lon_deg: self.ref_lon,
                        alt_m: self.ref_alt,
                    }))
                } else {
                    // Column reference: kept only when all three are chosen.
                    match (self.ref_lat_f, self.ref_lon_f, self.ref_alt_f) {
                        (Some(lat), Some(lon), Some(alt)) => {
                            Some(NedReference::Fields { lat, lon, alt })
                        }
                        _ => None,
                    }
                },
            },
            PosMode::Gps => PosMapping::Gps {
                lat: self.lat?,
                lon: self.lon?,
                alt: self.alt?,
                lat_lon_dege7: self.lat_lon_dege7,
                alt_mm: self.alt_mm,
                alt_offset_m: self.alt_offset_m,
            },
        };
        let ori = match self.ori_mode {
            OriMode::Static => OriMapping::Static,
            OriMode::Euler => OriMapping::Euler {
                roll: self.roll?,
                pitch: self.pitch?,
                yaw: self.yaw?,
                degrees: self.euler_degrees,
            },
            OriMode::Quat => OriMapping::Quat {
                w: self.qw?,
                x: self.qx?,
                y: self.qy?,
                z: self.qz?,
            },
        };
        let model = if let ModelKind::CustomGlb(_) = self.model {
            ModelKind::CustomGlb(self.custom_path.clone().into())
        } else {
            self.model.clone()
        };
        Some(VehicleConfig {
            source,
            label: self.label.clone(),
            show: self.show,
            pos,
            ori,
            model,
            color: self.color,
            path_color: self.path_color,
            scale: self.scale.max(0.01),
        })
    }
}

/// Dialog state: open flag plus one editable draft per vehicle. `was_open`
/// drives a resync from `vehicles` on the open edge.
#[derive(Default)]
pub struct VehicleDialog {
    pub open: bool,
    drafts: Vec<Draft>,
    was_open: bool,
}

fn field_topic(snapshot: &StoreSnapshot, field: FieldId) -> Option<TopicId> {
    snapshot.fields.get(field.index()).map(|f| f.topic)
}

fn source_topics(snapshot: &StoreSnapshot, source: SourceId) -> Vec<(TopicId, String)> {
    let mut out = Vec::new();
    for src in snapshot.sources.iter() {
        if src.entry.id != source || src.entry.removed {
            continue;
        }
        for &topic_id in src.topics.iter() {
            if let Some(topic) = snapshot.topic(topic_id)
                && !topic.entry.removed
            {
                out.push((topic_id, topic.entry.name.clone()));
            }
        }
    }
    out.sort_by_key(|(_, name)| name.to_ascii_lowercase());
    out
}

fn topic_fields(snapshot: &StoreSnapshot, topic: TopicId) -> Vec<(FieldId, String)> {
    snapshot
        .fields
        .iter()
        .filter(|f| f.topic == topic && !f.removed)
        .map(|f| (f.id, f.name.clone()))
        .collect()
}

fn combo_label<'a, T: PartialEq>(items: &'a [(T, String)], sel: &Option<T>) -> &'a str {
    match sel {
        Some(s) => items
            .iter()
            .find(|(v, _)| v == s)
            .map(|(_, l)| l.as_str())
            .unwrap_or("—"),
        None => "—",
    }
}

/// A **searchable** picker over `(value, label)` items, rendered as a single
/// toggle button (for use as a grid cell — the caller supplies the label).
/// The dropdown carries a text filter (persisted in egui memory) so long
/// topic / field lists are easy to narrow. Returns `true` if the selection
/// changed. Ids are derived from the calling `ui` so repeated salts across
/// several vehicles do not collide.
fn searchable_combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut Option<T>,
    items: &[(T, String)],
) -> bool {
    let before = *sel;
    let filter_id = ui.make_persistent_id((salt, "filter"));
    let highlight_id = ui.make_persistent_id((salt, "highlight"));
    // A toggle button + an explicit popup: `CloseOnClickOutside` keeps the
    // popup open while typing in the search box (a plain ComboBox closes on
    // that click), and the scroll area shows many rows at once.
    let button = ui.button(combo_label(items, sel));
    egui::Popup::from_toggle_button_response(&button)
        .id(button.id.with("popup"))
        .width(170.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_max_width(170.0);
            {
                let mut filter: String =
                    ui.memory_mut(|m| m.data.get_temp(filter_id).unwrap_or_default());
                let response = ui.add(
                    egui::TextEdit::singleline(&mut filter)
                        .hint_text("search…")
                        .desired_width(f32::INFINITY),
                );
                response.request_focus();
                let needle = filter.to_ascii_lowercase();
                ui.memory_mut(|m| m.data.insert_temp(filter_id, filter));
                let visible = items
                    .iter()
                    .filter(|(_, name)| {
                        needle.is_empty() || name.to_ascii_lowercase().contains(&needle)
                    })
                    .map(|(value, name)| (*value, name.as_str()))
                    .collect::<Vec<_>>();
                let mut highlighted = ui
                    .memory_mut(|m| m.data.get_temp::<usize>(highlight_id))
                    .unwrap_or_else(|| {
                        visible
                            .iter()
                            .position(|(value, _)| *sel == Some(*value))
                            .unwrap_or(0)
                    });
                if !visible.is_empty() {
                    highlighted = highlighted.min(visible.len() - 1);
                    let move_down = ui
                        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
                    let move_up =
                        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
                    let choose =
                        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                    if move_down {
                        highlighted = (highlighted + 1).min(visible.len() - 1);
                    }
                    if move_up {
                        highlighted = highlighted.saturating_sub(1);
                    }
                    if choose {
                        *sel = Some(visible[highlighted].0);
                        ui.close();
                    }
                }
                ui.memory_mut(|m| m.data.insert_temp(highlight_id, highlighted));
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for (i, (value, name)) in visible.iter().enumerate() {
                            let selected = *sel == Some(*value) || i == highlighted;
                            let response = ui.selectable_label(selected, *name);
                            if i == highlighted {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.clicked() {
                                ui.memory_mut(|m| m.data.insert_temp(highlight_id, i));
                                *sel = Some(*value);
                                ui.close();
                            }
                        }
                    });
            }
        });
    *sel != before
}

/// A plain field picker restricted to one topic's columns (no search — a
/// single topic has few columns), rendered as a single combo for a grid cell.
fn field_combo(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut Option<FieldId>,
    fields: &[(FieldId, String)],
) {
    egui::ComboBox::from_id_salt(salt)
        .selected_text(combo_label(fields, sel))
        .show_ui(ui, |ui| {
            for (id, name) in fields {
                ui.selectable_value(sel, Some(*id), name);
            }
        });
}

/// Render the dialog; returns `true` when the vehicle set changed.
pub fn show(
    ctx: &egui::Context,
    state: &mut VehicleDialog,
    vehicles: &mut Vec<VehicleConfig>,
    snapshot: &StoreSnapshot,
) -> bool {
    // Resync drafts from the current vehicles on the open edge, so external
    // changes (e.g. a loaded layout) are reflected when the dialog is opened.
    if state.open && !state.was_open {
        state.drafts = vehicles
            .iter()
            .map(|v| Draft::from_config(v, snapshot))
            .collect();
    }
    state.was_open = state.open;
    if !state.open {
        return false;
    }

    let mut open = state.open;
    egui::Window::new("Vehicles")
        .open(&mut open)
        .collapsible(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .default_width(DIALOG_WIDTH)
        .show(ctx, |ui| {
            // Keep a stable, full width even with no vehicles configured.
            ui.set_min_width(DIALOG_WIDTH);
            if ui
                .add(egui::Button::image_and_text(
                    icon(ui, crate::icons::plus()),
                    "Add Vehicle",
                ))
                .clicked()
            {
                // Default the name to "Vehicle #N"; the user can rename it.
                let n = state.drafts.len() + 1;
                state.drafts.push(Draft {
                    label: format!("Vehicle #{n}"),
                    ..Draft::default()
                });
            }
            ui.add_space(8.0);

            let mut remove: Option<usize> = None;
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, draft) in state.drafts.iter_mut().enumerate() {
                    // The header shows the editable name, falling back to the
                    // positional default when the user clears it.
                    let title = if draft.label.trim().is_empty() {
                        format!("Vehicle #{}", i + 1)
                    } else {
                        draft.label.clone()
                    };
                    egui::CollapsingHeader::new(title)
                        .id_salt(("vehicle", i))
                        .default_open(true)
                        .show(ui, |ui| {
                            draft_editor(ui, draft, snapshot);
                            ui.add_space(8.0);
                            if ui
                                .add(egui::Button::image_and_text(
                                    icon(ui, crate::icons::trash()),
                                    "Remove Vehicle",
                                ))
                                .clicked()
                            {
                                remove = Some(i);
                            }
                        });
                    ui.add_space(6.0);
                }
            });
            if let Some(i) = remove {
                state.drafts.remove(i);
            }
        });
    state.open = open;

    // Rebuild the vehicle set from the drafts that resolve to a complete
    // mapping. Commit whenever it differs so cosmetic edits (colour, scale,
    // model, visibility) show in the 3D view immediately, but only report a
    // change — which drives the off-thread trajectory rebuild — when a
    // trajectory-relevant aspect (source or position mapping) actually moves.
    let rebuilt: Vec<VehicleConfig> = state.drafts.iter().filter_map(Draft::build).collect();
    if rebuilt == *vehicles {
        return false;
    }
    let traj_changed = rebuilt
        .iter()
        .map(|v| (v.source, &v.pos))
        .ne(vehicles.iter().map(|v| (v.source, &v.pos)));
    *vehicles = rebuilt;
    traj_changed
}

/// A 16px menu/button icon tinted to the current text colour.
fn icon(ui: &egui::Ui, src: egui::ImageSource<'static>) -> egui::Image<'static> {
    egui::Image::new(src)
        .fit_to_exact_size(egui::vec2(16.0, 16.0))
        .tint(ui.visuals().text_color())
}

/// A bold section heading separating the General / Orientation / Position
/// groups within a vehicle's editor.
fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).strong());
    ui.add_space(2.0);
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
                ui.text_edit_singleline(&mut draft.custom_path);
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

    // Field mapping needs a source; until one is chosen there is nothing more
    // to show.
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

/// One grid row: a label cell and a single-topic column picker, then `end_row`.
fn grid_field(
    ui: &mut egui::Ui,
    salt: &str,
    label: &str,
    sel: &mut Option<FieldId>,
    cols: &[(FieldId, String)],
) {
    ui.label(label);
    field_combo(ui, salt, sel, cols);
    ui.end_row();
}

/// A searchable topic picker for a grid cell; returns `true` if the selection
/// changed (so the caller can clear the now-stale column selections).
fn topic_combo(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut Option<TopicId>,
    topics: &[(TopicId, String)],
) -> bool {
    searchable_combo(ui, salt, sel, topics)
}
