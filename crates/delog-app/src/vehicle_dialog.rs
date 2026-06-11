//! Vehicle configuration dialog (PLAN.md §12.1, TDV-03). Lists configured
//! vehicles and edits a draft with per-source field-mapping pickers; an
//! auto-detect button fills the draft from common ArduPilot/PX4/MAVLink field
//! names. Building a [`VehicleConfig`] is where every mapping variant is
//! constructed, so this is the consumer that retires `vehicle.rs`'s temporary
//! dead-code allowance.

use delog_core::identity::{FieldId, SourceId};
use delog_core::snapshot::StoreSnapshot;
use egui::Color32;

use crate::vehicle::{GpsRef, LengthUnit, ModelKind, OriMapping, PosMapping, VehicleConfig};

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
    pos_mode: PosMode,
    north: Option<FieldId>,
    east: Option<FieldId>,
    down: Option<FieldId>,
    unit: LengthUnit,
    lat: Option<FieldId>,
    lon: Option<FieldId>,
    alt: Option<FieldId>,
    gps_degrees: bool,
    gps_manual_ref: bool,
    ref_lat: f64,
    ref_lon: f64,
    ref_alt: f64,
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
            pos_mode: PosMode::Ned,
            north: None,
            east: None,
            down: None,
            unit: LengthUnit::Meters,
            lat: None,
            lon: None,
            alt: None,
            gps_degrees: true,
            gps_manual_ref: false,
            ref_lat: 0.0,
            ref_lon: 0.0,
            ref_alt: 0.0,
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
    /// Build a `VehicleConfig` if the selected mapping has all its fields.
    fn build(&self) -> Option<VehicleConfig> {
        let source = self.source?;
        let pos = match self.pos_mode {
            PosMode::Ned => PosMapping::Ned {
                north: self.north?,
                east: self.east?,
                down: self.down?,
                unit: self.unit,
            },
            PosMode::Gps => PosMapping::Gps {
                lat: self.lat?,
                lon: self.lon?,
                alt: self.alt?,
                degrees: self.gps_degrees,
                reference: if self.gps_manual_ref {
                    GpsRef::Manual {
                        lat_deg: self.ref_lat,
                        lon_deg: self.ref_lon,
                        alt_m: self.ref_alt,
                    }
                } else {
                    GpsRef::Auto
                },
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

    /// Fill field selections by matching common position/attitude field names.
    fn autodetect(&mut self, fields: &[(FieldId, String)]) {
        let find = |needles: &[&str]| -> Option<FieldId> {
            fields.iter().find_map(|(id, label)| {
                let l = label.to_ascii_lowercase();
                needles.iter().any(|n| l.ends_with(n)).then_some(*id)
            })
        };
        // GPS first (AP POS.Lat/Lng/Alt, MAVLink lat/lon/alt).
        if let (Some(la), Some(lo), Some(al)) =
            (find(&[".lat"]), find(&[".lng", ".lon"]), find(&[".alt"]))
        {
            self.pos_mode = PosMode::Gps;
            self.lat = Some(la);
            self.lon = Some(lo);
            self.alt = Some(al);
        }
        if let (Some(r), Some(p), Some(y)) = (find(&[".roll"]), find(&[".pitch"]), find(&[".yaw"]))
        {
            self.ori_mode = OriMode::Euler;
            self.roll = Some(r);
            self.pitch = Some(p);
            self.yaw = Some(y);
        }
    }
}

/// Vehicle-config dialog state (open flag + the working draft).
#[derive(Default)]
pub struct VehicleDialog {
    pub open: bool,
    draft: Draft,
}

/// All `(FieldId, "TOPIC.field")` of a source, for the field pickers.
fn source_fields(snapshot: &StoreSnapshot, source: SourceId) -> Vec<(FieldId, String)> {
    let mut out = Vec::new();
    for src in snapshot.sources.iter() {
        if src.entry.id != source || src.entry.removed {
            continue;
        }
        for &topic_id in src.topics.iter() {
            let Some(topic) = snapshot.topic(topic_id) else {
                continue;
            };
            if topic.entry.removed {
                continue;
            }
            for field in snapshot.fields.iter() {
                if field.topic == topic_id && !field.removed {
                    out.push((field.id, format!("{}.{}", topic.entry.name, field.name)));
                }
            }
        }
    }
    out
}

fn field_label(fields: &[(FieldId, String)], sel: Option<FieldId>) -> &str {
    match sel {
        Some(id) => fields
            .iter()
            .find(|(f, _)| *f == id)
            .map(|(_, l)| l.as_str())
            .unwrap_or("—"),
        None => "—",
    }
}

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
            .selected_text(field_label(fields, *sel))
            .show_ui(ui, |ui| {
                for (id, name) in fields {
                    ui.selectable_value(sel, Some(*id), name);
                }
            });
    });
}

