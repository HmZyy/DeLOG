//! Source-agnostic layout persistence (PLAN.md §14, LAY-01/02).
//!
//! Layouts deliberately store fields as `topic.field`, never as runtime IDs or
//! source labels, so the same plot/vehicle setup can be reused across logs.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use delog_core::diagnostics::Diag;
use delog_core::identity::{FieldId, SourceId};
use delog_core::snapshot::StoreSnapshot;
use egui::Color32;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::camera::OrbitCamera;
use crate::plot::{GhostTrace, PlotPane, TraceMode, TraceRef, ViewX};
use crate::settings::AppSettings;
use crate::vehicle::{GeoRef, ModelKind, NedReference, OriMapping, PosMapping, VehicleConfig};
use crate::workspace::{Pane, Scene3dPane, Workspace};

const APP_ID: &str = "DeLOG";
const LAYOUT_VERSION: u32 = 1;

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutDoc {
    pub delog_layout: u32,
    pub name: String,
    pub view: Option<ViewLayout>,
    pub playback: PlaybackLayout,
    pub workspace: WorkspaceLayout,
    pub vehicles: Vec<VehicleLayout>,
    #[serde(default)]
    pub favorites: Vec<FieldRef>,
    #[serde(default)]
    pub docks: BTreeMap<String, bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldRef {
    pub topic: String,
    pub field: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    Window,
    Full,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ViewLayout {
    pub mode: ViewMode,
    pub min_us: i64,
    pub max_us: i64,
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
        /// Lat/lon stored as `degE7` integers (scale 1e-7 to degrees).
        #[serde(default)]
        lat_lon_dege7: bool,
        /// Altitude stored in millimetres (scale 1e-3 to metres).
        #[serde(default)]
        alt_mm: bool,
        /// Fixed vertical offset in metres (up-positive).
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

pub struct LayoutApply {
    pub workspace: Workspace,
    pub view: Option<ViewX>,
    /// Restored fit-to-view toggle (`ViewMode::Full`), so the view re-fits the
    /// data range as it would after pressing the timeline fit button (LAY-09).
    pub fit_all: bool,
    pub speed: f64,
    pub follow_live: bool,
    pub vehicles: Vec<VehicleConfig>,
    pub diagnostics: Vec<Diag>,
}

#[derive(Clone, Debug)]
pub struct PendingLayout {
    pub name: String,
    doc: LayoutDoc,
    ambiguities: Vec<AmbiguousField>,
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

pub enum LoadOutcome {
    Applied(LayoutApply),
    NeedsMapping(PendingLayout),
}

#[derive(Clone, Debug)]
pub enum LayoutError {
    Io(String),
    Json(String),
    UnsupportedVersion(u32),
    NoStorageDir,
    MissingVersion,
}

pub struct CurrentLayout<'a> {
    pub name: String,
    pub workspace: &'a Workspace,
    pub snapshot: &'a StoreSnapshot,
    pub view: Option<ViewX>,
    /// Whether the fit-to-view toggle is engaged — persisted as
    /// `ViewMode::Full` (LAY-09).
    pub fit_all: bool,
    pub speed: f64,
    pub follow_live: bool,
    pub vehicles: &'a [VehicleConfig],
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

impl PendingLayout {
    pub fn ambiguities_mut(&mut self) -> &mut [AmbiguousField] {
        &mut self.ambiguities
    }

    pub fn ambiguity_count(&self) -> usize {
        self.ambiguities.len()
    }

    pub fn apply(self, snapshot: &StoreSnapshot) -> LayoutApply {
        let choices = self
            .ambiguities
            .iter()
            .filter_map(|a| {
                a.candidates
                    .get(a.selected)
                    .map(|c| (a.field.clone(), c.source))
            })
            .collect();
        apply_doc(self.doc, snapshot, &choices, false).expect("choices resolve ambiguities")
    }

    pub fn apply_skipping(self, snapshot: &StoreSnapshot) -> LayoutApply {
        apply_doc(self.doc, snapshot, &HashMap::new(), false).expect("skip mode cannot block")
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

/// The app config directory (where settings.json lives), if resolvable.
/// Used by the scripts panel (SCR-07) to locate its library dir.
#[cfg_attr(not(feature = "scripting"), allow(dead_code))]
pub fn config_dir() -> Option<std::path::PathBuf> {
    storage_dir(APP_ID)
}

/// Path to the app-wide settings file (LAY-08). Separate from layouts and from
/// `session.json` so loading a layout never changes user preferences.
fn settings_path() -> Result<PathBuf, LayoutError> {
    let Some(base) = storage_dir(APP_ID) else {
        return Err(LayoutError::NoStorageDir);
    };
    Ok(base.join("settings.json"))
}

/// Load app settings, falling back to defaults if the file is absent or
/// unreadable (first run, or written by a newer version).
pub fn load_app_settings() -> AppSettings {
    match settings_path() {
        Ok(path) => load_app_settings_at(&path),
        Err(_) => AppSettings::default(),
    }
}

/// Persist app settings to `settings.json` atomically.
pub fn save_app_settings(settings: &AppSettings) -> Result<(), LayoutError> {
    save_app_settings_at(&settings_path()?, settings)
}

/// Read app settings from an explicit path, defaulting on any failure.
fn load_app_settings_at(path: &Path) -> AppSettings {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write app settings to an explicit path atomically.
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
        // Future versions will add pure `migrate_vN_to_vN_plus_1` steps here.
        other => Err(LayoutError::UnsupportedVersion(other)),
    }
}

pub fn load_doc(doc: LayoutDoc, snapshot: &StoreSnapshot) -> Result<LoadOutcome, LayoutError> {
    if doc.delog_layout != LAYOUT_VERSION {
        return Err(LayoutError::UnsupportedVersion(doc.delog_layout));
    }
    match apply_doc(doc.clone(), snapshot, &HashMap::new(), true) {
        Ok(applied) => Ok(LoadOutcome::Applied(applied)),
        Err(ambiguities) => Ok(LoadOutcome::NeedsMapping(PendingLayout {
            name: doc.name.clone(),
            doc,
            ambiguities,
        })),
    }
}

pub fn current_doc(input: CurrentLayout<'_>) -> LayoutDoc {
    LayoutDoc {
        delog_layout: LAYOUT_VERSION,
        name: input.name,
        view: input.view.map(|v| ViewLayout {
            mode: if input.fit_all {
                ViewMode::Full
            } else {
                ViewMode::Window
            },
            min_us: v.min_us,
            max_us: v.max_us,
        }),
        playback: PlaybackLayout {
            speed: input.speed,
            follow_live: input.follow_live,
        },
        workspace: workspace_doc(input.workspace, input.snapshot),
        vehicles: input
            .vehicles
            .iter()
            .filter_map(|v| vehicle_to_layout(v, input.snapshot))
            .collect(),
        favorites: Vec::new(),
        docks: BTreeMap::new(),
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

fn workspace_doc(workspace: &Workspace, snapshot: &StoreSnapshot) -> WorkspaceLayout {
    let root = workspace
        .tree
        .root()
        .and_then(|id| node_to_layout(workspace, snapshot, id))
        .unwrap_or(LayoutNode::Plot {
            traces: Vec::new(),
            show_legend: true,
            show_tooltip: true,
        });
    WorkspaceLayout { root }
}

fn node_to_layout(
    workspace: &Workspace,
    snapshot: &StoreSnapshot,
    tile: egui_tiles::TileId,
) -> Option<LayoutNode> {
    match workspace.tree.tiles.get(tile)? {
        egui_tiles::Tile::Pane(Pane::Plot(pane)) => Some(LayoutNode::Plot {
            traces: pane
                .traces
                .iter()
                .filter_map(|t| trace_to_layout(t, snapshot))
                .chain(pane.ghosts.iter().map(ghost_to_layout))
                .collect(),
            show_legend: pane.show_legend,
            show_tooltip: pane.show_tooltip,
        }),
        egui_tiles::Tile::Pane(Pane::Scene3D(scene)) => Some(LayoutNode::Scene3d(SceneLayout {
            camera: CameraLayout {
                yaw: scene.camera.yaw,
                pitch: scene.camera.pitch,
                distance: scene.camera.distance,
            },
            tracked_vehicle: scene.tracked_vehicle,
        })),
        egui_tiles::Tile::Container(container) => {
            let children = container
                .children()
                .filter_map(|&child| node_to_layout(workspace, snapshot, child))
                .collect();
            Some(LayoutNode::Split {
                split: match container.kind() {
                    egui_tiles::ContainerKind::Tabs => SplitLayout::Tabs,
                    egui_tiles::ContainerKind::Horizontal => SplitLayout::Horizontal,
                    egui_tiles::ContainerKind::Vertical => SplitLayout::Vertical,
                    egui_tiles::ContainerKind::Grid => SplitLayout::Grid,
                },
                children,
            })
        }
    }
}

fn trace_to_layout(trace: &TraceRef, snapshot: &StoreSnapshot) -> Option<TraceLayout> {
    Some(TraceLayout {
        field: field_ref(snapshot, trace.field)?,
        color: trace.color,
        width_px: trace.width_px,
        mode: trace.mode.into(),
        visible: trace.visible,
    })
}

fn ghost_to_layout(ghost: &GhostTrace) -> TraceLayout {
    TraceLayout {
        field: FieldRef {
            topic: ghost.topic.clone(),
            field: ghost.field.clone(),
        },
        color: ghost.color,
        width_px: ghost.width_px,
        mode: ghost.mode.into(),
        visible: ghost.visible,
    }
}

fn vehicle_to_layout(v: &VehicleConfig, snapshot: &StoreSnapshot) -> Option<VehicleLayout> {
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

fn field_ref(snapshot: &StoreSnapshot, field: FieldId) -> Option<FieldRef> {
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

fn apply_doc(
    doc: LayoutDoc,
    snapshot: &StoreSnapshot,
    choices: &HashMap<FieldRef, SourceId>,
    collect_ambiguities: bool,
) -> Result<LayoutApply, Vec<AmbiguousField>> {
    let mut resolver = Resolver {
        snapshot,
        choices,
        diagnostics: Vec::new(),
        ambiguities: BTreeMap::new(),
        collect_ambiguities,
    };
    if collect_ambiguities {
        collect_field_refs(&doc, &mut resolver);
        if !resolver.ambiguities.is_empty() {
            return Err(resolver.ambiguities.into_values().collect());
        }
    }
    let workspace = workspace_from_layout(&doc.workspace, &mut resolver);
    let vehicles = doc
        .vehicles
        .iter()
        .filter_map(|v| vehicle_from_layout(v, &mut resolver))
        .collect::<Vec<_>>();

    Ok(LayoutApply {
        workspace,
        view: doc.view.map(|v| ViewX::new(v.min_us, v.max_us)),
        fit_all: doc.view.is_some_and(|v| matches!(v.mode, ViewMode::Full)),
        speed: doc.playback.speed,
        follow_live: doc.playback.follow_live,
        vehicles,
        diagnostics: resolver.diagnostics,
    })
}

fn collect_field_refs(doc: &LayoutDoc, resolver: &mut Resolver<'_>) {
    collect_node_field_refs(&doc.workspace.root, resolver);
    for vehicle in &doc.vehicles {
        collect_pos_field_refs(&vehicle.position, resolver);
        collect_ori_field_refs(&vehicle.orientation, resolver);
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

struct Resolver<'a> {
    snapshot: &'a StoreSnapshot,
    choices: &'a HashMap<FieldRef, SourceId>,
    diagnostics: Vec<Diag>,
    ambiguities: BTreeMap<FieldRef, AmbiguousField>,
    collect_ambiguities: bool,
}

impl Resolver<'_> {
    fn resolve(&mut self, key: &FieldRef) -> Option<FieldId> {
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

    fn resolve_in_source(&self, source: SourceId, key: &FieldRef) -> Option<FieldId> {
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

fn workspace_from_layout(doc: &WorkspaceLayout, resolver: &mut Resolver<'_>) -> Workspace {
    let mut tiles = egui_tiles::Tiles::default();
    let root = insert_node(&mut tiles, &doc.root, resolver)
        .unwrap_or_else(|| tiles.insert_pane(Pane::Plot(PlotPane::default())));
    Workspace {
        tree: egui_tiles::Tree::new("plot_workspace", root, tiles),
        focused: Some(root),
        shared_y_gutter: 0.0,
        default_show_legend: true,
    }
}

fn insert_node(
    tiles: &mut egui_tiles::Tiles<Pane>,
    node: &LayoutNode,
    resolver: &mut Resolver<'_>,
) -> Option<egui_tiles::TileId> {
    match node {
        LayoutNode::Plot {
            traces,
            show_legend,
            show_tooltip,
        } => {
            let mut pane = PlotPane {
                show_legend: *show_legend,
                show_tooltip: *show_tooltip,
                ..PlotPane::default()
            };
            for trace in traces {
                match trace_from_layout(trace, resolver) {
                    Some(resolved) => pane.traces.push(resolved),
                    None => pane.add_ghost(ghost_from_layout(trace)),
                }
            }
            Some(tiles.insert_pane(Pane::Plot(pane)))
        }
        LayoutNode::Scene3d(scene) => Some(tiles.insert_pane(Pane::Scene3D(Scene3dPane {
            camera: OrbitCamera {
                target: glam::Vec3::ZERO,
                yaw: scene.camera.yaw,
                pitch: scene.camera.pitch,
                distance: scene.camera.distance,
            },
            tracked_vehicle: scene.tracked_vehicle,
        }))),
        LayoutNode::Split { split, children } => {
            let child_ids = children
                .iter()
                .filter_map(|child| insert_node(tiles, child, resolver))
                .collect::<Vec<_>>();
            if child_ids.is_empty() {
                None
            } else if child_ids.len() == 1 {
                child_ids.first().copied()
            } else {
                Some(tiles.insert_container(egui_tiles::Container::new(
                    match split {
                        SplitLayout::Tabs => egui_tiles::ContainerKind::Tabs,
                        SplitLayout::Horizontal => egui_tiles::ContainerKind::Horizontal,
                        SplitLayout::Vertical => egui_tiles::ContainerKind::Vertical,
                        SplitLayout::Grid => egui_tiles::ContainerKind::Grid,
                    },
                    child_ids,
                )))
            }
        }
    }
}

fn trace_from_layout(trace: &TraceLayout, resolver: &mut Resolver<'_>) -> Option<TraceRef> {
    Some(TraceRef {
        field: resolver.resolve(&trace.field)?,
        color: trace.color,
        width_px: trace.width_px,
        mode: trace.mode.into(),
        visible: trace.visible,
    })
}

fn ghost_from_layout(trace: &TraceLayout) -> GhostTrace {
    GhostTrace {
        topic: trace.field.topic.clone(),
        field: trace.field.field.clone(),
        color: trace.color,
        width_px: trace.width_px,
        mode: trace.mode.into(),
        visible: trace.visible,
    }
}

fn vehicle_from_layout(v: &VehicleLayout, resolver: &mut Resolver<'_>) -> Option<VehicleConfig> {
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

impl From<TraceMode> for TraceModeLayout {
    fn from(value: TraceMode) -> Self {
        match value {
            TraceMode::Line => Self::Line,
            TraceMode::Scatter => Self::Scatter,
            TraceMode::Step => Self::Step,
        }
    }
}

impl From<TraceModeLayout> for TraceMode {
    fn from(value: TraceModeLayout) -> Self {
        match value {
            TraceModeLayout::Line => Self::Line,
            TraceModeLayout::Scatter => Self::Scatter,
            TraceModeLayout::Step => Self::Step,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delog_core::identity::IdentityRegistry;

    #[test]
    fn app_settings_round_trip_through_settings_json() {
        let path = std::env::temp_dir().join(format!(
            "delog-settings-rt-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("settings")
        ));
        let mut settings = AppSettings::default();
        settings.show_fps = true;
        settings.render_mode = crate::settings::RenderMode::Continuous;
        settings.theme = crate::theme::ThemeChoice::Light;

        save_app_settings_at(&path, &settings).expect("save settings");
        let loaded = load_app_settings_at(&path);
        let _ = fs::remove_file(&path);

        assert_eq!(loaded, settings);
    }

    #[test]
    fn load_app_settings_defaults_when_file_missing() {
        let missing = std::env::temp_dir().join(format!(
            "delog-settings-missing-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("settings")
        ));
        let _ = fs::remove_file(&missing); // ensure absent
        assert_eq!(load_app_settings_at(&missing), AppSettings::default());
    }

    #[test]
    fn sanitize_layout_name_blocks_paths() {
        assert_eq!(sanitize_name("../bad/name"), "bad_name");
        assert_eq!(sanitize_name(""), "default");
        assert_eq!(sanitize_name("ap-attitude_1"), "ap-attitude_1");
    }

    #[test]
    fn plot_field_ref_has_no_source_in_json() {
        let trace = TraceLayout {
            field: FieldRef {
                topic: "ATT".into(),
                field: "Roll".into(),
            },
            color: [1.0, 0.0, 0.0, 1.0],
            width_px: 1.5,
            mode: TraceModeLayout::Line,
            visible: true,
        };
        let json = serde_json::to_string(&trace).unwrap();
        assert!(json.contains("\"topic\":\"ATT\""));
        assert!(!json.contains("source"));
    }

    #[test]
    fn export_import_doc_round_trips_through_json_file() {
        let path = std::env::temp_dir().join(format!(
            "delog-layout-test-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("layout")
        ));
        let doc = empty_doc("portable");

        export_doc(&path, &doc).expect("export should write JSON");
        let imported = import_doc(&path).expect("import should read JSON");
        let _ = fs::remove_file(&path);

        assert_eq!(imported.delog_layout, LAYOUT_VERSION);
        assert_eq!(imported.name, "portable");
        let json = serde_json::to_string(&imported).unwrap();
        assert!(!json.contains("\"source\""));
    }

    #[test]
    fn legacy_layout_with_settings_key_still_decodes_ignoring_it() {
        // Layouts written before LAY-08 embedded a `settings` object. Decoding
        // must succeed and simply ignore the unknown key (serde default).
        let doc = decode_doc(
            r#"{
                "delog_layout": 1,
                "name": "legacy",
                "playback": {"speed": 1.0, "follow_live": false},
                "workspace": {
                    "root": {
                        "plot": {"traces": [], "show_legend": true, "show_tooltip": true}
                    }
                },
                "vehicles": [],
                "settings": {"theme": "light", "show_fps": true, "render_mode": "continuous"}
            }"#,
        )
        .expect("legacy layout with settings key should decode");
        assert_eq!(doc.name, "legacy");
    }

    #[test]
    fn invalid_version_is_rejected() {
        let mut doc = empty_doc("bad");
        doc.delog_layout = 99;
        match load_doc(doc, &StoreSnapshot::empty()) {
            Err(LayoutError::UnsupportedVersion(99)) => {}
            Ok(_) => panic!("expected unsupported version, got successful load"),
            Err(err) => panic!("expected unsupported version, got {err}"),
        }
    }

    #[test]
    fn missing_version_is_rejected_by_decoder() {
        match decode_doc(r#"{"name":"missing"}"#) {
            Err(LayoutError::MissingVersion) => {}
            Ok(_) => panic!("expected missing version, got successful decode"),
            Err(err) => panic!("expected missing version, got {err}"),
        }
    }

    #[test]
    fn frozen_v1_fixture_decodes_and_applies_cross_log() {
        let doc = decode_doc(include_str!("../../../fixtures/layouts/v1_basic.json"))
            .expect("fixture should decode");
        assert_eq!(doc.delog_layout, LAYOUT_VERSION);
        assert_eq!(doc.name, "v1-basic");

        let snapshot = snapshot_with_topics(&[
            ("different_log", "ATT", &["Roll", "Pitch", "Yaw"]),
            ("different_log", "POS", &["Lat", "Lng", "Alt"]),
        ]);
        let outcome = load_doc(doc, &snapshot).expect("fixture should load");
        let LoadOutcome::Applied(layout) = outcome else {
            panic!("single-source fixture should not need mapping");
        };

        assert_eq!(layout.vehicles.len(), 1);
        assert_eq!(layout.vehicles[0].label, "Vehicle");
        assert_eq!(layout.diagnostics.len(), 0);
    }

    #[test]
    fn same_layout_populates_after_loading_before_log_schema() {
        let doc = decode_doc(include_str!("../../../fixtures/layouts/v1_basic.json"))
            .expect("fixture should decode");
        let LoadOutcome::Applied(empty_layout) =
            load_doc(doc.clone(), &StoreSnapshot::empty()).expect("empty load should apply")
        else {
            panic!("empty store should not need mapping");
        };
        assert_eq!(empty_layout.vehicles.len(), 0);
        let (traces, ghosts) = plot_trace_counts(&empty_layout.workspace);
        assert_eq!(traces, 0);
        assert_eq!(ghosts, 2);

        let snapshot = snapshot_with_topics(&[
            ("later_log", "ATT", &["Roll", "Pitch", "Yaw"]),
            ("later_log", "POS", &["Lat", "Lng", "Alt"]),
        ]);
        let LoadOutcome::Applied(populated) =
            load_doc(doc, &snapshot).expect("schema load should apply")
        else {
            panic!("single source should not need mapping");
        };
        assert_eq!(populated.vehicles.len(), 1);
        let (traces, ghosts) = plot_trace_counts(&populated.workspace);
        assert_eq!(traces, 2);
        assert_eq!(ghosts, 0);
    }

    #[test]
    fn one_loaded_source_resolves_topic_field_without_source() {
        let (snapshot, field) = snapshot_with_sources(&[("flight_a", "ATT", "Roll")]);
        let mut resolver = Resolver {
            snapshot: &snapshot,
            choices: &HashMap::new(),
            diagnostics: Vec::new(),
            ambiguities: BTreeMap::new(),
            collect_ambiguities: true,
        };

        let got = resolver.resolve(&FieldRef {
            topic: "ATT".into(),
            field: "Roll".into(),
        });

        assert_eq!(got, Some(field[0]));
        assert!(resolver.ambiguities.is_empty());
    }

    #[test]
    fn duplicate_topic_field_across_sources_is_ambiguous() {
        let (snapshot, _) =
            snapshot_with_sources(&[("flight_a", "ATT", "Roll"), ("flight_b", "ATT", "Roll")]);
        let mut resolver = Resolver {
            snapshot: &snapshot,
            choices: &HashMap::new(),
            diagnostics: Vec::new(),
            ambiguities: BTreeMap::new(),
            collect_ambiguities: true,
        };

        let got = resolver.resolve(&FieldRef {
            topic: "ATT".into(),
            field: "Roll".into(),
        });

        assert_eq!(got, None);
        let ambiguity = resolver.ambiguities.values().next().unwrap();
        assert_eq!(ambiguity.candidates.len(), 2);
        assert_eq!(ambiguity.candidates[0].label, "flight_a");
        assert_eq!(ambiguity.candidates[1].label, "flight_b");
    }

    fn snapshot_with_sources(entries: &[(&str, &str, &str)]) -> (StoreSnapshot, Vec<FieldId>) {
        let mut ids = IdentityRegistry::new();
        let mut fields = Vec::new();
        for (source, topic, field) in entries {
            let source = ids.add_source(*source);
            let topic = ids.add_topic(source, *topic).unwrap();
            fields.push(ids.add_field(topic, *field).unwrap());
        }
        (
            StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot"),
            fields,
        )
    }

    fn snapshot_with_topics(entries: &[(&str, &str, &[&str])]) -> StoreSnapshot {
        let mut ids = IdentityRegistry::new();
        let mut sources = HashMap::new();
        for (source, topic, fields) in entries {
            let source_id = *sources
                .entry(*source)
                .or_insert_with(|| ids.add_source(*source));
            let topic = ids.add_topic(source_id, *topic).unwrap();
            for field in *fields {
                ids.add_field(topic, *field).unwrap();
            }
        }
        StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot")
    }

    #[test]
    fn fit_to_view_persists_as_full_mode_and_restores_fit_all() {
        // ViewMode::Full in the saved view restores the fit-all toggle (LAY-09).
        let mut doc = empty_doc("fit");
        doc.view = Some(ViewLayout {
            mode: ViewMode::Full,
            min_us: 0,
            max_us: 10,
        });
        let LoadOutcome::Applied(layout) =
            load_doc(doc, &StoreSnapshot::empty()).expect("should apply")
        else {
            panic!("no sources → no mapping");
        };
        assert!(layout.fit_all);

        // A normal windowed view does not engage fit-all.
        let mut doc = empty_doc("win");
        doc.view = Some(ViewLayout {
            mode: ViewMode::Window,
            min_us: 0,
            max_us: 10,
        });
        let LoadOutcome::Applied(layout) =
            load_doc(doc, &StoreSnapshot::empty()).expect("should apply")
        else {
            panic!("no sources → no mapping");
        };
        assert!(!layout.fit_all);
    }

    fn empty_doc(name: &str) -> LayoutDoc {
        LayoutDoc {
            delog_layout: LAYOUT_VERSION,
            name: name.into(),
            view: None,
            playback: PlaybackLayout {
                speed: 1.0,
                follow_live: false,
            },
            workspace: WorkspaceLayout {
                root: LayoutNode::Plot {
                    traces: Vec::new(),
                    show_legend: true,
                    show_tooltip: true,
                },
            },
            vehicles: Vec::new(),
            favorites: Vec::new(),
            docks: BTreeMap::new(),
        }
    }

    fn plot_trace_counts(workspace: &Workspace) -> (usize, usize) {
        workspace
            .tree
            .tiles
            .tiles()
            .filter_map(|tile| match tile {
                egui_tiles::Tile::Pane(Pane::Plot(pane)) => {
                    Some((pane.traces.len(), pane.ghosts.len()))
                }
                _ => None,
            })
            .fold((0, 0), |(traces, ghosts), (t, g)| (traces + t, ghosts + g))
    }
}
