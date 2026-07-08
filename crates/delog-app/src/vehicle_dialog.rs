use delog_core::identity::{FieldId, SourceId, TopicId};
use delog_core::snapshot::StoreSnapshot;
use egui::Color32;

use crate::layout::{FieldRef, ModelLayout, NedRefLayout, OriLayout, PosLayout, VehicleLayout};
use crate::logging::{LogLevel, PendingLog, log};
use crate::vehicle::{GeoRef, ModelKind, NedReference, OriMapping, PosMapping, VehicleConfig};
use crate::vehicle_profiles::{VEHICLE_PROFILE_VERSION, VehicleProfileDoc, VehicleProfileLibrary};

const DIALOG_WIDTH: f32 = 240.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum VehicleDialogTab {
    Vehicles,
    Profiles,
}

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

#[derive(Clone)]
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
    /// degE7 integers (×1e-7 → degrees).
    lat_lon_dege7: bool,
    /// millimetres (×1e-3 → metres).
    alt_mm: bool,
    /// metres, up-positive.
    alt_offset_m: f64,
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
    selected_profile: Option<String>,
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
            model: ModelKind::FixedWing,
            custom_path: String::new(),
            color: Color32::from_rgb(90, 170, 255),
            path_color: Color32::from_rgb(255, 170, 60),
            scale: 1.0,
            selected_profile: None,
        }
    }
}

impl Draft {
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

    #[allow(dead_code)]
    fn apply_config_preserving_label(&mut self, cfg: &VehicleConfig, snapshot: &StoreSnapshot) {
        let previous_label = self.label.clone();
        *self = Draft::from_config(cfg, snapshot);
        self.label = previous_label;
    }

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

#[derive(Clone)]
struct ProfileDraft {
    label: String,
    show: bool,
    pos_mode: PosMode,
    pos_topic: String,
    north: String,
    east: String,
    down: String,
    lat: String,
    lon: String,
    alt: String,
    lat_lon_dege7: bool,
    alt_mm: bool,
    alt_offset_m: f64,
    ned_has_ref: bool,
    ned_ref_manual: bool,
    ref_lat: f64,
    ref_lon: f64,
    ref_alt: f64,
    ref_lat_f: String,
    ref_lon_f: String,
    ref_alt_f: String,
    ori_mode: OriMode,
    ori_topic: String,
    roll: String,
    pitch: String,
    yaw: String,
    euler_degrees: bool,
    qw: String,
    qx: String,
    qy: String,
    qz: String,
    model: ModelKind,
    custom_path: String,
    color: Color32,
    path_color: Color32,
    scale: f32,
}

impl Default for ProfileDraft {
    fn default() -> Self {
        Self {
            label: "Vehicle".to_owned(),
            show: true,
            pos_mode: PosMode::Gps,
            pos_topic: "GLOBAL_POSITION_INT".to_owned(),
            north: String::new(),
            east: String::new(),
            down: String::new(),
            lat: "lat".to_owned(),
            lon: "lon".to_owned(),
            alt: "alt".to_owned(),
            lat_lon_dege7: true,
            alt_mm: true,
            alt_offset_m: 0.0,
            ned_has_ref: false,
            ned_ref_manual: false,
            ref_lat: 0.0,
            ref_lon: 0.0,
            ref_alt: 0.0,
            ref_lat_f: String::new(),
            ref_lon_f: String::new(),
            ref_alt_f: String::new(),
            ori_mode: OriMode::Static,
            ori_topic: String::new(),
            roll: String::new(),
            pitch: String::new(),
            yaw: String::new(),
            euler_degrees: true,
            qw: String::new(),
            qx: String::new(),
            qy: String::new(),
            qz: String::new(),
            model: ModelKind::FixedWing,
            custom_path: String::new(),
            color: Color32::from_rgb(90, 170, 255),
            path_color: Color32::from_rgb(255, 170, 60),
            scale: 1.0,
        }
    }
}

impl ProfileDraft {
    fn from_doc(doc: &VehicleProfileDoc) -> Self {
        let vehicle = &doc.vehicle;
        let mut draft = Self {
            label: vehicle.label.clone(),
            show: vehicle.show,
            model: profile_model_from_layout(&vehicle.model),
            custom_path: match &vehicle.model {
                ModelLayout::CustomGlb { path } => path.clone(),
                _ => String::new(),
            },
            color: rgba_to_color(vehicle.color),
            path_color: rgba_to_color(vehicle.path_color),
            scale: vehicle.scale,
            ..Self::default()
        };

        match &vehicle.position {
            PosLayout::Ned {
                north,
                east,
                down,
                reference,
            } => {
                draft.pos_mode = PosMode::Ned;
                draft.pos_topic = north.topic.clone();
                draft.north = north.field.clone();
                draft.east = east.field.clone();
                draft.down = down.field.clone();
                match reference {
                    None => {}
                    Some(NedRefLayout::Manual {
                        lat_deg,
                        lon_deg,
                        alt_m,
                    }) => {
                        draft.ned_has_ref = true;
                        draft.ned_ref_manual = true;
                        draft.ref_lat = *lat_deg;
                        draft.ref_lon = *lon_deg;
                        draft.ref_alt = *alt_m;
                    }
                    Some(NedRefLayout::Fields { lat, lon, alt }) => {
                        draft.ned_has_ref = true;
                        draft.ned_ref_manual = false;
                        draft.ref_lat_f = lat.field.clone();
                        draft.ref_lon_f = lon.field.clone();
                        draft.ref_alt_f = alt.field.clone();
                    }
                }
            }
            PosLayout::Gps {
                lat,
                lon,
                alt,
                lat_lon_dege7,
                alt_mm,
                alt_offset_m,
            } => {
                draft.pos_mode = PosMode::Gps;
                draft.pos_topic = lat.topic.clone();
                draft.lat = lat.field.clone();
                draft.lon = lon.field.clone();
                draft.alt = alt.field.clone();
                draft.lat_lon_dege7 = *lat_lon_dege7;
                draft.alt_mm = *alt_mm;
                draft.alt_offset_m = *alt_offset_m;
            }
        }

        match &vehicle.orientation {
            OriLayout::Static => draft.ori_mode = OriMode::Static,
            OriLayout::Euler {
                roll,
                pitch,
                yaw,
                degrees,
            } => {
                draft.ori_mode = OriMode::Euler;
                draft.ori_topic = roll.topic.clone();
                draft.roll = roll.field.clone();
                draft.pitch = pitch.field.clone();
                draft.yaw = yaw.field.clone();
                draft.euler_degrees = *degrees;
            }
            OriLayout::Quat { w, x, y, z } => {
                draft.ori_mode = OriMode::Quat;
                draft.ori_topic = w.topic.clone();
                draft.qw = w.field.clone();
                draft.qx = x.field.clone();
                draft.qy = y.field.clone();
                draft.qz = z.field.clone();
            }
        }

        draft
    }

