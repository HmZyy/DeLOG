#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const VEHICLE_PROFILE_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VehicleProfileDoc {
    pub delog_vehicle_profile: u32,
    pub name: String,
    pub vehicle: crate::layout::VehicleLayout,
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
                profiles.push(stem.to_owned());
            }
        }
        profiles.sort();
        Ok(profiles)
    }

    pub fn load(&self, name: &str) -> io::Result<VehicleProfileDoc> {
        let path = self.profile_path(name)?;
        let json = fs::read_to_string(path)?;
        serde_json::from_str(&json).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
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

    use crate::layout::{FieldRef, ModelLayout, OriLayout, PosLayout, VehicleLayout};

    use super::*;

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

    fn sample_doc() -> VehicleProfileDoc {
        VehicleProfileDoc {
            delog_vehicle_profile: VEHICLE_PROFILE_VERSION,
            name: "MAVLink Local Position".to_owned(),
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

        library.save("MAVLink Local Position", &doc).unwrap();

        assert_eq!(library.list().unwrap(), vec!["MAVLink Local Position"]);
        assert_eq!(library.load("MAVLink Local Position").unwrap(), doc);

        fs::remove_dir_all(tmp).unwrap();
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
}
