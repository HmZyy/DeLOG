//! Vehicle configuration dialog (PLAN.md §12.1, TDV-03). Lists configured
//! vehicles (show / edit / remove) and edits a draft: pick a source, then a
//! position topic and an orientation topic, then the per-axis columns from
//! those topics. An auto-detect button fills the draft from common
//! ArduPilot/PX4/MAVLink field names. Building a [`VehicleConfig`] is where
//! every mapping variant is constructed.

use delog_core::identity::{FieldId, SourceId, TopicId};
use delog_core::snapshot::StoreSnapshot;
use egui::Color32;

use crate::vehicle::{GeoRef, ModelKind, NedReference, OriMapping, PosMapping, VehicleConfig};

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
    source: Option<SourceId>,
    pos_topic: Option<TopicId>,
    pos_mode: PosMode,
    north: Option<FieldId>,
    east: Option<FieldId>,
    down: Option<FieldId>,
    lat: Option<FieldId>,
    lon: Option<FieldId>,
    alt: Option<FieldId>,
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
            source: None,
            pos_topic: None,
            pos_mode: PosMode::Ned,
            north: None,
            east: None,
            down: None,
            lat: None,
            lon: None,
            alt: None,
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
            PosMapping::Gps { lat, lon, alt } => {
                d.pos_mode = PosMode::Gps;
                d.pos_topic = topic_of(*lat);
                d.lat = Some(*lat);
                d.lon = Some(*lon);
                d.alt = Some(*alt);
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
            show: true,
            pos,
            ori,
            model,
            color: self.color,
            path_color: self.path_color,
            scale: self.scale.max(0.01),
        })
    }
}

/// Dialog state: open flag, the working draft, and which vehicle is being
/// edited (`None` = adding a new one).
#[derive(Default)]
pub struct VehicleDialog {
    pub open: bool,
    draft: Draft,
    editing: Option<usize>,
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

/// A labelled, **searchable** ComboBox over `(value, label)` items. The
/// dropdown carries a text filter (persisted in egui memory) so long topic /
/// field lists are easy to narrow. Returns `true` if the selection changed.
fn searchable_combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    salt: &str,
    label: &str,
    sel: &mut Option<T>,
    items: &[(T, String)],
) -> bool {
    let before = *sel;
    ui.horizontal(|ui| {
        ui.label(label);
        // A toggle button + an explicit popup: `CloseOnClickOutside` keeps the
        // popup open while typing in the search box (a plain ComboBox closes on
        // that click), and the scroll area shows many rows at once.
        let button = ui.button(combo_label(items, sel));
        egui::Popup::from_toggle_button_response(&button)
            .id(egui::Id::new((salt, "popup")))
            .width(170.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_max_width(170.0);
                let filter_id = egui::Id::new((salt, "filter"));
                let highlight_id = egui::Id::new((salt, "highlight"));
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
            });
    });
    *sel != before
}

/// A plain field picker restricted to one topic's columns (no search — a
/// single topic has few columns).
fn field_combo(
    ui: &mut egui::Ui,
    salt: &str,
    label: &str,
    sel: &mut Option<FieldId>,
    fields: &[(FieldId, String)],
) {
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(salt)
            .selected_text(combo_label(fields, sel))
            .show_ui(ui, |ui| {
                for (id, name) in fields {
                    ui.selectable_value(sel, Some(*id), name);
                }
            });
    });
}

/// Render the dialog; returns `true` when the vehicle set changed.
pub fn show(
    ctx: &egui::Context,
    state: &mut VehicleDialog,
    vehicles: &mut Vec<VehicleConfig>,
    snapshot: &StoreSnapshot,
) -> bool {
    if !state.open {
        return false;
    }
    let mut open = state.open;
    let mut changed = false;
    egui::Window::new("Vehicles")
        .open(&mut open)
        .default_width(380.0)
        .show(ctx, |ui| {
            let mut remove: Option<usize> = None;
            for (i, v) in vehicles.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut v.show, "");
                    ui.label(&v.label);
                    ui.weak(v.model.label());
                    if ui.small_button("Edit").clicked() {
                        state.editing = Some(i);
                        state.draft = Draft::from_config(v, snapshot);
                    }
                    if ui.small_button("✕").on_hover_text("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                vehicles.remove(i);
                if state.editing == Some(i) {
                    state.editing = None;
                    state.draft = Draft::default();
                }
                changed = true;
            }
            ui.separator();

            ui.horizontal(|ui| {
                ui.label(if state.editing.is_some() {
                    "Editing vehicle"
                } else {
                    "New vehicle"
                });
                if ui.small_button("Clear").clicked() {
                    state.editing = None;
                    state.draft = Draft::default();
                }
            });
            draft_editor(ui, &mut state.draft, snapshot);

            ui.separator();
            let can_save = state.draft.build().is_some();
            let label = if state.editing.is_some() {
                "Save changes"
            } else {
                "Add vehicle"
            };
            ui.horizontal(|ui| {
                if ui.add_enabled(can_save, egui::Button::new(label)).clicked()
                    && let Some(cfg) = state.draft.build()
                {
                    match state.editing {
                        Some(i) if i < vehicles.len() => vehicles[i] = cfg,
                        _ => vehicles.push(cfg),
                    }
                    state.editing = None;
                    state.draft = Draft::default();
                    changed = true;
                }
                if !can_save {
                    ui.weak("pick a source, position topic + columns");
                }
            });
        });
    state.open = open;
    changed
}

