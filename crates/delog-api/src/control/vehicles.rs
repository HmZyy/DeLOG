use delog_core::identity::{FieldId, SourceId};

use super::ScriptOwner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVehicleField {
    pub id: FieldId,
    pub path: String,
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
