//! Layouts store fields as `topic.field`, never as runtime IDs or source
//! labels, so the same plot/vehicle setup can be reused across logs.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use delog_core::diagnostics::Diag;
use delog_core::identity::{FieldId, SourceId};
use delog_core::snapshot::StoreSnapshot;
use egui::Color32;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::settings::AppSettings;
use crate::scene3d::vehicle::{GeoRef, ModelKind, NedReference, OriMapping, PosMapping, VehicleConfig};

const APP_ID: &str = "DeLOG";
pub(crate) const LAYOUT_VERSION: u32 = 1;

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutDoc {
    pub delog_layout: u32,
    pub name: String,
    pub playback: PlaybackLayout,
    pub workspace: WorkspaceLayout,
    pub vehicles: Vec<VehicleLayout>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldRef {
    pub topic: String,
    pub field: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PlaybackLayout {
    pub speed: f64,
    pub follow_live: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceLayout {
    pub root: LayoutNode,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutNode {
    Plot {
        traces: Vec<TraceLayout>,
        #[serde(default = "default_true")]
        show_legend: bool,
        #[serde(default = "default_true")]
        show_tooltip: bool,
    },
    Scene3d(SceneLayout),
    Split {
        split: SplitLayout,
        children: Vec<LayoutNode>,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitLayout {
    Tabs,
    Horizontal,
    Vertical,
    Grid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceLayout {
    pub field: FieldRef,
    pub color: [f32; 4],
    pub width_px: f32,
    pub mode: TraceModeLayout,
    pub visible: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceModeLayout {
    Line,
    Scatter,
    Step,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SceneLayout {
    pub camera: CameraLayout,
    pub tracked_vehicle: Option<usize>,
    /// Defaults true so layouts saved before this field decode to the
    /// up-to-playhead behavior.
    #[serde(default = "default_trail_to_playhead")]
    pub trail_to_playhead: bool,
}

fn default_trail_to_playhead() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CameraLayout {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VehicleLayout {
    pub label: String,
    pub show: bool,
    pub model: ModelLayout,
    pub color: [u8; 4],
    pub path_color: [u8; 4],
    pub scale: f32,
    pub position: PosLayout,
    pub orientation: OriLayout,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelLayout {
    Quad,
    FixedWing,
    DeltaWing,
    Cone,
    CustomGlb { path: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PosLayout {
    Ned {
        north: FieldRef,
        east: FieldRef,
        down: FieldRef,
        reference: Option<NedRefLayout>,
    },
    Gps {
        lat: FieldRef,
        lon: FieldRef,
        alt: FieldRef,
        /// degE7 (scale 1e-7 to degrees).
        #[serde(default)]
        lat_lon_dege7: bool,
        /// mm (scale 1e-3 to metres).
        #[serde(default)]
        alt_mm: bool,
        /// metres, up-positive.
        #[serde(default)]
        alt_offset_m: f64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NedRefLayout {
    Manual {
        lat_deg: f64,
        lon_deg: f64,
        alt_m: f64,
    },
    Fields {
        lat: FieldRef,
        lon: FieldRef,
        alt: FieldRef,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriLayout {
    Static,
    Euler {
        roll: FieldRef,
        pitch: FieldRef,
        yaw: FieldRef,
        degrees: bool,
    },
    Quat {
        w: FieldRef,
        x: FieldRef,
        y: FieldRef,
        z: FieldRef,
    },
}

#[derive(Clone, Debug)]
pub struct AmbiguousField {
    pub field: FieldRef,
    pub candidates: Vec<SourceChoice>,
    pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct SourceChoice {
    pub source: SourceId,
    pub label: String,
}

#[derive(Clone, Debug)]
pub enum LayoutError {
    Io(String),
    Json(String),
    UnsupportedVersion(u32),
    NoStorageDir,
    MissingVersion,
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "layout IO error: {e}"),
            Self::Json(e) => write!(f, "layout JSON error: {e}"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported layout version {v}"),
            Self::NoStorageDir => write!(f, "no layout storage directory available"),
            Self::MissingVersion => write!(f, "layout JSON is missing `delog_layout`"),
        }
    }
}

pub fn layout_dir() -> Result<PathBuf, LayoutError> {
    let Some(base) = storage_dir(APP_ID) else {
        return Err(LayoutError::NoStorageDir);
    };
    Ok(base.join("layouts"))
}

pub fn list_layouts() -> Vec<String> {
    let Ok(dir) = layout_dir() else {
        return Vec::new();
    };
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names = read
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|s| s.to_str()) == Some("json"))
                .then_some(path)?
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

pub fn delete_named(name: &str) -> Result<(), LayoutError> {
    let path = named_layout_path(name)?;
    fs::remove_file(path).map_err(|e| LayoutError::Io(e.to_string()))
}

pub fn duplicate_named(from: &str, to: &str) -> Result<(), LayoutError> {
    let mut doc = import_doc(&named_layout_path(from)?)?;
    doc.name = sanitize_name(to);
    save_named(to, &doc)
}

pub fn rename_named(from: &str, to: &str) -> Result<(), LayoutError> {
    let mut doc = import_doc(&named_layout_path(from)?)?;
    doc.name = sanitize_name(to);
    save_named(to, &doc)?;
    let from_path = named_layout_path(from)?;
    let to_path = named_layout_path(to)?;
    if from_path != to_path {
        fs::remove_file(from_path).map_err(|e| LayoutError::Io(e.to_string()))?;
    }
    Ok(())
}

pub fn save_named(name: &str, doc: &LayoutDoc) -> Result<(), LayoutError> {
    let path = named_layout_path(name)?;
    let dir = path
        .parent()
        .ok_or_else(|| LayoutError::Io("layout path has no parent".into()))?;
    fs::create_dir_all(dir).map_err(|e| LayoutError::Io(e.to_string()))?;
    let json = doc_json(doc)?;
    write_json_atomic(&path, &json)
}

pub fn export_doc(path: &Path, doc: &LayoutDoc) -> Result<(), LayoutError> {
    let json = doc_json(doc)?;
    write_json_atomic(path, &json)
}

pub fn save_session_json(json: &str) -> Result<(), LayoutError> {
    let Some(base) = storage_dir(APP_ID) else {
        return Err(LayoutError::NoStorageDir);
    };
    write_json_atomic(&base.join("session.json"), json)
}

#[cfg_attr(not(feature = "scripting"), allow(dead_code))]
pub fn config_dir() -> Option<std::path::PathBuf> {
    storage_dir(APP_ID)
}

/// Separate from layouts and `session.json` so loading a layout never changes
/// user preferences.
fn settings_path() -> Result<PathBuf, LayoutError> {
    let Some(base) = storage_dir(APP_ID) else {
        return Err(LayoutError::NoStorageDir);
    };
    Ok(base.join("settings.json"))
}

pub fn load_app_settings() -> AppSettings {
    match settings_path() {
        Ok(path) => load_app_settings_at(&path),
        Err(_) => AppSettings::default(),
    }
}

pub fn save_app_settings(settings: &AppSettings) -> Result<(), LayoutError> {
    save_app_settings_at(&settings_path()?, settings)
}

fn load_app_settings_at(path: &Path) -> AppSettings {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_app_settings_at(path: &Path, settings: &AppSettings) -> Result<(), LayoutError> {
    let json =
        serde_json::to_string_pretty(settings).map_err(|e| LayoutError::Json(e.to_string()))?;
    write_json_atomic(path, &json)
}

pub fn doc_json(doc: &LayoutDoc) -> Result<String, LayoutError> {
    serde_json::to_string_pretty(doc).map_err(|e| LayoutError::Json(e.to_string()))
}

fn write_json_atomic(path: &Path, json: &str) -> Result<(), LayoutError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| LayoutError::Io(e.to_string()))?;
    }
    let tmp = path.with_extension(format!(
        "{}tmp",
        path.extension()
            .and_then(|s| s.to_str())
            .map(|s| format!("{s}."))
            .unwrap_or_default()
    ));
    fs::write(&tmp, json).map_err(|e| LayoutError::Io(e.to_string()))?;
    fs::rename(&tmp, path).map_err(|e| LayoutError::Io(e.to_string()))?;
    Ok(())
}

pub fn load_named_doc(name: &str) -> Result<LayoutDoc, LayoutError> {
    import_doc(&named_layout_path(name)?)
}

pub fn import_doc(path: &Path) -> Result<LayoutDoc, LayoutError> {
    let bytes = fs::read_to_string(path).map_err(|e| LayoutError::Io(e.to_string()))?;
    decode_doc(&bytes)
}

pub fn decode_doc(json: &str) -> Result<LayoutDoc, LayoutError> {
    let value: Value = serde_json::from_str(json).map_err(|e| LayoutError::Json(e.to_string()))?;
    let value = migrate_to_current(value)?;
    serde_json::from_value(value).map_err(|e| LayoutError::Json(e.to_string()))
}

fn migrate_to_current(value: Value) -> Result<Value, LayoutError> {
    let version = value
        .get("delog_layout")
        .and_then(Value::as_u64)
        .ok_or(LayoutError::MissingVersion)? as u32;
    match version {
        LAYOUT_VERSION => Ok(value),
        other => Err(LayoutError::UnsupportedVersion(other)),
    }
}

fn sanitize_name(name: &str) -> String {
    let out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "default".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn storage_dir(app_id: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join(app_id).join("data"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(PathBuf::from).map(|p| {
            p.join("Library")
                .join("Application Support")
                .join(app_id.replace(|c: char| c.is_ascii_whitespace(), "-"))
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
            .map(|p| p.join(app_id.to_lowercase().replace(char::is_whitespace, "")))
    }
}

fn named_layout_path(name: &str) -> Result<PathBuf, LayoutError> {
    Ok(layout_dir()?.join(format!("{}.json", sanitize_name(name))))
}

pub(crate) fn vehicle_to_layout(v: &VehicleConfig, snapshot: &StoreSnapshot) -> Option<VehicleLayout> {
    Some(VehicleLayout {
        label: v.label.clone(),
        show: v.show,
        model: model_to_layout(&v.model),
        color: color_to_rgba(v.color),
        path_color: color_to_rgba(v.path_color),
        scale: v.scale,
        position: pos_to_layout(&v.pos, snapshot)?,
        orientation: ori_to_layout(&v.ori, snapshot)?,
    })
}

#[allow(dead_code)]
pub fn vehicle_config_to_layout(
    v: &VehicleConfig,
    snapshot: &StoreSnapshot,
) -> Option<VehicleLayout> {
    vehicle_to_layout(v, snapshot)
}

#[allow(dead_code)]
pub fn vehicle_config_from_layout(
    v: &VehicleLayout,
    snapshot: &StoreSnapshot,
) -> Option<VehicleConfig> {
    let choices = HashMap::new();
    let mut resolver = Resolver {
        snapshot,
        choices: &choices,
        diagnostics: Vec::new(),
        ambiguities: BTreeMap::new(),
        collect_ambiguities: false,
    };
    vehicle_from_layout(v, &mut resolver)
}

#[allow(dead_code)]
pub fn vehicle_config_from_layout_for_source(
    v: &VehicleLayout,
    snapshot: &StoreSnapshot,
    source: SourceId,
) -> Option<VehicleConfig> {
    let mut choices = HashMap::new();
    collect_vehicle_field_choices(v, source, &mut choices);
    let mut resolver = Resolver {
        snapshot,
        choices: &choices,
        diagnostics: Vec::new(),
        ambiguities: BTreeMap::new(),
        collect_ambiguities: false,
    };
    vehicle_from_layout(v, &mut resolver)
}

pub(crate) fn field_ref(snapshot: &StoreSnapshot, field: FieldId) -> Option<FieldRef> {
    let field_entry = snapshot
        .fields
        .get(field.index())
        .filter(|f| f.id == field)?;
    let topic = snapshot.topic(field_entry.topic)?;
    Some(FieldRef {
        topic: topic.entry.name.clone(),
        field: field_entry.name.clone(),
    })
}

pub(crate) fn collect_field_refs(doc: &LayoutDoc, resolver: &mut Resolver<'_>) {
    collect_node_field_refs(&doc.workspace.root, resolver);
    for vehicle in &doc.vehicles {
        collect_pos_field_refs(&vehicle.position, resolver);
        collect_ori_field_refs(&vehicle.orientation, resolver);
    }
}

fn collect_vehicle_field_choices(
    vehicle: &VehicleLayout,
    source: SourceId,
    choices: &mut HashMap<FieldRef, SourceId>,
) {
    collect_pos_field_choices(&vehicle.position, source, choices);
    collect_ori_field_choices(&vehicle.orientation, source, choices);
}

fn collect_pos_field_choices(
    pos: &PosLayout,
    source: SourceId,
    choices: &mut HashMap<FieldRef, SourceId>,
) {
    match pos {
        PosLayout::Ned {
            north,
            east,
            down,
            reference,
        } => {
            choices.insert(north.clone(), source);
            choices.insert(east.clone(), source);
            choices.insert(down.clone(), source);
            if let Some(NedRefLayout::Fields { lat, lon, alt }) = reference {
                choices.insert(lat.clone(), source);
                choices.insert(lon.clone(), source);
                choices.insert(alt.clone(), source);
            }
        }
        PosLayout::Gps { lat, lon, alt, .. } => {
            choices.insert(lat.clone(), source);
            choices.insert(lon.clone(), source);
            choices.insert(alt.clone(), source);
        }
    }
}

fn collect_ori_field_choices(
    ori: &OriLayout,
    source: SourceId,
    choices: &mut HashMap<FieldRef, SourceId>,
) {
    match ori {
        OriLayout::Static => {}
        OriLayout::Euler {
            roll, pitch, yaw, ..
        } => {
            choices.insert(roll.clone(), source);
            choices.insert(pitch.clone(), source);
            choices.insert(yaw.clone(), source);
        }
        OriLayout::Quat { w, x, y, z } => {
            choices.insert(w.clone(), source);
            choices.insert(x.clone(), source);
            choices.insert(y.clone(), source);
            choices.insert(z.clone(), source);
        }
    }
}

fn collect_node_field_refs(node: &LayoutNode, resolver: &mut Resolver<'_>) {
    match node {
        LayoutNode::Plot { traces, .. } => {
            for trace in traces {
                let _ = resolver.resolve(&trace.field);
            }
        }
        LayoutNode::Scene3d(_) => {}
        LayoutNode::Split { children, .. } => {
            for child in children {
                collect_node_field_refs(child, resolver);
            }
        }
    }
}

fn collect_pos_field_refs(pos: &PosLayout, resolver: &mut Resolver<'_>) {
    match pos {
        PosLayout::Ned {
            north,
            east,
            down,
            reference,
        } => {
            let _ = resolver.resolve(north);
            let _ = resolver.resolve(east);
            let _ = resolver.resolve(down);
            if let Some(NedRefLayout::Fields { lat, lon, alt }) = reference {
                let _ = resolver.resolve(lat);
                let _ = resolver.resolve(lon);
                let _ = resolver.resolve(alt);
            }
        }
        PosLayout::Gps { lat, lon, alt, .. } => {
            let _ = resolver.resolve(lat);
            let _ = resolver.resolve(lon);
            let _ = resolver.resolve(alt);
        }
    }
}

fn collect_ori_field_refs(ori: &OriLayout, resolver: &mut Resolver<'_>) {
    match ori {
        OriLayout::Static => {}
        OriLayout::Euler {
            roll, pitch, yaw, ..
        } => {
            let _ = resolver.resolve(roll);
            let _ = resolver.resolve(pitch);
            let _ = resolver.resolve(yaw);
        }
        OriLayout::Quat { w, x, y, z } => {
            let _ = resolver.resolve(w);
            let _ = resolver.resolve(x);
            let _ = resolver.resolve(y);
            let _ = resolver.resolve(z);
        }
    }
}

pub(crate) struct Resolver<'a> {
    pub(crate) snapshot: &'a StoreSnapshot,
    pub(crate) choices: &'a HashMap<FieldRef, SourceId>,
    pub(crate) diagnostics: Vec<Diag>,
    pub(crate) ambiguities: BTreeMap<FieldRef, AmbiguousField>,
    pub(crate) collect_ambiguities: bool,
}

impl Resolver<'_> {
    pub(crate) fn resolve(&mut self, key: &FieldRef) -> Option<FieldId> {
        if let Some(&source) = self.choices.get(key) {
            return self.resolve_in_source(source, key).or_else(|| {
                self.diagnostics.push(layout_warning(format!(
                    "{}.{} no longer exists in selected source",
                    key.topic, key.field
                )));
                None
            });
        }

        let live_sources = self
            .snapshot
            .sources
            .iter()
            .filter(|s| !s.entry.removed)
            .collect::<Vec<_>>();
        if live_sources.len() == 1 {
            let source = live_sources[0].entry.id;
            let got = self.resolve_in_source(source, key);
            if got.is_none() {
                self.diagnostics.push(layout_warning(format!(
                    "{}.{} not found in loaded source",
                    key.topic, key.field
                )));
            }
            return got;
        }

        let matches = live_sources
            .iter()
            .filter_map(|source| {
                self.resolve_in_source(source.entry.id, key)
                    .map(|field| (source.entry.id, source.entry.label.clone(), field))
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [(_, _, field)] => Some(*field),
            [] => {
                self.diagnostics.push(layout_warning(format!(
                    "{}.{} not found in loaded sources",
                    key.topic, key.field
                )));
                None
            }
            _ if self.collect_ambiguities => {
                self.ambiguities
                    .entry(key.clone())
                    .or_insert_with(|| AmbiguousField {
                        field: key.clone(),
                        candidates: matches
                            .iter()
                            .map(|(source, label, _)| SourceChoice {
                                source: *source,
                                label: label.clone(),
                            })
                            .collect(),
                        selected: 0,
                    });
                None
            }
            _ => None,
        }
    }

    pub(crate) fn resolve_in_source(&self, source: SourceId, key: &FieldRef) -> Option<FieldId> {
        for topic_id in self.snapshot.source(source)?.topics.iter().copied() {
            let topic = self.snapshot.topic(topic_id)?;
            if topic.entry.removed || topic.entry.name != key.topic {
                continue;
            }
            let field = self
                .snapshot
                .fields
                .iter()
                .find(|f| f.topic == topic_id && !f.removed && f.name == key.field)?;
            return Some(field.id);
        }
        None
    }
}

pub(crate) fn vehicle_from_layout(v: &VehicleLayout, resolver: &mut Resolver<'_>) -> Option<VehicleConfig> {
    let source = first_resolved_source(v, resolver)?;
    Some(VehicleConfig {
        source,
        label: v.label.clone(),
        show: v.show,
        pos: pos_from_layout(&v.position, resolver)?,
        ori: ori_from_layout(&v.orientation, resolver)?,
        model: model_from_layout(&v.model),
        color: rgba_to_color(v.color),
        path_color: rgba_to_color(v.path_color),
        scale: v.scale.max(0.01),
    })
}

fn first_resolved_source(v: &VehicleLayout, resolver: &mut Resolver<'_>) -> Option<SourceId> {
    let field = first_vehicle_field(v)?;
    let id = resolver.resolve(field)?;
    let topic = resolver.snapshot.fields.get(id.index())?.topic;
    Some(resolver.snapshot.topic(topic)?.entry.source)
}

fn first_vehicle_field(v: &VehicleLayout) -> Option<&FieldRef> {
    match &v.position {
        PosLayout::Ned { north, .. } => Some(north),
        PosLayout::Gps { lat, .. } => Some(lat),
    }
}

fn pos_to_layout(pos: &PosMapping, snapshot: &StoreSnapshot) -> Option<PosLayout> {
    match pos {
        PosMapping::Ned {
            north,
            east,
            down,
            reference,
        } => Some(PosLayout::Ned {
            north: field_ref(snapshot, *north)?,
            east: field_ref(snapshot, *east)?,
            down: field_ref(snapshot, *down)?,
            reference: match reference {
                None => None,
                Some(NedReference::Manual(r)) => Some(NedRefLayout::Manual {
                    lat_deg: r.lat_deg,
                    lon_deg: r.lon_deg,
                    alt_m: r.alt_m,
                }),
                Some(NedReference::Fields { lat, lon, alt }) => Some(NedRefLayout::Fields {
                    lat: field_ref(snapshot, *lat)?,
                    lon: field_ref(snapshot, *lon)?,
                    alt: field_ref(snapshot, *alt)?,
                }),
            },
        }),
        PosMapping::Gps {
            lat,
            lon,
            alt,
            lat_lon_dege7,
            alt_mm,
            alt_offset_m,
        } => Some(PosLayout::Gps {
            lat: field_ref(snapshot, *lat)?,
            lon: field_ref(snapshot, *lon)?,
            alt: field_ref(snapshot, *alt)?,
            lat_lon_dege7: *lat_lon_dege7,
            alt_mm: *alt_mm,
            alt_offset_m: *alt_offset_m,
        }),
    }
}

fn pos_from_layout(pos: &PosLayout, resolver: &mut Resolver<'_>) -> Option<PosMapping> {
    match pos {
        PosLayout::Ned {
            north,
            east,
            down,
            reference,
        } => Some(PosMapping::Ned {
            north: resolver.resolve(north)?,
            east: resolver.resolve(east)?,
            down: resolver.resolve(down)?,
            reference: match reference {
                None => None,
                Some(NedRefLayout::Manual {
                    lat_deg,
                    lon_deg,
                    alt_m,
                }) => Some(NedReference::Manual(GeoRef {
                    lat_deg: *lat_deg,
                    lon_deg: *lon_deg,
                    alt_m: *alt_m,
                })),
                Some(NedRefLayout::Fields { lat, lon, alt }) => Some(NedReference::Fields {
                    lat: resolver.resolve(lat)?,
                    lon: resolver.resolve(lon)?,
                    alt: resolver.resolve(alt)?,
                }),
            },
        }),
        PosLayout::Gps {
            lat,
            lon,
            alt,
            lat_lon_dege7,
            alt_mm,
            alt_offset_m,
        } => Some(PosMapping::Gps {
            lat: resolver.resolve(lat)?,
            lon: resolver.resolve(lon)?,
            alt: resolver.resolve(alt)?,
            lat_lon_dege7: *lat_lon_dege7,
            alt_mm: *alt_mm,
            alt_offset_m: *alt_offset_m,
        }),
    }
}

fn ori_to_layout(ori: &OriMapping, snapshot: &StoreSnapshot) -> Option<OriLayout> {
    match ori {
        OriMapping::Static => Some(OriLayout::Static),
        OriMapping::Euler {
            roll,
            pitch,
            yaw,
            degrees,
        } => Some(OriLayout::Euler {
            roll: field_ref(snapshot, *roll)?,
            pitch: field_ref(snapshot, *pitch)?,
            yaw: field_ref(snapshot, *yaw)?,
            degrees: *degrees,
        }),
        OriMapping::Quat { w, x, y, z } => Some(OriLayout::Quat {
            w: field_ref(snapshot, *w)?,
            x: field_ref(snapshot, *x)?,
            y: field_ref(snapshot, *y)?,
            z: field_ref(snapshot, *z)?,
        }),
    }
}

fn ori_from_layout(ori: &OriLayout, resolver: &mut Resolver<'_>) -> Option<OriMapping> {
    match ori {
        OriLayout::Static => Some(OriMapping::Static),
        OriLayout::Euler {
            roll,
            pitch,
            yaw,
            degrees,
        } => Some(OriMapping::Euler {
            roll: resolver.resolve(roll)?,
            pitch: resolver.resolve(pitch)?,
            yaw: resolver.resolve(yaw)?,
            degrees: *degrees,
        }),
        OriLayout::Quat { w, x, y, z } => Some(OriMapping::Quat {
            w: resolver.resolve(w)?,
            x: resolver.resolve(x)?,
            y: resolver.resolve(y)?,
            z: resolver.resolve(z)?,
        }),
    }
}

fn model_to_layout(model: &ModelKind) -> ModelLayout {
    match model {
        ModelKind::Quad => ModelLayout::Quad,
        ModelKind::FixedWing => ModelLayout::FixedWing,
        ModelKind::DeltaWing => ModelLayout::DeltaWing,
        ModelKind::Cone => ModelLayout::Cone,
        ModelKind::CustomGlb(path) => ModelLayout::CustomGlb {
            path: path.to_string_lossy().into_owned(),
        },
    }
}

fn model_from_layout(model: &ModelLayout) -> ModelKind {
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

fn rgba_to_color(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn layout_warning(message: String) -> Diag {
    Diag::warning("layout", message)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
