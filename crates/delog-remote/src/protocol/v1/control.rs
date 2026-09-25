use serde::{Deserialize, Serialize};

use crate::handles::OpaqueId;

/// Version 1 control commands. All references to live resources are opaque.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlCommandDto {
    WindowOpen {
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    WorkspaceAddPlot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<OpaqueId>,
        direction: String,
    },
    WorkspaceSplit {
        plot: OpaqueId,
        direction: String,
    },
    WorkspaceClose {
        plot: OpaqueId,
    },
    WorkspaceEqualize {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<OpaqueId>,
    },
    SceneSetVisible {
        visible: bool,
    },
    PlaybackSet {
        #[serde(skip_serializing_if = "Option::is_none")]
        speed: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        follow_live: Option<bool>,
    },
    TraceAdd {
        plot: OpaqueId,
        field: OpaqueId,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        width_px: Option<f32>,
        mode: String,
    },
    TraceSet {
        trace: OpaqueId,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        width_px: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        visible: Option<bool>,
    },
    TraceRemove {
        trace: OpaqueId,
    },
    TraceClear {
        plot: OpaqueId,
    },
    AnnotationAdd {
        plot: OpaqueId,
        geometry: AnnotationGeometryDto,
        label: String,
        #[serde(default)]
        style: AnnotationStyleDto,
    },
    AnnotationSet {
        annotation: OpaqueId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        geometry: Option<AnnotationGeometryDto>,
        #[serde(default)]
        style: AnnotationStyleDto,
    },
    AnnotationRemove {
        annotation: OpaqueId,
    },
    MarkerAdd {
        time_ns: i64,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    MarkerSet {
        marker: OpaqueId,
        #[serde(skip_serializing_if = "Option::is_none")]
        time_ns: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    MarkerRemove {
        marker: OpaqueId,
    },
    VehicleAdd {
        source: OpaqueId,
        label: String,
        show: bool,
        show_path: bool,
        position: VehiclePositionDto,
        orientation: VehicleOrientationDto,
        model: String,
        color: String,
        path_color: String,
        scale: f32,
    },
    VehicleSet {
        vehicle: OpaqueId,
        patch: VehiclePatchDto,
    },
    VehicleRemove {
        vehicle: OpaqueId,
    },
    VehicleProfileList,
    VehicleProfileSave {
        name: String,
        vehicle: OpaqueId,
    },
    VehicleProfileLoad {
        name: String,
    },
    VehicleProfileApply {
        name: String,
        source: OpaqueId,
    },
    VehicleProfileDelete {
        name: String,
    },
    LayoutList,
    LayoutSave {
        name: String,
    },
    LayoutLoad {
        name: String,
    },
    LayoutDelete {
        name: String,
    },
    LayoutRename {
        from: String,
        to: String,
    },
    LayoutDuplicate {
        from: String,
        to: String,
    },
    LayoutImport {
        path: String,
    },
    LayoutExport {
        name: String,
        path: String,
    },
    LayoutClear,
    LayoutCurrent,
    LayoutApply {
        json: String,
    },
    RemoveOwned,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointDto {
    pub time_ns: i64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnnotationGeometryDto {
    Text { at: PointDto },
    Segment { from: PointDto, to: PointDto },
    Rect { a: PointDto, b: PointDto },
    Ellipse { a: PointDto, b: PointDto },
    HLine { y: f64 },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationStyleDto {
    pub color: Option<String>,
    pub stroke_px: Option<f32>,
    pub fill_opacity: Option<f32>,
    pub font_px: Option<f32>,
    pub arrow: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VehicleNedReferenceDto {
    Manual {
        lat_deg: f64,
        lon_deg: f64,
        alt_m: f64,
    },
    Fields {
        lat: OpaqueId,
        lon: OpaqueId,
        alt: OpaqueId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VehiclePositionDto {
    Ned {
        north: OpaqueId,
        east: OpaqueId,
        down: OpaqueId,
        #[serde(skip_serializing_if = "Option::is_none")]
        reference: Option<VehicleNedReferenceDto>,
    },
    Gps {
        lat: OpaqueId,
        lon: OpaqueId,
        alt: OpaqueId,
        lat_lon_dege7: bool,
        alt_mm: bool,
        alt_offset_m: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VehicleOrientationDto {
    Static,
    Euler {
        roll: OpaqueId,
        pitch: OpaqueId,
        yaw: OpaqueId,
        degrees: bool,
    },
    Quat {
        w: OpaqueId,
        x: OpaqueId,
        y: OpaqueId,
        z: OpaqueId,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VehiclePatchDto {
    pub label: Option<String>,
    pub show: Option<bool>,
    pub show_path: Option<bool>,
    pub position: Option<VehiclePositionDto>,
    pub orientation: Option<VehicleOrientationDto>,
    pub model: Option<String>,
    pub color: Option<String>,
    pub path_color: Option<String>,
    pub scale: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlResultDto {
    Unit,
    Resource {
        handle: OpaqueId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<OpaqueId>,
    },
    Names {
        names: Vec<String>,
    },
    Layout {
        json: String,
    },
    LoadReport {
        ambiguous: Vec<LayoutFieldIssueDto>,
        unresolved: Vec<String>,
        warnings: Vec<String>,
    },
    VehicleProfile {
        profile: VehicleProfileDto,
    },
    Removed {
        ui_resources: usize,
        publications: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutFieldIssueDto {
    pub field: String,
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VehicleProfileDto {
    pub name: String,
    pub label: String,
    pub show: bool,
    pub show_path: bool,
    pub position: serde_json::Value,
    pub orientation: serde_json::Value,
    pub model: String,
    pub color: String,
    pub path_color: String,
    pub scale: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlStateDto {
    pub windows: Vec<WindowStateDto>,
    pub plots: Vec<PlotStateDto>,
    pub traces: Vec<TraceStateDto>,
    pub annotations: Vec<AnnotationStateDto>,
    pub markers: Vec<MarkerStateDto>,
    pub vehicles: Vec<VehicleStateDto>,
    pub layout_names: Vec<String>,
    pub current_layout: String,
    pub playback: PlaybackStateDto,
    pub scene_visible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowStateDto {
    pub handle: OpaqueId,
    pub title: String,
    pub owner: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlotStateDto {
    pub handle: OpaqueId,
    pub window: OpaqueId,
    pub label: String,
    pub owner: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceStateDto {
    pub handle: OpaqueId,
    pub plot: OpaqueId,
    pub field: String,
    pub color: String,
    pub width_px: f32,
    pub mode: String,
    pub visible: bool,
    pub owner: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationStateDto {
    pub handle: OpaqueId,
    pub plot: OpaqueId,
    pub geometry: AnnotationGeometryDto,
    pub label: String,
    pub color: String,
    pub owner: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkerStateDto {
    pub handle: OpaqueId,
    pub time_ns: i64,
    pub label: String,
    pub color: String,
    pub note: String,
    pub origin: String,
    pub owner: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VehicleStateDto {
    pub handle: OpaqueId,
    pub source: String,
    pub label: String,
    pub show: bool,
    pub show_path: bool,
    /// Display metadata; commands still require catalog field handles.
    pub position: serde_json::Value,
    pub orientation: serde_json::Value,
    pub model: String,
    pub color: String,
    pub path_color: String,
    pub scale: f32,
    pub owner: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackStateDto {
    pub speed: f64,
    pub follow_live: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlBatchDto {
    pub commands: Vec<ControlCommandDto>,
}