    fn to_doc(&self, name: &str) -> Result<VehicleProfileDoc, String> {
        let position = match self.pos_mode {
            PosMode::Ned => PosLayout::Ned {
                north: profile_field_ref(&self.pos_topic, &self.north, "north")?,
                east: profile_field_ref(&self.pos_topic, &self.east, "east")?,
                down: profile_field_ref(&self.pos_topic, &self.down, "down")?,
                reference: if !self.ned_has_ref {
                    None
                } else if self.ned_ref_manual {
                    Some(NedRefLayout::Manual {
                        lat_deg: self.ref_lat,
                        lon_deg: self.ref_lon,
                        alt_m: self.ref_alt,
                    })
                } else {
                    Some(NedRefLayout::Fields {
                        lat: profile_field_ref(&self.pos_topic, &self.ref_lat_f, "ref latitude")?,
                        lon: profile_field_ref(&self.pos_topic, &self.ref_lon_f, "ref longitude")?,
                        alt: profile_field_ref(&self.pos_topic, &self.ref_alt_f, "ref altitude")?,
                    })
                },
            },
            PosMode::Gps => PosLayout::Gps {
                lat: profile_field_ref(&self.pos_topic, &self.lat, "latitude")?,
                lon: profile_field_ref(&self.pos_topic, &self.lon, "longitude")?,
                alt: profile_field_ref(&self.pos_topic, &self.alt, "altitude")?,
                lat_lon_dege7: self.lat_lon_dege7,
                alt_mm: self.alt_mm,
                alt_offset_m: self.alt_offset_m,
            },
        };
        let orientation = match self.ori_mode {
            OriMode::Static => OriLayout::Static,
            OriMode::Euler => OriLayout::Euler {
                roll: profile_field_ref(&self.ori_topic, &self.roll, "roll")?,
                pitch: profile_field_ref(&self.ori_topic, &self.pitch, "pitch")?,
                yaw: profile_field_ref(&self.ori_topic, &self.yaw, "yaw")?,
                degrees: self.euler_degrees,
            },
            OriMode::Quat => OriLayout::Quat {
                w: profile_field_ref(&self.ori_topic, &self.qw, "quaternion w")?,
                x: profile_field_ref(&self.ori_topic, &self.qx, "quaternion x")?,
                y: profile_field_ref(&self.ori_topic, &self.qy, "quaternion y")?,
                z: profile_field_ref(&self.ori_topic, &self.qz, "quaternion z")?,
            },
        };

        Ok(VehicleProfileDoc {
            delog_vehicle_profile: VEHICLE_PROFILE_VERSION,
            name: name.trim().to_owned(),
            vehicle: VehicleLayout {
                label: self.label.clone(),
                show: self.show,
                model: profile_model_to_layout(&self.model, &self.custom_path),
                color: color_to_rgba(self.color),
                path_color: color_to_rgba(self.path_color),
                scale: self.scale.max(0.01),
                position,
                orientation,
            },
        })
    }
}

pub struct VehicleDialog {
    pub open: bool,
    drafts: Vec<Draft>,
    was_open: bool,
    selected_tab: VehicleDialogTab,
    profiles: Vec<String>,
    profile_editor_selected: Option<String>,
    profile_editor_name: String,
    profile_editor_draft: ProfileDraft,
    pending_profile_delete: Option<String>,
    pending_logs: Vec<PendingLog>,
}

impl Default for VehicleDialog {
    fn default() -> Self {
        Self {
            open: false,
            drafts: Vec::new(),
            was_open: false,
            selected_tab: VehicleDialogTab::Vehicles,
            profiles: Vec::new(),
            profile_editor_selected: None,
            profile_editor_name: String::new(),
            profile_editor_draft: ProfileDraft::default(),
            pending_profile_delete: None,
            pending_logs: Vec::new(),
        }
    }
}

impl VehicleDialog {
    pub fn take_logs(&mut self) -> Vec<PendingLog> {
        std::mem::take(&mut self.pending_logs)
    }
}

#[track_caller]
fn log_profile(state: &mut VehicleDialog, level: LogLevel, message: impl Into<String>) {
    state.pending_logs.push(log(level, message));
}

enum ProfileAction {
    Apply { draft: usize, name: String },
    Delete(String),
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

fn profile_library() -> Option<VehicleProfileLibrary> {
    VehicleProfileLibrary::from_config_dir()
}

fn refresh_profiles(state: &mut VehicleDialog) {
    let Some(library) = profile_library() else {
        log_profile(
            state,
            LogLevel::Warning,
            "vehicle profile config directory is unavailable",
        );
        state.profiles.clear();
        for draft in &mut state.drafts {
            draft.selected_profile = None;
        }
        return;
    };

    match library.list() {
        Ok(profiles) => {
            state.profiles = profiles;
            if state
                .profile_editor_selected
                .as_ref()
                .is_some_and(|selected| !state.profiles.contains(selected))
            {
                state.profile_editor_selected = None;
                state.profile_editor_name.clear();
                state.profile_editor_draft = ProfileDraft::default();
            }
            for draft in &mut state.drafts {
                if draft
                    .selected_profile
                    .as_ref()
                    .is_some_and(|selected| !state.profiles.contains(selected))
                {
                    draft.selected_profile = None;
                }
            }
        }
        Err(err) => {
            log_profile(
                state,
                LogLevel::Error,
                format!("failed to list vehicle profiles: {err}"),
            );
            state.profiles.clear();
            state.profile_editor_selected = None;
            for draft in &mut state.drafts {
                draft.selected_profile = None;
            }
        }
    }
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

/// Returns `true` if the selection changed. Ids are derived from the calling
/// `ui` so repeated salts across several vehicles do not collide.
fn searchable_combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut Option<T>,
    items: &[(T, String)],
) -> bool {
    let before = *sel;
    let filter_id = ui.make_persistent_id((salt, "filter"));
    let highlight_id = ui.make_persistent_id((salt, "highlight"));
    // `CloseOnClickOutside` keeps the popup open while typing in the search box;
    // a plain ComboBox closes on that click.
    let button = ui.button(combo_label(items, sel));
    egui::Popup::from_toggle_button_response(&button)
        .id(button.id.with("popup"))
        .width(170.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(170.0);
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
                let filter_changed = response.changed();
                let needle = filter.to_ascii_lowercase();
                ui.memory_mut(|m| m.data.insert_temp(filter_id, filter));
                let visible = items
                    .iter()
                    .filter(|(_, name)| {
                        needle.is_empty() || name.to_ascii_lowercase().contains(&needle)
                    })
                    .map(|(value, name)| (*value, name.as_str()))
                    .collect::<Vec<_>>();
                let stored_highlight = ui.memory_mut(|m| m.data.get_temp::<usize>(highlight_id));
                let initialized_highlight = stored_highlight.is_none();
                let mut highlighted = stored_highlight.unwrap_or_else(|| {
                    visible
                        .iter()
                        .position(|(value, _)| *sel == Some(*value))
                        .unwrap_or(0)
                });
                let mut highlight_changed_by_keyboard = false;
                if !visible.is_empty() {
                    highlighted = highlighted.min(visible.len() - 1);
                    let move_down = ui
                        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
                    let move_up =
                        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
                    let choose =
                        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                    if move_down {
                        let next = (highlighted + 1).min(visible.len() - 1);
                        highlight_changed_by_keyboard |= next != highlighted;
                        highlighted = next;
                    }
                    if move_up {
                        let next = highlighted.saturating_sub(1);
                        highlight_changed_by_keyboard |= next != highlighted;
                        highlighted = next;
                    }
                    if choose {
                        *sel = Some(visible[highlighted].0);
                        ui.close();
                    }
                }
                ui.memory_mut(|m| m.data.insert_temp(highlight_id, highlighted));
                let scroll_to_highlight =
                    highlight_changed_by_keyboard || initialized_highlight || filter_changed;
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        for (i, (value, name)) in visible.iter().enumerate() {
                            let selected = *sel == Some(*value) || i == highlighted;
                            let response = ui.selectable_label(selected, *name);
                            if scroll_to_highlight && i == highlighted {
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

fn choose_custom_glb_path(current_path: &str) -> Option<String> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("Choose custom GLB")
        .add_filter("GLB models", &["glb", "GLB"])
        .add_filter("All files", &["*"]);
    let current = std::path::Path::new(current_path.trim());
    if let Some(parent) = current.parent()
        && !parent.as_os_str().is_empty()
    {
        dialog = dialog.set_directory(parent);
    }
    dialog
        .pick_file()
        .map(|path| path.to_string_lossy().into_owned())
}

/// Returns `true` when the vehicle set changed.
pub fn show(
    ctx: &egui::Context,
    state: &mut VehicleDialog,
    vehicles: &mut Vec<VehicleConfig>,
    snapshot: &StoreSnapshot,
) -> bool {
    // Resync drafts on the open edge so external changes (e.g. a loaded layout)
    // are reflected when the dialog opens.
    if state.open && !state.was_open {
        state.drafts = vehicles
            .iter()
            .map(|v| Draft::from_config(v, snapshot))
            .collect();
        refresh_profiles(state);
    }
    state.was_open = state.open;
    if !state.open {
        state.pending_profile_delete = None;
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
            ui.set_min_width(DIALOG_WIDTH);
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut state.selected_tab,
                    VehicleDialogTab::Vehicles,
                    "Vehicle Config",
                );
                ui.selectable_value(
                    &mut state.selected_tab,
                    VehicleDialogTab::Profiles,
                    "Profiles",
                );
            });
            ui.separator();

            match state.selected_tab {
                VehicleDialogTab::Vehicles => show_vehicle_config_tab(ui, state, snapshot),
                VehicleDialogTab::Profiles => show_profiles_tab(ui, state, snapshot),
            }
        });
    show_profile_delete_confirmation(ctx, state);
    state.open = open;
    if !state.open {
        state.pending_profile_delete = None;
    }

