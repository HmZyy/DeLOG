#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use delog_core::identity::SourceId;
use delog_core::snapshot::StoreSnapshot;
use serde::{Deserialize, Serialize};

use crate::layout;
use crate::vehicle::VehicleConfig;

pub const VEHICLE_PROFILE_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VehicleProfileDoc {
    pub delog_vehicle_profile: u32,
    pub name: String,
    pub vehicle: crate::layout::VehicleLayout,
}

impl VehicleProfileDoc {
    pub fn from_config(
        name: &str,
        config: &VehicleConfig,
        snapshot: &StoreSnapshot,
    ) -> Option<Self> {
        Some(Self {
            delog_vehicle_profile: VEHICLE_PROFILE_VERSION,
            name: name.trim().to_owned(),
            vehicle: layout::vehicle_config_to_layout(config, snapshot)?,
        })
    }

    pub fn to_config(&self, snapshot: &StoreSnapshot) -> Option<VehicleConfig> {
        if self.delog_vehicle_profile != VEHICLE_PROFILE_VERSION {
            return None;
        }
        layout::vehicle_config_from_layout(&self.vehicle, snapshot)
    }

    pub fn to_config_for_source(
        &self,
        snapshot: &StoreSnapshot,
        source: SourceId,
    ) -> Option<VehicleConfig> {
        if self.delog_vehicle_profile != VEHICLE_PROFILE_VERSION {
            return None;
        }
        layout::vehicle_config_from_layout_for_source(&self.vehicle, snapshot, source)
    }
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
        crate::layout::config_dir().map(|dir| Self::new(dir.join("vehicle_profiles")))
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
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                if let Ok(name) = sanitize_name(stem)
                    && name == stem
                {
                    profiles.push(name);
                }
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
        Ok(doc)
    }

    pub fn save(&self, name: &str, doc: &VehicleProfileDoc) -> io::Result<()> {
        let path = self.profile_path(name)?;
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

    use crate::layout::{FieldRef, ModelLayout, OriLayout, PosLayout, VehicleLayout};
    use crate::vehicle::{ModelKind, OriMapping, PosMapping, VehicleConfig};

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
                label: "Vehicle".to_owned(),
                show: true,
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
            source,
            label: "Rover".to_owned(),
            show: true,
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
                label: "Rover".to_owned(),
                show: true,
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