/// Render the dialog; mutates `vehicles` when the user adds/removes/edits.
pub fn show(
    ctx: &egui::Context,
    state: &mut VehicleDialog,
    vehicles: &mut Vec<VehicleConfig>,
    snapshot: &StoreSnapshot,
) {
    if !state.open {
        return;
    }
    let mut open = state.open;
    egui::Window::new("Vehicles")
        .open(&mut open)
        .default_width(360.0)
        .show(ctx, |ui| {
            // Existing vehicles: show toggle + remove.
            let mut remove: Option<usize> = None;
            for (i, v) in vehicles.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut v.show, "");
                    ui.label(&v.label);
                    ui.label(format!("({})", v.model.label()));
                    if ui.button("✕").on_hover_text("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                vehicles.remove(i);
            }
            ui.separator();

            draft_editor(ui, &mut state.draft, snapshot);

            ui.separator();
            let can_add = state.draft.build().is_some();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can_add, egui::Button::new("Add vehicle"))
                    .clicked()
                    && let Some(cfg) = state.draft.build()
                {
                    vehicles.push(cfg);
                }
                if !can_add {
                    ui.weak("pick a source and its position fields");
                }
            });
        });
    state.open = open;
}

fn draft_editor(ui: &mut egui::Ui, draft: &mut Draft, snapshot: &StoreSnapshot) {
    // Source picker.
    let sources: Vec<(SourceId, String)> = snapshot
        .sources
        .iter()
        .filter(|s| !s.entry.removed)
        .map(|s| (s.entry.id, s.entry.label.clone()))
        .collect();
    ui.horizontal(|ui| {
        ui.label("Source");
        let sel_label = draft
            .source
            .and_then(|id| sources.iter().find(|(s, _)| *s == id))
            .map(|(_, l)| l.as_str())
            .unwrap_or("—");
        egui::ComboBox::from_id_salt("veh-source")
            .selected_text(sel_label)
            .show_ui(ui, |ui| {
                for (id, label) in &sources {
                    ui.selectable_value(&mut draft.source, Some(*id), label);
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
    let fields = source_fields(snapshot, source);
    if ui.button("Auto-detect fields").clicked() {
        draft.autodetect(&fields);
    }

    // Position.
    ui.separator();
    ui.label("Position");
    ui.horizontal(|ui| {
        ui.selectable_value(&mut draft.pos_mode, PosMode::Ned, "NED / local");
        ui.selectable_value(&mut draft.pos_mode, PosMode::Gps, "GPS");
    });
    match draft.pos_mode {
        PosMode::Ned => {
            field_combo(ui, "veh-n", "North", &mut draft.north, &fields);
            field_combo(ui, "veh-e", "East", &mut draft.east, &fields);
            field_combo(ui, "veh-d", "Down", &mut draft.down, &fields);
            ui.horizontal(|ui| {
                ui.label("Unit");
                for (u, name) in [
                    (LengthUnit::Meters, "m"),
                    (LengthUnit::Centimeters, "cm"),
                    (LengthUnit::Feet, "ft"),
                ] {
                    ui.selectable_value(&mut draft.unit, u, name);
                }
            });
        }
        PosMode::Gps => {
            field_combo(ui, "veh-lat", "Lat", &mut draft.lat, &fields);
            field_combo(ui, "veh-lon", "Lon", &mut draft.lon, &fields);
            field_combo(ui, "veh-alt", "Alt", &mut draft.alt, &fields);
            ui.checkbox(&mut draft.gps_degrees, "Lat/Lon in degrees");
            ui.checkbox(&mut draft.gps_manual_ref, "Manual reference origin");
            if draft.gps_manual_ref {
                ui.horizontal(|ui| {
                    ui.label("ref lat/lon/alt");
                    ui.add(egui::DragValue::new(&mut draft.ref_lat).speed(0.0001));
                    ui.add(egui::DragValue::new(&mut draft.ref_lon).speed(0.0001));
                    ui.add(egui::DragValue::new(&mut draft.ref_alt).speed(0.1));
                });
            }
        }
    }

    // Orientation.
    ui.separator();
    ui.label("Orientation");
    ui.horizontal(|ui| {
        ui.selectable_value(&mut draft.ori_mode, OriMode::Static, "Static");
        ui.selectable_value(&mut draft.ori_mode, OriMode::Euler, "Euler");
        ui.selectable_value(&mut draft.ori_mode, OriMode::Quat, "Quaternion");
    });
    match draft.ori_mode {
        OriMode::Static => {}
        OriMode::Euler => {
            field_combo(ui, "veh-roll", "Roll", &mut draft.roll, &fields);
            field_combo(ui, "veh-pitch", "Pitch", &mut draft.pitch, &fields);
            field_combo(ui, "veh-yaw", "Yaw", &mut draft.yaw, &fields);
            ui.checkbox(&mut draft.euler_degrees, "Angles in degrees");
        }
        OriMode::Quat => {
            field_combo(ui, "veh-qw", "W", &mut draft.qw, &fields);
            field_combo(ui, "veh-qx", "X", &mut draft.qx, &fields);
            field_combo(ui, "veh-qy", "Y", &mut draft.qy, &fields);
            field_combo(ui, "veh-qz", "Z", &mut draft.qz, &fields);
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
