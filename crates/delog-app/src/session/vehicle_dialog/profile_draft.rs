use egui::Color32;

use crate::config::layout::doc::{
    FieldRef, ModelLayout, NedRefLayout, OriLayout, PosLayout, VehicleLayout,
};
use crate::scene3d::vehicle::ModelKind;
use crate::session::vehicle_profiles::{VEHICLE_PROFILE_VERSION, VehicleProfileDoc};

use super::draft::{OriMode, PosMode};

#[derive(Clone)]
pub(super) struct ProfileDraft {
    pub(super) label: String,
    pub(super) show: bool,
    pub(super) show_path: bool,
    pub(super) pos_mode: PosMode,
    pub(super) pos_topic: String,
    pub(super) north: String,
    pub(super) east: String,
    pub(super) down: String,
    pub(super) lat: String,
    pub(super) lon: String,
    pub(super) alt: String,
    pub(super) lat_lon_dege7: bool,
    pub(super) alt_mm: bool,
    pub(super) alt_offset_m: f64,
    pub(super) ned_has_ref: bool,
    pub(super) ned_ref_manual: bool,
    pub(super) ref_lat: f64,
    pub(super) ref_lon: f64,
    pub(super) ref_alt: f64,
    pub(super) ref_lat_f: String,
    pub(super) ref_lon_f: String,
    pub(super) ref_alt_f: String,
    pub(super) ori_mode: OriMode,
    pub(super) ori_topic: String,
    pub(super) roll: String,
    pub(super) pitch: String,
    pub(super) yaw: String,
    pub(super) euler_degrees: bool,
    pub(super) qw: String,
    pub(super) qx: String,
    pub(super) qy: String,
    pub(super) qz: String,
    pub(super) model: ModelKind,
    pub(super) custom_path: String,
    pub(super) color: Color32,
    pub(super) path_color: Color32,
    pub(super) scale: f32,
}

impl Default for ProfileDraft {
    fn default() -> Self {
        Self {
            label: "Vehicle".to_owned(),
            show: true,
            show_path: true,
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
    pub(super) fn from_doc(doc: &VehicleProfileDoc) -> Self {
        let vehicle = &doc.vehicle;
        let mut draft = Self {
            label: vehicle.label.clone(),
            show: vehicle.show,
            show_path: vehicle.show_path,
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

    pub(super) fn to_doc(&self, name: &str) -> Result<VehicleProfileDoc, String> {
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
                show_path: self.show_path,
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

pub(super) fn profile_field_ref(topic: &str, field: &str, label: &str) -> Result<FieldRef, String> {
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

pub(super) fn profile_model_to_layout(model: &ModelKind, custom_path: &str) -> ModelLayout {
    match model {
        ModelKind::None => ModelLayout::None,
        ModelKind::Quad => ModelLayout::Quad,
        ModelKind::FixedWing => ModelLayout::FixedWing,
        ModelKind::DeltaWing => ModelLayout::DeltaWing,
        ModelKind::Cone => ModelLayout::Cone,
        ModelKind::Sphere => ModelLayout::Sphere,
        ModelKind::Cube => ModelLayout::Cube,
        ModelKind::CustomGlb(_) => ModelLayout::CustomGlb {
            path: custom_path.trim().to_owned(),
        },
    }
}

pub(super) fn profile_model_from_layout(model: &ModelLayout) -> ModelKind {
    match model {
        ModelLayout::None => ModelKind::None,
        ModelLayout::Quad => ModelKind::Quad,
        ModelLayout::FixedWing => ModelKind::FixedWing,
        ModelLayout::DeltaWing => ModelKind::DeltaWing,
        ModelLayout::Cone => ModelKind::Cone,
        ModelLayout::Sphere => ModelKind::Sphere,
        ModelLayout::Cube => ModelKind::Cube,
        ModelLayout::CustomGlb { path } => ModelKind::CustomGlb(path.into()),
    }
}

pub(super) fn color_to_rgba(c: Color32) -> [u8; 4] {
    [c.r(), c.g(), c.b(), c.a()]
}

pub(super) fn rgba_to_color(rgba: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
}
