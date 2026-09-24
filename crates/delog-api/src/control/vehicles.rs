use delog_core::identity::{FieldId, SourceId};

use super::ScriptOwner;
use crate::catalog::FieldMatch;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVehicleField {
    pub id: FieldId,
    pub path: String,
}

impl From<FieldMatch> for ResolvedVehicleField {
    fn from(field: FieldMatch) -> Self {
        Self {
            id: field.field_id,
            path: format!(
                "{}/{}/{}",
                field.source_label, field.topic_name, field.field_name
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VehicleNedReference {
    Manual {
        lat_deg: f64,
        lon_deg: f64,
        alt_m: f64,
    },
    Fields {
        lat: ResolvedVehicleField,
        lon: ResolvedVehicleField,
        alt: ResolvedVehicleField,
    },
}

impl VehicleNedReference {
    pub fn manual(lat_deg: f64, lon_deg: f64, alt_m: f64) -> Result<Self> {
        validate_f64(lat_deg, "lat_deg")?;
        validate_f64(lon_deg, "lon_deg")?;
        validate_f64(alt_m, "alt_m")?;
        if !(-90.0..=90.0).contains(&lat_deg) {
            return Err(Error::invalid_input("lat_deg must be between -90 and 90"));
        }
        if !(-180.0..=180.0).contains(&lon_deg) {
            return Err(Error::invalid_input("lon_deg must be between -180 and 180"));
        }
        Ok(Self::Manual {
            lat_deg,
            lon_deg,
            alt_m,
        })
    }

    pub fn fields(
        lat: ResolvedVehicleField,
        lon: ResolvedVehicleField,
        alt: ResolvedVehicleField,
    ) -> Self {
        Self::Fields { lat, lon, alt }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Manual {
                lat_deg,
                lon_deg,
                alt_m,
            } => Self::manual(*lat_deg, *lon_deg, *alt_m).map(|_| ()),
            Self::Fields { .. } => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VehiclePosition {
    Ned {
        north: ResolvedVehicleField,
        east: ResolvedVehicleField,
        down: ResolvedVehicleField,
        reference: Option<VehicleNedReference>,
    },
    Gps {
        lat: ResolvedVehicleField,
        lon: ResolvedVehicleField,
        alt: ResolvedVehicleField,
        lat_lon_dege7: bool,
        alt_mm: bool,
        alt_offset_m: f64,
    },
}

impl VehiclePosition {
    pub fn gps(
        lat: ResolvedVehicleField,
        lon: ResolvedVehicleField,
        alt: ResolvedVehicleField,
        lat_lon_dege7: bool,
        alt_mm: bool,
        alt_offset_m: f64,
    ) -> Result<Self> {
        validate_f64(alt_offset_m, "alt_offset_m")?;
        Ok(Self::Gps {
            lat,
            lon,
            alt,
            lat_lon_dege7,
            alt_mm,
            alt_offset_m,
        })
    }

    pub fn ned(
        north: ResolvedVehicleField,
        east: ResolvedVehicleField,
        down: ResolvedVehicleField,
        reference: Option<VehicleNedReference>,
    ) -> Result<Self> {
        if let Some(reference) = &reference {
            reference.validate()?;
        }
        Ok(Self::Ned {
            north,
            east,
            down,
            reference,
        })
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Ned { reference, .. } => {
                if let Some(reference) = reference {
                    reference.validate()?;
                }
                Ok(())
            }
            Self::Gps { alt_offset_m, .. } => validate_f64(*alt_offset_m, "alt_offset_m"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VehicleOrientation {
    Static,
    Euler {
        roll: ResolvedVehicleField,
        pitch: ResolvedVehicleField,
        yaw: ResolvedVehicleField,
        degrees: bool,
    },
    Quat {
        w: ResolvedVehicleField,
        x: ResolvedVehicleField,
        y: ResolvedVehicleField,
        z: ResolvedVehicleField,
    },
}

impl VehicleOrientation {
    pub fn static_orientation() -> Self {
        Self::Static
    }

    pub fn euler(
        roll: ResolvedVehicleField,
        pitch: ResolvedVehicleField,
        yaw: ResolvedVehicleField,
        degrees: bool,
    ) -> Self {
        Self::Euler {
            roll,
            pitch,
            yaw,
            degrees,
        }
    }

    pub fn quaternion(
        w: ResolvedVehicleField,
        x: ResolvedVehicleField,
        y: ResolvedVehicleField,
        z: ResolvedVehicleField,
    ) -> Self {
        Self::Quat { w, x, y, z }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VehicleModel {
    None,
    Quad,
    FixedWing,
    DeltaWing,
    Cone,
    Sphere,
    Cube,
    CustomGlb(String),
}

impl VehicleModel {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "none" => Ok(Self::None),
            "quad" => Ok(Self::Quad),
            "fixedwing" => Ok(Self::FixedWing),
            "deltawing" => Ok(Self::DeltaWing),
            "cone" => Ok(Self::Cone),
            "sphere" => Ok(Self::Sphere),
            "cube" => Ok(Self::Cube),
            path if path.ends_with(".glb") => Ok(Self::CustomGlb(path.to_owned())),
            _ => Err(Error::invalid_input(format!(
                "vehicle model must be 'none', 'quad', 'fixedwing', 'deltawing', 'cone', 'sphere', 'cube', or a .glb path, got {name:?}"
            ))),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Quad => "quad",
            Self::FixedWing => "fixedwing",
            Self::DeltaWing => "deltawing",
            Self::Cone => "cone",
            Self::Sphere => "sphere",
            Self::Cube => "cube",
            Self::CustomGlb(path) => path,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VehicleSpec {
    pub source_id: SourceId,
    pub source: String,
    pub label: String,
    pub show: bool,
    pub show_path: bool,
    pub position: VehiclePosition,
    pub orientation: VehicleOrientation,
    pub model: VehicleModel,
    pub color: [f32; 4],
    pub path_color: [f32; 4],
    pub scale: f32,
    pub owner: Option<ScriptOwner>,
}

impl VehicleSpec {
    pub fn validate(&self) -> Result<()> {
        self.position.validate()?;
        validate_color(self.color, "vehicle color")?;
        validate_color(self.path_color, "vehicle path color")?;
        validate_scale(self.scale)
    }

    pub fn apply_patch(&mut self, patch: VehiclePatch) -> Result<()> {
        patch.validate()?;
        if let Some(value) = patch.label {
            self.label = value;
        }
        if let Some(value) = patch.show {
            self.show = value;
        }
        if let Some(value) = patch.show_path {
            self.show_path = value;
        }
        if let Some(value) = patch.position {
            self.position = value;
        }
        if let Some(value) = patch.orientation {
            self.orientation = value;
        }
        if let Some(value) = patch.model {
            self.model = value;
        }
        if let Some(value) = patch.color {
            self.color = value;
        }
        if let Some(value) = patch.path_color {
            self.path_color = value;
        }
        if let Some(value) = patch.scale {
            self.scale = value;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct VehiclePatch {
    pub label: Option<String>,
    pub show: Option<bool>,
    pub show_path: Option<bool>,
    pub position: Option<VehiclePosition>,
    pub orientation: Option<VehicleOrientation>,
    pub model: Option<VehicleModel>,
    pub color: Option<[f32; 4]>,
    pub path_color: Option<[f32; 4]>,
    pub scale: Option<f32>,
}

impl VehiclePatch {
    pub fn validate(&self) -> Result<()> {
        if let Some(position) = &self.position {
            position.validate()?;
        }
        if let Some(color) = self.color {
            validate_color(color, "vehicle color")?;
        }
        if let Some(path_color) = self.path_color {
            validate_color(path_color, "vehicle path color")?;
        }
        if let Some(scale) = self.scale {
            validate_scale(scale)?;
        }
        Ok(())
    }
}

fn validate_f64(value: f64, name: &str) -> Result<()> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(Error::invalid_input(format!("{name} must be finite")))
    }
}

fn validate_color(color: [f32; 4], name: &str) -> Result<()> {
    if color
        .iter()
        .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
    {
        Ok(())
    } else {
        Err(Error::invalid_input(format!(
            "{name} components must be finite and between 0 and 1"
        )))
    }
}

fn validate_scale(scale: f32) -> Result<()> {
    if scale.is_finite() && scale > 0.0 {
        Ok(())
    } else {
        Err(Error::invalid_input("vehicle scale must be finite and > 0"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VehicleInfo {
    pub id: u64,
    pub index: usize,
    pub spec: VehicleSpec,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VehicleFilter {
    Id(u64),
    Index(usize),
    Label(String),
    Source(SourceId),
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VehicleRequest {
    List,
    Add(VehicleSpec),
    Set { id: u64, patch: VehiclePatch },
    Remove(VehicleFilter),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileFieldRef {
    pub topic: String,
    pub field: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProfileNedReference {
    Manual {
        lat_deg: f64,
        lon_deg: f64,
        alt_m: f64,
    },
    Fields {
        lat: ProfileFieldRef,
        lon: ProfileFieldRef,
        alt: ProfileFieldRef,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProfilePosition {
    Ned {
        north: ProfileFieldRef,
        east: ProfileFieldRef,
        down: ProfileFieldRef,
        reference: Option<ProfileNedReference>,
    },
    Gps {
        lat: ProfileFieldRef,
        lon: ProfileFieldRef,
        alt: ProfileFieldRef,
        lat_lon_dege7: bool,
        alt_mm: bool,
        alt_offset_m: f64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProfileOrientation {
    Static,
    Euler {
        roll: ProfileFieldRef,
        pitch: ProfileFieldRef,
        yaw: ProfileFieldRef,
        degrees: bool,
    },
    Quat {
        w: ProfileFieldRef,
        x: ProfileFieldRef,
        y: ProfileFieldRef,
        z: ProfileFieldRef,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct VehicleProfileInfo {
    pub name: String,
    pub label: String,
    pub show: bool,
    pub show_path: bool,
    pub position: ProfilePosition,
    pub orientation: ProfileOrientation,
    pub model: VehicleModel,
    pub color: [f32; 4],
    pub path_color: [f32; 4],
    pub scale: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VehicleProfileRequest {
    List,
    Save {
        name: String,
        vehicle_id: u64,
    },
    Load {
        name: String,
    },
    Apply {
        name: String,
        source_id: SourceId,
        source: String,
        owner: Option<ScriptOwner>,
    },
    Delete {
        name: String,
    },
}

pub fn validate_profile_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(Error::invalid_input(
            "vehicle profile name must not be empty or contain path separators/traversal",
        ));
    }
    Ok(name.to_owned())
}
