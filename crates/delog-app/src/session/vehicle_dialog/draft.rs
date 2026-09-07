use delog_core::identity::{FieldId, SourceId, TopicId};
use delog_core::snapshot::StoreSnapshot;
use egui::Color32;

use crate::scene3d::vehicle::{
    GeoRef, ModelKind, NedReference, OriMapping, PosMapping, VehicleConfig,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PosMode {
    Ned,
    Gps,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OriMode {
    Static,
    Euler,
    Quat,
}

#[derive(Clone)]
pub(super) struct Draft {
    pub(super) label: String,
    pub(super) show: bool,
    pub(super) show_path: bool,
    pub(super) source: Option<SourceId>,
    pub(super) pos_topic: Option<TopicId>,
    pub(super) pos_mode: PosMode,
    pub(super) north: Option<FieldId>,
    pub(super) east: Option<FieldId>,
    pub(super) down: Option<FieldId>,
    pub(super) lat: Option<FieldId>,
    pub(super) lon: Option<FieldId>,
    pub(super) alt: Option<FieldId>,
    /// degE7 integers (×1e-7 → degrees).
    pub(super) lat_lon_dege7: bool,
    /// millimetres (×1e-3 → metres).
    pub(super) alt_mm: bool,
    /// metres, up-positive.
    pub(super) alt_offset_m: f64,
    pub(super) ned_has_ref: bool,
    /// Reference from fixed values (true) or from columns (false).
    pub(super) ned_ref_manual: bool,
    pub(super) ref_lat: f64,
    pub(super) ref_lon: f64,
    pub(super) ref_alt: f64,
    pub(super) ref_lat_f: Option<FieldId>,
    pub(super) ref_lon_f: Option<FieldId>,
    pub(super) ref_alt_f: Option<FieldId>,
    pub(super) ori_topic: Option<TopicId>,
    pub(super) ori_mode: OriMode,
    pub(super) roll: Option<FieldId>,
    pub(super) pitch: Option<FieldId>,
    pub(super) yaw: Option<FieldId>,
    pub(super) euler_degrees: bool,
    pub(super) qw: Option<FieldId>,
    pub(super) qx: Option<FieldId>,
    pub(super) qy: Option<FieldId>,
    pub(super) qz: Option<FieldId>,
    pub(super) model: ModelKind,
    pub(super) custom_path: String,
    pub(super) color: Color32,
    pub(super) path_color: Color32,
    pub(super) scale: f32,
    pub(super) selected_profile: Option<String>,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            label: "Vehicle".into(),
            show: true,
            show_path: true,
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
    pub(super) fn from_config(cfg: &VehicleConfig, snapshot: &StoreSnapshot) -> Self {
        let topic_of = |f: FieldId| field_topic(snapshot, f);
        let mut d = Draft {
            label: cfg.label.clone(),
            show: cfg.show,
            show_path: cfg.show_path,
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
    pub(super) fn apply_config_preserving_label(
        &mut self,
        cfg: &VehicleConfig,
        snapshot: &StoreSnapshot,
    ) {
        let previous_label = self.label.clone();
        *self = Draft::from_config(cfg, snapshot);
        self.label = previous_label;
    }

    pub(super) fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.source.is_none() {
            missing.push("a data source");
            return missing;
        }
        match self.pos_mode {
            PosMode::Ned => {
                if self.north.is_none() || self.east.is_none() || self.down.is_none() {
                    if self.pos_topic.is_none() {
                        missing.push("a position topic");
                    } else {
                        if self.north.is_none() {
                            missing.push("North (X)");
                        }
                        if self.east.is_none() {
                            missing.push("East (Y)");
                        }
                        if self.down.is_none() {
                            missing.push("Down (Z)");
                        }
                    }
                }
            }
            PosMode::Gps => {
                if self.lat.is_none() || self.lon.is_none() || self.alt.is_none() {
                    if self.pos_topic.is_none() {
                        missing.push("a position topic");
                    } else {
                        if self.lat.is_none() {
                            missing.push("Latitude");
                        }
                        if self.lon.is_none() {
                            missing.push("Longitude");
                        }
                        if self.alt.is_none() {
                            missing.push("Altitude");
                        }
                    }
                }
            }
        }
        match self.ori_mode {
            OriMode::Static => {}
            OriMode::Euler => {
                if self.roll.is_none() || self.pitch.is_none() || self.yaw.is_none() {
                    if self.ori_topic.is_none() {
                        missing.push("an orientation topic");
                    } else {
                        if self.roll.is_none() {
                            missing.push("Roll");
                        }
                        if self.pitch.is_none() {
                            missing.push("Pitch");
                        }
                        if self.yaw.is_none() {
                            missing.push("Yaw");
                        }
                    }
                }
            }
            OriMode::Quat => {
                if self.qw.is_none() || self.qx.is_none() || self.qy.is_none() || self.qz.is_none()
                {
                    if self.ori_topic.is_none() {
                        missing.push("an orientation topic");
                    } else {
                        if self.qw.is_none() {
                            missing.push("QW");
                        }
                        if self.qx.is_none() {
                            missing.push("QX");
                        }
                        if self.qy.is_none() {
                            missing.push("QY");
                        }
                        if self.qz.is_none() {
                            missing.push("QZ");
                        }
                    }
                }
            }
        }
        missing
    }

    pub(super) fn build(&self) -> Option<VehicleConfig> {
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
            show_path: self.show_path,
            pos,
            ori,
            model,
            color: self.color,
            path_color: self.path_color,
            scale: self.scale.max(0.01),
        })
    }
}

pub(super) fn field_topic(snapshot: &StoreSnapshot, field: FieldId) -> Option<TopicId> {
    snapshot.fields.get(field.index()).map(|f| f.topic)
}

pub(super) fn source_topics(snapshot: &StoreSnapshot, source: SourceId) -> Vec<(TopicId, String)> {
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

pub(super) fn topic_fields(snapshot: &StoreSnapshot, topic: TopicId) -> Vec<(FieldId, String)> {
    snapshot
        .fields
        .iter()
        .filter(|f| f.topic == topic && !f.removed)
        .map(|f| (f.id, f.name.clone()))
        .collect()
}

pub(super) fn general_summary(draft: &Draft, source_name: Option<&str>) -> String {
    let source = source_name.unwrap_or("No source");
    format!("{source} \u{b7} {}", draft.model.label())
}

pub(super) fn position_summary(draft: &Draft, topic_name: Option<&str>) -> String {
    let mut parts = Vec::new();
    match draft.pos_mode {
        PosMode::Ned => {
            parts.push("Local NED".to_owned());
            if let Some(topic) = topic_name {
                parts.push(topic.to_owned());
            }
            if draft.ned_has_ref {
                parts.push("georeferenced".to_owned());
            }
        }
        PosMode::Gps => {
            parts.push("Global GPS".to_owned());
            if let Some(topic) = topic_name {
                parts.push(topic.to_owned());
            }
            if draft.lat_lon_dege7 {
                parts.push("degE7".to_owned());
            }
            if draft.alt_mm {
                parts.push("mm".to_owned());
            }
        }
    }
    parts.join(" \u{b7} ")
}

pub(super) fn orientation_summary(draft: &Draft, topic_name: Option<&str>) -> String {
    let mut parts = Vec::new();
    match draft.ori_mode {
        OriMode::Static => return "Static".to_owned(),
        OriMode::Euler => parts.push("Euler".to_owned()),
        OriMode::Quat => parts.push("Quaternion".to_owned()),
    }
    if let Some(topic) = topic_name {
        parts.push(topic.to_owned());
    }
    if draft.ori_mode == OriMode::Euler {
        parts.push(
            if draft.euler_degrees {
                "degrees"
            } else {
                "radians"
            }
            .to_owned(),
        );
    }
    parts.join(" \u{b7} ")
}