fn draft_editor(ui: &mut egui::Ui, draft: &mut Draft, snapshot: &StoreSnapshot) {
    let sources: Vec<(SourceId, String)> = snapshot
        .sources
        .iter()
        .filter(|s| !s.entry.removed)
        .map(|s| (s.entry.id, s.entry.label.clone()))
        .collect();
    ui.horizontal(|ui| {
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
                            model: draft.model.clone(),
                            color: draft.color,
                            path_color: draft.path_color,
                            scale: draft.scale,
                            ..Draft::default()
                        };
                    }
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label("Label");
        ui.text_edit_singleline(&mut draft.label);
    });

    let Some(source) = draft.source else {
        ui.weak("Select a source to map its fields.");
        return;
    };
    let topics = source_topics(snapshot, source);

    // Position: pick a topic, then columns from it.
    ui.separator();
    ui.label("Position");
    ui.horizontal(|ui| {
        ui.selectable_value(&mut draft.pos_mode, PosMode::Ned, "NED / local");
        ui.selectable_value(&mut draft.pos_mode, PosMode::Gps, "GPS");
    });
    if topic_combo(ui, "veh-pos-topic", "Topic", &mut draft.pos_topic, &topics) {
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
    if let Some(topic) = draft.pos_topic {
        let cols = topic_fields(snapshot, topic);
        match draft.pos_mode {
            PosMode::Ned => {
                field_combo(ui, "veh-n", "North", &mut draft.north, &cols);
                field_combo(ui, "veh-e", "East", &mut draft.east, &cols);
                field_combo(ui, "veh-d", "Down", &mut draft.down, &cols);
                ui.checkbox(&mut draft.ned_has_ref, "Reference origin");
                if draft.ned_has_ref {
                    ui.checkbox(&mut draft.ned_ref_manual, "Manual (fixed values)");
                    if draft.ned_ref_manual {
                        ui.horizontal(|ui| {
                            ui.label("ref lat/lon/alt");
                            ui.add(egui::DragValue::new(&mut draft.ref_lat).speed(0.0001));
                            ui.add(egui::DragValue::new(&mut draft.ref_lon).speed(0.0001));
                            ui.add(egui::DragValue::new(&mut draft.ref_alt).speed(0.1));
                        });
                    } else {
                        field_combo(ui, "veh-rlat", "ref Lat", &mut draft.ref_lat_f, &cols);
                        field_combo(ui, "veh-rlon", "ref Lon", &mut draft.ref_lon_f, &cols);
                        field_combo(ui, "veh-ralt", "ref Alt", &mut draft.ref_alt_f, &cols);
                    }
                }
            }
            PosMode::Gps => {
                field_combo(ui, "veh-lat", "Lat", &mut draft.lat, &cols);
                field_combo(ui, "veh-lon", "Lon", &mut draft.lon, &cols);
                field_combo(ui, "veh-alt", "Alt", &mut draft.alt, &cols);
            }
        }
    } else {
        ui.weak("Select a position topic.");
    }

    // Orientation: pick a topic, then columns from it.
    ui.separator();
    ui.label("Orientation");
    ui.horizontal(|ui| {
        ui.selectable_value(&mut draft.ori_mode, OriMode::Static, "Static");
        ui.selectable_value(&mut draft.ori_mode, OriMode::Euler, "Euler");
        ui.selectable_value(&mut draft.ori_mode, OriMode::Quat, "Quaternion");
    });
    if draft.ori_mode != OriMode::Static {
        if topic_combo(ui, "veh-ori-topic", "Topic", &mut draft.ori_topic, &topics) {
            draft.roll = None;
            draft.pitch = None;
            draft.yaw = None;
            draft.qw = None;
            draft.qx = None;
            draft.qy = None;
            draft.qz = None;
        }
        if let Some(topic) = draft.ori_topic {
            let cols = topic_fields(snapshot, topic);
            match draft.ori_mode {
                OriMode::Static => {}
                OriMode::Euler => {
                    field_combo(ui, "veh-roll", "Roll", &mut draft.roll, &cols);
                    field_combo(ui, "veh-pitch", "Pitch", &mut draft.pitch, &cols);
                    field_combo(ui, "veh-yaw", "Yaw", &mut draft.yaw, &cols);
                    ui.horizontal(|ui| {
                        ui.label("Angle unit");
                        ui.selectable_value(&mut draft.euler_degrees, true, "Degrees");
                        ui.selectable_value(&mut draft.euler_degrees, false, "Radians");
                    });
                }
                OriMode::Quat => {
                    field_combo(ui, "veh-qw", "W", &mut draft.qw, &cols);
                    field_combo(ui, "veh-qx", "X", &mut draft.qx, &cols);
                    field_combo(ui, "veh-qy", "Y", &mut draft.qy, &cols);
                    field_combo(ui, "veh-qz", "Z", &mut draft.qz, &cols);
                }
            }
        } else {
            ui.weak("Select an orientation topic.");
        }
    }

    // Appearance.
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Model");
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
    });
    if matches!(draft.model, ModelKind::CustomGlb(_)) {
        ui.horizontal(|ui| {
            ui.label("GLB path");
            ui.text_edit_singleline(&mut draft.custom_path);
        });
    }
    ui.horizontal(|ui| {
        ui.label("Body");
        ui.color_edit_button_srgba(&mut draft.color);
        ui.label("Path");
        ui.color_edit_button_srgba(&mut draft.path_color);
        ui.label("Scale");
        ui.add(
            egui::DragValue::new(&mut draft.scale)
                .speed(0.05)
                .range(0.05..=50.0),
        );
    });
}

/// A searchable topic ComboBox; returns `true` if the selection changed (so the
/// caller can clear column selections that belonged to the previous topic).
fn topic_combo(
    ui: &mut egui::Ui,
    salt: &str,
    label: &str,
    sel: &mut Option<TopicId>,
    topics: &[(TopicId, String)],
) -> bool {
    searchable_combo(ui, salt, label, sel, topics)
}