    // Commit on any diff so cosmetic edits show immediately, but only report a
    // change (which drives the off-thread trajectory rebuild) when source or
    // position mapping moves.
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

fn show_vehicle_config_tab(ui: &mut egui::Ui, state: &mut VehicleDialog, snapshot: &StoreSnapshot) {
    if ui
        .add(egui::Button::image_and_text(
            icon(ui, crate::icons::plus()),
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
                                icon(ui, crate::icons::trash()),
                                "Remove Vehicle",
                            ))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                        if ui
                            .add(egui::Button::image_and_text(
                                icon(ui, crate::icons::copy()),
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
            .selected_text(draft.selected_profile.as_deref().unwrap_or("—"))
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

fn show_profiles_tab(ui: &mut egui::Ui, state: &mut VehicleDialog, snapshot: &StoreSnapshot) {
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
                egui::Button::image(icon(ui, crate::icons::trash())),
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
                            egui::Button::image(icon(ui, crate::icons::folder_open())),
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

fn profile_field_ref(topic: &str, field: &str, label: &str) -> Result<FieldRef, String> {
    let topic = topic.trim();
    let field = field.trim();
    if topic.is_empty() {
        return Err(format!("Enter a topic for {label}"));
    }
    if field.is_empty() {
        return Err(format!("Enter a field for {label}"));
    }
    Ok(FieldRef {
        topic: topic.to_owned(),
        field: field.to_owned(),
    })
}

fn profile_model_to_layout(model: &ModelKind, custom_path: &str) -> ModelLayout {
    match model {
        ModelKind::Quad => ModelLayout::Quad,
        ModelKind::FixedWing => ModelLayout::FixedWing,
        ModelKind::DeltaWing => ModelLayout::DeltaWing,
        ModelKind::Cone => ModelLayout::Cone,
        ModelKind::CustomGlb(_) => ModelLayout::CustomGlb {
            path: custom_path.trim().to_owned(),
        },
    }
}

fn profile_model_from_layout(model: &ModelLayout) -> ModelKind {
    match model {
        ModelLayout::Quad => ModelKind::Quad,
        ModelLayout::FixedWing => ModelKind::FixedWing,
        ModelLayout::DeltaWing => ModelKind::DeltaWing,
        ModelLayout::Cone => ModelKind::Cone,
        ModelLayout::CustomGlb { path } => ModelKind::CustomGlb(path.into()),
    }
}

fn color_to_rgba(c: Color32) -> [u8; 4] {
    [c.r(), c.g(), c.b(), c.a()]
}

fn rgba_to_color(rgba: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
}

fn handle_profile_action(
    action: ProfileAction,
    state: &mut VehicleDialog,
    snapshot: &StoreSnapshot,
) {
    match action {
        ProfileAction::Apply { draft, name } => {
            apply_profile_to_draft(state, draft, &name, snapshot)
        }
        ProfileAction::Delete(name) => {
            state.pending_profile_delete = Some(name);
        }
    }
}

fn load_profile_editor(state: &mut VehicleDialog) {
    let Some(name) = state.profile_editor_selected.clone() else {
        state.profile_editor_name.clear();
        state.profile_editor_draft = ProfileDraft::default();
        return;
    };
    let Some(library) = profile_library() else {
        log_profile(
            state,
            LogLevel::Warning,
            "vehicle profile config directory is unavailable",
        );
        return;
    };
    match library.load(&name) {
        Ok(doc) => {
            state.profile_editor_name = doc.name.clone();
            state.profile_editor_draft = ProfileDraft::from_doc(&doc);
        }
        Err(err) => {
            log_profile(
                state,
                LogLevel::Error,
                format!("failed to load vehicle profile '{name}': {err}"),
            );
        }
    }
}

fn save_profile_from_editor(state: &mut VehicleDialog) {
    let name = state.profile_editor_name.trim().to_owned();
    if name.is_empty() {
        log_profile(state, LogLevel::Warning, "enter a vehicle profile name");
        return;
    }
    let Some(library) = profile_library() else {
        log_profile(
            state,
            LogLevel::Warning,
            "vehicle profile config directory is unavailable",
        );
        return;
    };
    let doc = match state.profile_editor_draft.to_doc(&name) {
        Ok(doc) => doc,
        Err(err) => {
            log_profile(
                state,
                LogLevel::Warning,
                format!("invalid vehicle profile '{name}': {err}"),
            );
            return;
        }
    };
    if let Err(err) = library.save(&name, &doc) {
        log_profile(
            state,
            LogLevel::Error,
            format!("failed to save vehicle profile '{name}': {err}"),
        );
        return;
    }

    refresh_profiles(state);
    state.profile_editor_selected = Some(name.clone());
    state.profile_editor_name = name.clone();
    log_profile(
        state,
        LogLevel::Info,
        format!("saved vehicle profile '{name}'"),
    );
}

fn apply_profile_to_draft(
    state: &mut VehicleDialog,
    draft_index: usize,
    name: &str,
    snapshot: &StoreSnapshot,
) {
    let Some(library) = profile_library() else {
        log_profile(
            state,
            LogLevel::Warning,
            "vehicle profile config directory is unavailable",
        );
        return;
    };
    let doc = match library.load(name) {
        Ok(doc) => doc,
        Err(err) => {
            log_profile(
                state,
                LogLevel::Error,
                format!("failed to load vehicle profile '{name}': {err}"),
            );
            return;
        }
    };
    let Some(draft) = state.drafts.get_mut(draft_index) else {
        log_profile(
            state,
            LogLevel::Warning,
            format!("missing vehicle draft {draft_index} for profile '{name}'"),
        );
        return;
    };
    let cfg = match draft.source {
        Some(source) => doc.to_config_for_source(snapshot, source),
        None => doc.to_config(snapshot),
    };
    let Some(cfg) = cfg else {
        log_profile(
            state,
            LogLevel::Warning,
            format!("vehicle profile '{name}' does not match the current data"),
        );
        return;
    };

    draft.apply_config_preserving_label(&cfg, snapshot);
    draft.selected_profile = Some(name.to_owned());
    log_profile(
        state,
        LogLevel::Info,
        format!("applied vehicle profile '{name}'"),
    );
}

fn show_profile_delete_confirmation(ctx: &egui::Context, state: &mut VehicleDialog) {
    let Some(name) = state.pending_profile_delete.clone() else {
        return;
    };

    let mut close_confirmation = false;

    egui::Window::new("Delete profile?")
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .show(ctx, |ui| {
            ui.label(format!("Delete vehicle profile '{name}'?"));
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    match profile_library() {
                        Some(library) => match library.delete(&name) {
                            Ok(()) => {
                                for draft in &mut state.drafts {
                                    if draft.selected_profile.as_deref() == Some(name.as_str()) {
                                        draft.selected_profile = None;
                                    }
                                }
                                if state.profile_editor_selected.as_deref() == Some(name.as_str()) {
                                    state.profile_editor_selected = None;
                                    state.profile_editor_name.clear();
                                    state.profile_editor_draft = ProfileDraft::default();
                                }
                                refresh_profiles(state);
                                log_profile(
                                    state,
                                    LogLevel::Info,
                                    format!("deleted vehicle profile '{name}'"),
                                );
                            }
                            Err(err) => {
                                log_profile(
                                    state,
                                    LogLevel::Error,
                                    format!("failed to delete vehicle profile '{name}': {err}"),
                                );
                            }
                        },
                        None => {
                            log_profile(
                                state,
                                LogLevel::Warning,
                                "vehicle profile config directory is unavailable",
                            );
                        }
                    }
                    close_confirmation = true;
                }
                if ui.button("Cancel").clicked() {
                    close_confirmation = true;
                }
            });
        });

    if close_confirmation {
        state.pending_profile_delete = None;
    }
}

fn icon(ui: &egui::Ui, src: egui::ImageSource<'static>) -> egui::Image<'static> {
    egui::Image::new(src)
        .fit_to_exact_size(egui::vec2(16.0, 16.0))
        .tint(ui.visuals().text_color())
}

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
                            egui::Button::image(icon(ui, crate::icons::folder_open())),
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
                            egui::Button::image(icon(ui, crate::icons::close())),
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

/// Returns `true` if the selection changed (caller clears stale columns).
fn topic_combo(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut Option<TopicId>,
    topics: &[(TopicId, String)],
) -> bool {
    searchable_combo(ui, salt, sel, topics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_glb_path_uses_file_picker_not_text_edit() {
        let source = include_str!("vehicle_dialog.rs");

        assert!(source.contains(".set_title(\"Choose custom GLB\")"));
        assert!(source.contains(".add_filter(\"GLB models\", &[\"glb\", \"GLB\"])"));
        let text_edit = concat!("text_edit_singleline", "(&mut draft.custom_path)");
        assert!(!source.contains(text_edit));
    }

    #[test]
    fn searchable_topic_combo_keeps_scrollbar_at_popup_edge_without_fighting_mouse_scroll() {
        let source = include_str!("vehicle_dialog.rs");
        let combo = source
            .split("fn searchable_combo")
            .nth(1)
            .expect("searchable combo should exist");

        assert!(
            combo.contains(".auto_shrink([false, true])"),
            "topic dropdown list should keep the horizontal space reserved by the popup"
        );
        assert!(
            combo.contains("let scroll_to_highlight")
                && combo.contains("highlight_changed_by_keyboard || initialized_highlight"),
            "highlighted topic should only request scrolling after an explicit highlight move"
        );
        assert!(
            combo.contains("if scroll_to_highlight && i == highlighted"),
            "mouse-wheel frames must not re-scroll to the highlighted topic"
        );
        assert!(
            !combo.contains(
                "if i == highlighted {\n                                response.scroll_to_me"
            ),
            "unconditional scroll_to_me fights normal mouse-wheel scrolling"
        );
    }

    #[test]
    fn new_vehicle_draft_defaults_to_fixed_wing_model() {
        assert_eq!(Draft::default().model, ModelKind::FixedWing);
    }

    #[test]
    fn vehicle_dialog_has_profile_tab_and_auto_apply_dropdown() {
        let source = include_str!("vehicle_dialog.rs");

        assert!(source.contains("VehicleDialogTab"));
        assert!(source.contains("Vehicle Config"));
        assert!(source.contains("Profiles"));
        assert!(source.contains("Profile"));
        assert!(source.contains("draft.selected_profile != before"));
        assert!(source.contains("ProfileAction::Apply"));
        assert!(source.contains("Add Profile"));
        assert!(source.contains("Update Profile"));
        assert!(source.contains("profile_editor_form"));
        assert!(source.contains("Lat/Lon units"));
        assert!(source.contains("Angle Unit"));
        assert!(source.contains("Delete profile?"));
        assert!(!source.contains(concat!("open_vehicle", "_profile")));
    }

    #[test]
    fn add_vehicle_button_is_only_in_vehicle_config_tab() {
        let source = include_str!("vehicle_dialog.rs");
        let window_body = source
            .split(".show(ctx, |ui| {")
            .nth(1)
            .expect("vehicle window body should exist")
            .split("fn show_vehicle_config_tab")
            .next()
            .expect("window body should precede config tab function");
        let config_tab = source
            .split("fn show_vehicle_config_tab")
            .nth(1)
            .expect("config tab should exist")
            .split("fn show_vehicle_profile_dropdown")
            .next()
            .expect("config tab should precede profile dropdown");

        assert!(!window_body.contains("\"Add Vehicle\""));
        assert!(config_tab.contains("\"Add Vehicle\""));
    }

    #[test]
    fn profile_delete_uses_confirmation_window() {
        let source = include_str!("vehicle_dialog.rs");

        assert!(source.contains("pending_profile_delete"));
        assert!(source.contains("Delete profile?"));
        assert!(source.contains(concat!("egui::Window", "::new(\"Delete ", "profile?\")")));
        assert!(!source.contains(concat!("ui.", "group(|ui|")));
        assert!(source.contains(".delete("));
    }

    #[test]
    fn profile_editor_state_is_stored_on_dialog() {
        let source = include_str!("vehicle_dialog.rs");
        let draft = source
            .split("struct Draft")
            .nth(1)
            .expect("Draft should exist")
            .split("impl Default for Draft")
            .next()
            .expect("Draft fields should precede default impl");
        let dialog = source
            .split("pub struct VehicleDialog")
            .nth(1)
            .expect("VehicleDialog should exist")
            .split("impl Default for VehicleDialog")
            .next()
            .expect("VehicleDialog fields should precede default impl");

        assert!(draft.contains("selected_profile: Option<String>"));
        assert!(!draft.contains("save_profile_name: String"));
        assert!(!dialog.contains("selected_profile: Option<String>"));
        assert!(dialog.contains("profile_editor_selected: Option<String>"));
        assert!(dialog.contains("profile_editor_name: String"));
        assert!(dialog.contains("profile_editor_draft: ProfileDraft"));
        assert!(!source.contains(concat!("Add a vehicle before ", "creating a profile.")));
        assert!(!source.contains(concat!("profile_editor", "_json")));
    }
}
