#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use delog_core::identity::SourceId;
use delog_core::snapshot::StoreSnapshot;
use serde::{Deserialize, Serialize};

use crate::config::layout::doc as layout;
use crate::scene3d::vehicle::VehicleConfig;

pub const VEHICLE_PROFILE_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VehicleProfileDoc {
    pub delog_vehicle_profile: u32,
    pub name: String,
    pub vehicle: crate::config::layout::doc::VehicleLayout,
}

impl VehicleProfileDoc {
    pub fn from_config(
        name: &str,
        config: &VehicleConfig,
        snapshot: &StoreSnapshot,
    ) -> Option<Self> {
        let doc = Self {
            delog_vehicle_profile: VEHICLE_PROFILE_VERSION,
            name: name.trim().to_owned(),
            vehicle: layout::vehicle_config_to_layout(config, snapshot)?,
        };
        doc.validate().ok()?;
        Some(doc)
    }

    pub fn to_config(&self, snapshot: &StoreSnapshot) -> Option<VehicleConfig> {
        self.validate().ok()?;
        let mut config = layout::vehicle_config_from_layout(&self.vehicle, snapshot)?;
        config.scale = self.vehicle.scale;
        Some(config)
    }

    pub fn to_config_for_source(
        &self,
        snapshot: &StoreSnapshot,
        source: SourceId,
    ) -> Option<VehicleConfig> {
        self.validate().ok()?;
        let mut config =
            layout::vehicle_config_from_layout_for_source(&self.vehicle, snapshot, source)?;
        config.scale = self.vehicle.scale;
        Some(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.delog_vehicle_profile != VEHICLE_PROFILE_VERSION {
            return Err(format!(
                "unsupported vehicle profile version {}",
                self.delog_vehicle_profile
            ));
        }
        if !self.vehicle.scale.is_finite() || self.vehicle.scale <= 0.0 {
            return Err("vehicle scale must be finite and > 0".to_owned());
        }
        match &self.vehicle.position {
            layout::PosLayout::Gps { alt_offset_m, .. } => {
                if !alt_offset_m.is_finite() {
                    return Err("vehicle GPS alt_offset_m must be finite".to_owned());
                }
            }
            layout::PosLayout::Ned {
                reference:
                    Some(layout::NedRefLayout::Manual {
                        lat_deg,
                        lon_deg,
                        alt_m,
                    }),
                ..
            } => {
                if !lat_deg.is_finite()
                    || !lon_deg.is_finite()
                    || !alt_m.is_finite()
                    || !(-90.0..=90.0).contains(lat_deg)
                    || !(-180.0..=180.0).contains(lon_deg)
                {
                    return Err("vehicle NED georeference is invalid".to_owned());
                }
            }
            layout::PosLayout::Ned { .. } => {}
        }
        Ok(())
    }

    #[cfg(feature = "scripting")]
    pub fn to_script_info(&self) -> delog_script::VehicleProfileInfo {
        delog_script::VehicleProfileInfo {
            name: self.name.clone(),
            label: self.vehicle.label.clone(),
            show: self.vehicle.show,
            show_path: self.vehicle.show_path,
            position: script_position(&self.vehicle.position),
            orientation: script_orientation(&self.vehicle.orientation),
            model: script_model(&self.vehicle.model),
            color: script_color(self.vehicle.color),
            path_color: script_color(self.vehicle.path_color),
            scale: self.vehicle.scale,
        }
    }
}

#[cfg(feature = "scripting")]
fn script_field(field: &layout::FieldRef) -> delog_script::ProfileFieldRef {
    delog_script::ProfileFieldRef {
        topic: field.topic.clone(),
        field: field.field.clone(),
    }
}

#[cfg(feature = "scripting")]
fn script_position(position: &layout::PosLayout) -> delog_script::ProfilePosition {
    match position {
        layout::PosLayout::Ned {
            north,
            east,
            down,
            reference,
        } => delog_script::ProfilePosition::Ned {
            north: script_field(north),
            east: script_field(east),
            down: script_field(down),
            reference: reference.as_ref().map(|reference| match reference {
                layout::NedRefLayout::Manual {
                    lat_deg,
                    lon_deg,
                    alt_m,
                } => delog_script::ProfileNedReference::Manual {
                    lat_deg: *lat_deg,
                    lon_deg: *lon_deg,
                    alt_m: *alt_m,
                },
                layout::NedRefLayout::Fields { lat, lon, alt } => {
                    delog_script::ProfileNedReference::Fields {
                        lat: script_field(lat),
                        lon: script_field(lon),
                        alt: script_field(alt),
                    }
                }
            }),
        },
        layout::PosLayout::Gps {
            lat,
            lon,
            alt,
            lat_lon_dege7,
            alt_mm,
            alt_offset_m,
        } => delog_script::ProfilePosition::Gps {
            lat: script_field(lat),
            lon: script_field(lon),
            alt: script_field(alt),
            lat_lon_dege7: *lat_lon_dege7,
            alt_mm: *alt_mm,
            alt_offset_m: *alt_offset_m,
        },
    }
}

#[cfg(feature = "scripting")]
fn script_orientation(orientation: &layout::OriLayout) -> delog_script::ProfileOrientation {
    match orientation {
        layout::OriLayout::Static => delog_script::ProfileOrientation::Static,
        layout::OriLayout::Euler {
            roll,
            pitch,
            yaw,
            degrees,
        } => delog_script::ProfileOrientation::Euler {
            roll: script_field(roll),
            pitch: script_field(pitch),
            yaw: script_field(yaw),
            degrees: *degrees,
        },
        layout::OriLayout::Quat { w, x, y, z } => delog_script::ProfileOrientation::Quat {
            w: script_field(w),
            x: script_field(x),
            y: script_field(y),
            z: script_field(z),
        },
    }
}

#[cfg(feature = "scripting")]
fn script_model(model: &layout::ModelLayout) -> delog_script::VehicleModel {
    match model {
        layout::ModelLayout::None => delog_script::VehicleModel::None,
        layout::ModelLayout::Quad => delog_script::VehicleModel::Quad,
        layout::ModelLayout::FixedWing => delog_script::VehicleModel::FixedWing,
        layout::ModelLayout::DeltaWing => delog_script::VehicleModel::DeltaWing,
        layout::ModelLayout::Cone => delog_script::VehicleModel::Cone,
        layout::ModelLayout::Sphere => delog_script::VehicleModel::Sphere,
        layout::ModelLayout::Cube => delog_script::VehicleModel::Cube,
        layout::ModelLayout::CustomGlb { path } => {
            delog_script::VehicleModel::CustomGlb(path.clone())
        }
    }
}

#[cfg(feature = "scripting")]
fn script_color(color: [u8; 4]) -> [f32; 4] {
    color.map(|component| component as f32 / 255.0)
}

impl PartialEq for VehicleProfileDoc {
    fn eq(&self, other: &Self) -> bool {
        self.delog_vehicle_profile == other.delog_vehicle_profile
            && self.name == other.name
            && serde_json::to_value(&self.vehicle).ok() == serde_json::to_value(&other.vehicle).ok()
    }
}

#[derive(Clone, Debug)]
pub struct VehicleProfileLibrary {
    dir: PathBuf,
}

impl VehicleProfileLibrary {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn from_config_dir() -> Option<Self> {
        crate::config::layout::doc::config_dir().map(|dir| Self::new(dir.join("vehicle_profiles")))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn list(&self) -> io::Result<Vec<String>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }

        let mut profiles = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
                && let Ok(name) = sanitize_name(stem)
                && name == stem
            {
                profiles.push(name);
            }
        }
        profiles.sort();
        Ok(profiles)
    }

    pub fn load(&self, name: &str) -> io::Result<VehicleProfileDoc> {
        let path = self.profile_path(name)?;
        let json = fs::read_to_string(path)?;
        let doc: VehicleProfileDoc = serde_json::from_str(&json)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        if doc.delog_vehicle_profile != VEHICLE_PROFILE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported vehicle profile version {}",
                    doc.delog_vehicle_profile
                ),
            ));
        }
        doc.validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(doc)
    }

    pub fn save(&self, name: &str, doc: &VehicleProfileDoc) -> io::Result<()> {
        let path = self.profile_path(name)?;
        doc.validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::create_dir_all(&self.dir)?;
        let json = serde_json::to_string_pretty(doc)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        fs::write(path, json)
    }

    pub fn delete(&self, name: &str) -> io::Result<()> {
        fs::remove_file(self.profile_path(name)?)
    }

    fn profile_path(&self, name: &str) -> io::Result<PathBuf> {
        Ok(self.dir.join(format!("{}.json", sanitize_name(name)?)))
    }
}

fn sanitize_name(name: &str) -> io::Result<String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "vehicle profile name must not be empty or contain path separators/traversal",
        ));
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use egui::Color32;

    use crate::config::layout::doc::{FieldRef, ModelLayout, OriLayout, PosLayout, VehicleLayout};
    use crate::scene3d::vehicle::{ModelKind, OriMapping, PosMapping, VehicleConfig};

    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(test_name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "delog_vehicle_profiles_{test_name}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_profile_dir(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "delog_vehicle_profiles_{test_name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn field(topic: &str, field: &str) -> FieldRef {
        FieldRef {
            topic: topic.to_owned(),
            field: field.to_owned(),
        }
    }

    fn snapshot_with_local_position_and_attitude() -> delog_core::snapshot::StoreSnapshot {
        let mut ids = delog_core::identity::IdentityRegistry::new();
        let source = ids.add_source("log");
        let local = ids.add_topic(source, "LOCAL_POSITION_NED").unwrap();
        ids.add_field(local, "x").unwrap();
        ids.add_field(local, "y").unwrap();
        ids.add_field(local, "z").unwrap();
        let attitude = ids.add_topic(source, "ATTITUDE").unwrap();
        ids.add_field(attitude, "roll").unwrap();
        ids.add_field(attitude, "pitch").unwrap();
        ids.add_field(attitude, "yaw").unwrap();
        delog_core::snapshot::StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot")
    }

    fn snapshot_with_duplicate_local_position() -> delog_core::snapshot::StoreSnapshot {
        let mut ids = delog_core::identity::IdentityRegistry::new();
        for source in ["flight_a", "flight_b"] {
            let source = ids.add_source(source);
            let local = ids.add_topic(source, "LOCAL_POSITION_NED").unwrap();
            ids.add_field(local, "x").unwrap();
            ids.add_field(local, "y").unwrap();
            ids.add_field(local, "z").unwrap();
        }
        delog_core::snapshot::StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot")
    }

    fn source_id(
        snapshot: &delog_core::snapshot::StoreSnapshot,
        label: &str,
    ) -> delog_core::identity::SourceId {
        snapshot
            .sources
            .iter()
            .find(|source| !source.entry.removed && source.entry.label == label)
            .map(|source| source.entry.id)
            .expect("source should exist")
    }

    fn field_id(
        snapshot: &delog_core::snapshot::StoreSnapshot,
        topic_name: &str,
        field_name: &str,
    ) -> delog_core::identity::FieldId {
        snapshot
            .fields
            .iter()
            .find(|field| {
                !field.removed
                    && field.name == field_name
                    && snapshot
                        .topic(field.topic)
                        .is_some_and(|topic| topic.entry.name == topic_name)
            })
            .map(|field| field.id)
            .expect("field should exist")
    }

    fn sample_doc() -> VehicleProfileDoc {
        VehicleProfileDoc {
            delog_vehicle_profile: VEHICLE_PROFILE_VERSION,
            name: "mavlink_local_position".to_owned(),
            vehicle: VehicleLayout {
                owner: None,
                label: "Vehicle".to_owned(),
                show: true,
                show_path: true,
                model: ModelLayout::FixedWing,
                color: [90, 170, 255, 255],
                path_color: [255, 170, 60, 255],
                scale: 1.0,
                position: PosLayout::Ned {
                    north: field("LOCAL_POSITION_NED", "x"),
                    east: field("LOCAL_POSITION_NED", "y"),
                    down: field("LOCAL_POSITION_NED", "z"),
                    reference: None,
                },
                orientation: OriLayout::Euler {
                    roll: field("ATTITUDE", "roll"),
                    pitch: field("ATTITUDE", "pitch"),
                    yaw: field("ATTITUDE", "yaw"),
                    degrees: false,
                },
            },
        }
    }

    #[test]
    fn profile_json_round_trips() {
        let tmp = temp_profile_dir("profile_json_round_trips");
        let library = VehicleProfileLibrary::new(&tmp);
        let doc = sample_doc();

        library.save("mavlink_local_position", &doc).unwrap();

        assert_eq!(library.list().unwrap(), vec!["mavlink_local_position"]);
        assert_eq!(library.load("mavlink_local_position").unwrap(), doc);

        fs::remove_dir_all(tmp).unwrap();
    }

    #[cfg(feature = "scripting")]
    #[test]
    fn profile_doc_converts_to_source_independent_script_payload() {
        let info = sample_doc().to_script_info();

        assert_eq!(info.name, "mavlink_local_position");
        assert_eq!(info.label, "Vehicle");
        assert_eq!(info.model, delog_script::VehicleModel::FixedWing);
        assert_eq!(info.color, [90.0 / 255.0, 170.0 / 255.0, 1.0, 1.0]);
        assert!(matches!(
            info.position,
            delog_script::ProfilePosition::Ned {
                north: delog_script::ProfileFieldRef { ref topic, ref field },
                ..
            } if topic == "LOCAL_POSITION_NED" && field == "x"
        ));
        assert!(matches!(
            info.orientation,
            delog_script::ProfileOrientation::Euler {
                yaw: delog_script::ProfileFieldRef { ref topic, ref field },
                degrees: false,
                ..
            } if topic == "ATTITUDE" && field == "yaw"
        ));
    }

    #[test]
    fn profile_doc_from_vehicle_config_uses_layout_conversion() {
        let snapshot = snapshot_with_local_position_and_attitude();
        let source = snapshot
            .sources
            .iter()
            .find(|source| !source.entry.removed)
            .map(|source| source.entry.id)
            .expect("source should exist");
        let cfg = VehicleConfig {
            runtime: crate::scene3d::vehicle::VehicleRuntime::unassigned(),
            source,
            label: "Rover".to_owned(),
            show: true,
            show_path: true,
            pos: PosMapping::Ned {
                north: field_id(&snapshot, "LOCAL_POSITION_NED", "x"),
                east: field_id(&snapshot, "LOCAL_POSITION_NED", "y"),
                down: field_id(&snapshot, "LOCAL_POSITION_NED", "z"),
                reference: None,
            },
            ori: OriMapping::Static,
            model: ModelKind::Cone,
            color: Color32::WHITE,
            path_color: Color32::BLACK,
            scale: 2.0,
        };

        let doc = VehicleProfileDoc::from_config("Local", &cfg, &snapshot)
            .expect("profile should serialize");

        assert_eq!(doc.name, "Local");
        assert_eq!(doc.vehicle.label, "Rover");
        assert_eq!(
            doc.to_config(&snapshot).expect("profile should resolve"),
            cfg
        );
    }

    #[test]
    fn to_config_for_source_resolves_duplicate_topic_fields() {
        let snapshot = snapshot_with_duplicate_local_position();
        let second_source = source_id(&snapshot, "flight_b");
        let doc = VehicleProfileDoc {
            delog_vehicle_profile: VEHICLE_PROFILE_VERSION,
            name: "Local".to_owned(),
            vehicle: VehicleLayout {
                owner: None,
                label: "Rover".to_owned(),
                show: true,
                show_path: true,
                model: ModelLayout::Cone,
                color: [255, 255, 255, 255],
                path_color: [0, 0, 0, 255],
                scale: 2.0,
                position: PosLayout::Ned {
                    north: field("LOCAL_POSITION_NED", "x"),
                    east: field("LOCAL_POSITION_NED", "y"),
                    down: field("LOCAL_POSITION_NED", "z"),
                    reference: None,
                },
                orientation: OriLayout::Static,
            },
        };

        let cfg = doc
            .to_config_for_source(&snapshot, second_source)
            .expect("profile should resolve for selected source");

        assert_eq!(cfg.source, second_source);
        let PosMapping::Ned {
            north, east, down, ..
        } = cfg.pos
        else {
            panic!("expected NED mapping");
        };
        for field in [north, east, down] {
            let topic = snapshot
                .fields
                .get(field.index())
                .and_then(|field| snapshot.topic(field.topic))
                .expect("field topic should exist");
            assert_eq!(topic.entry.source, second_source);
        }
    }

    #[test]
    fn rejects_path_traversal_names() {
        let tmp = temp_profile_dir("rejects_path_traversal_names");
        let library = VehicleProfileLibrary::new(&tmp);
        let doc = sample_doc();

        assert!(library.save("../evil", &doc).is_err());
        assert!(library.load("a/b").is_err());
        assert!(library.delete("a\\b").is_err());

        fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn list_ignores_invalid_external_file_names() {
        let tmp = temp_profile_dir("list_ignores_invalid_external_file_names");
        let library = VehicleProfileLibrary::new(&tmp);
        let doc = sample_doc();

        library.save("mavlink_local_position", &doc).unwrap();
        fs::write(tmp.join("bad..name.json"), "{}").unwrap();

        assert_eq!(library.list().unwrap(), vec!["mavlink_local_position"]);

        fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn load_rejects_unsupported_profile_version() {
        let tmp = temp_profile_dir("load_rejects_unsupported_profile_version");
        let library = VehicleProfileLibrary::new(&tmp);
        let mut doc = sample_doc();
        doc.delog_vehicle_profile = 99;
        fs::write(
            tmp.join("mavlink_local_position.json"),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();

        let err = library.load("mavlink_local_position").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        fs::remove_dir_all(tmp).unwrap();
    }
}
