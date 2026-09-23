use std::sync::Arc;

use delog_api::markers::PendingMarker;
use delog_core::identity::{FieldId, SourceId};
use delog_core::snapshot::StoreSnapshot;

pub mod annotations;
pub mod layouts;
pub mod markers;
pub mod plots;
mod runtime;
pub mod testing;
pub mod traces;
pub mod vehicles;
pub mod workspace;

pub use runtime::{
    BatchPy, DeferredControlBuffer, HostGuard, RecordingHost, call_immediate_detached,
    control_call_error, current_host, install_host, request_is_batchable, stage_batch_request,
};

#[derive(Debug, Clone, PartialEq)]
pub struct PlotInfo {
    pub window: u64,
    pub tile: u64,
    pub index: usize,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceMode {
    Line,
    Scatter,
    Step,
}

impl TraceMode {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "line" => Some(Self::Line),
            "scatter" => Some(Self::Scatter),
            "step" => Some(Self::Step),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceInfo {
    pub index: usize,
    pub field_id: FieldId,
    pub field: String,
    pub color: [f32; 4],
    pub width_px: f32,
    pub mode: TraceMode,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TraceRequest {
    List {
        window: u64,
        tile: u64,
    },
    Add {
        window: u64,
        tile: u64,
        field_id: FieldId,
        field: String,
        color: Option<[f32; 4]>,
        width_px: Option<f32>,
        mode: TraceMode,
        owner: Option<ScriptOwner>,
    },
    Remove {
        window: u64,
        tile: u64,
        index: Option<usize>,
        field_id: Option<FieldId>,
        field: Option<String>,
    },
    Clear {
        window: u64,
        tile: u64,
    },
    Set {
        window: u64,
        tile: u64,
        index: usize,
        field_id: FieldId,
        color: Option<[f32; 4]>,
        width_px: Option<f32>,
        mode: Option<TraceMode>,
        visible: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationKind {
    Text,
    Segment,
    Rect,
    Ellipse,
    HLine,
}

impl AnnotationKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "text" => Some(Self::Text),
            "segment" => Some(Self::Segment),
            "rect" => Some(Self::Rect),
            "ellipse" => Some(Self::Ellipse),
            "hline" => Some(Self::HLine),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationGeometry {
    Text { at: (i64, f64) },
    Segment { from: (i64, f64), to: (i64, f64) },
    Rect { a: (i64, f64), b: (i64, f64) },
    Ellipse { a: (i64, f64), b: (i64, f64) },
    HLine { y: f64 },
}

impl AnnotationGeometry {
    pub fn kind(&self) -> AnnotationKind {
        match self {
            Self::Text { .. } => AnnotationKind::Text,
            Self::Segment { .. } => AnnotationKind::Segment,
            Self::Rect { .. } => AnnotationKind::Rect,
            Self::Ellipse { .. } => AnnotationKind::Ellipse,
            Self::HLine { .. } => AnnotationKind::HLine,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AnnotationStylePatch {
    pub color: Option<[f32; 4]>,
    pub stroke_px: Option<f32>,
    pub fill_opacity: Option<f32>,
    pub font_px: Option<f32>,
    pub arrow: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationFilter {
    Index(usize),
    Id(u64),
    Kind(AnnotationKind),
    Label(String),
    Owner(String),
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationInfo {
    pub window: u64,
    pub tile: u64,
    pub id: u64,
    pub index: usize,
    pub kind: AnnotationKind,
    pub geometry: AnnotationGeometry,
    pub label: String,
    pub color: [f32; 4],
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationRequest {
    Add {
        window: u64,
        tile: u64,
        geometry: AnnotationGeometry,
        label: String,
        style: AnnotationStylePatch,
        owner: Option<ScriptOwner>,
    },
    List {
        target: Option<(u64, u64)>,
    },
    Remove {
        target: Option<(u64, u64)>,
        filter: AnnotationFilter,
    },
    Set {
        window: u64,
        tile: u64,
        id: u64,
        label: Option<String>,
        geometry: Option<AnnotationGeometry>,
        style: AnnotationStylePatch,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerOrigin {
    Manual,
    Script,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarkerInfo {
    pub id: u64,
    pub index: usize,
    pub t_us: i64,
    pub label: String,
    pub color: [f32; 4],
    pub note: String,
    pub origin: MarkerOrigin,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MarkerPatch {
    pub t_us: Option<i64>,
    pub label: Option<String>,
    pub color: Option<[f32; 4]>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkerFilter {
    Id(u64),
    Index(usize),
    Owner(String),
    Origin(MarkerOrigin),
    ScriptLabel(String),
    ScriptTimeRange {
        after: Option<i64>,
        before: Option<i64>,
    },
    ScriptAll,
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkerRequest {
    Append {
        owner: String,
        generation: u64,
        markers: Vec<PendingMarker>,
    },
    RemoveOwned {
        owner: String,
    },
    List,
    Set {
        id: u64,
        patch: MarkerPatch,
    },
    Remove(MarkerFilter),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlotRequest {
    List { window: Option<u64> },
    Focused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

impl SplitDirection {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "horizontal" => Some(Self::Horizontal),
            "vertical" => Some(Self::Vertical),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceRequest {
    AddPlot {
        direction: SplitDirection,
    },
    Split {
        window: u64,
        tile: u64,
        direction: SplitDirection,
    },
    Close {
        window: u64,
        tile: u64,
    },
    Equalize,
    ShowScene {
        visible: bool,
    },
    OpenWindow {
        title: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackRequest {
    Set {
        speed: Option<f64>,
        follow_live: Option<bool>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptOwner {
    pub name: String,
    pub generation: u64,
}

#[derive(Clone)]
pub struct PlotContext {
    pub owner: Option<ScriptOwner>,
    pub snapshot: Arc<StoreSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenerationRequest {
    Commit { owner: String, generation: u64 },
    Rollback { owner: String, generation: u64 },
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutFieldIssue {
    pub field: String,
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadReport {
    pub ambiguous: Vec<LayoutFieldIssue>,
    pub unresolved: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutRequest {
    List,
    Save { name: String },
    Load { name: String },
    Delete { name: String },
    Rename { from: String, to: String },
    Duplicate { from: String, to: String },
    ImportFile { path: String },
    ExportFile { name: String, path: String },
    Clear,
    Current,
    Apply { json: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlRequest {
    Markers(MarkerRequest),
    Plots(PlotRequest),
    Traces(TraceRequest),
    Annotations(AnnotationRequest),
    Generation(GenerationRequest),
    Workspace(WorkspaceRequest),
    Playback(PlaybackRequest),
    Vehicles(VehicleRequest),
    VehicleProfiles(VehicleProfileRequest),
    Layouts(LayoutRequest),
    Batch(Vec<ControlRequest>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlResponse {
    Unit,
    Plots(Vec<PlotInfo>),
    Traces(Vec<TraceInfo>),
    Annotations(Vec<AnnotationInfo>),
    Window(u64),
    Vehicles(Vec<VehicleInfo>),
    VehicleProfile(VehicleProfileInfo),
    Names(Vec<String>),
    Markers(Vec<MarkerInfo>),
    Layout(String),
    LoadReport(LoadReport),
}

pub trait ControlHost: Send + Sync {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse, String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn call(request: ControlRequest) -> Result<ControlResponse, String> {
        Python::attach(|py| call_immediate_detached(py, request))
    }

    #[test]
    fn immediate_calls_without_a_host_report_the_missing_context() {
        let error = call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
            owner: "flight.py".into(),
        }))
        .unwrap_err();
        assert!(error.contains("not available"), "{error}");
    }

    #[test]
    fn installing_a_host_routes_calls_to_it_and_uninstalls_on_drop() {
        let host = Arc::new(RecordingHost::default());
        {
            let _guard = install_host(Some(host.clone()));
            let response = call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: "flight.py".into(),
            }))
            .unwrap();
            assert_eq!(response, ControlResponse::Unit);
        }
        assert_eq!(
            host.taken(),
            vec![ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: "flight.py".into()
            })]
        );
        assert!(
            call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: "x".into()
            }))
            .is_err()
        );
    }

    #[test]
    fn recording_host_preserves_vehicle_requests() {
        let host = Arc::new(RecordingHost::default());
        let _guard = install_host(Some(host.clone()));
        let request = ControlRequest::Vehicles(VehicleRequest::List);
        Python::attach(|py| call_immediate_detached(py, request.clone())).unwrap();
        assert_eq!(host.taken(), vec![request]);
    }

    #[test]
    fn recording_host_preserves_p4_requests() {
        let host = Arc::new(RecordingHost::default());
        let _guard = install_host(Some(host.clone()));
        let marker = ControlRequest::Markers(MarkerRequest::List);
        let layout = ControlRequest::Layouts(LayoutRequest::Current);
        let batch = ControlRequest::Batch(vec![ControlRequest::Playback(PlaybackRequest::Set {
            speed: Some(2.0),
            follow_live: None,
        })]);
        let _report = ControlResponse::LoadReport(LoadReport::default());
        Python::attach(|py| {
            call_immediate_detached(py, marker.clone()).unwrap();
            call_immediate_detached(py, layout.clone()).unwrap();
            call_immediate_detached(py, batch.clone()).unwrap();
        });
        assert_eq!(host.taken(), vec![marker, layout, batch]);
    }

    #[test]
    fn a_host_error_surfaces_verbatim() {
        let host = Arc::new(RecordingHost::failing("pane closed"));
        let _guard = install_host(Some(host));
        let error = call(ControlRequest::Markers(MarkerRequest::RemoveOwned {
            owner: "x".into(),
        }))
        .unwrap_err();
        assert_eq!(error, "pane closed");
    }

    #[test]
    fn installing_none_uninstalls_the_current_host() {
        let host = Arc::new(RecordingHost::default());
        let _outer = install_host(Some(host));
        {
            let _inner = install_host(None);
            assert!(current_host().is_none());
        }
        assert!(current_host().is_some());
    }

    struct BlockingHost {
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl ControlHost for BlockingHost {
        fn call(&self, _request: ControlRequest) -> Result<ControlResponse, String> {
            let _ = self.entered.send(());
            let _ = self.release.lock().unwrap().recv();
            Ok(ControlResponse::Unit)
        }
    }

    #[test]
    fn a_waiting_call_leaves_the_interpreter_free_for_other_threads() {
        let (entered_tx, entered_rx) = sync_channel::<()>(1);
        let (release_tx, release_rx) = sync_channel::<()>(1);
        let host = Arc::new(BlockingHost {
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let waiter = std::thread::spawn(move || {
            let _guard = install_host(Some(host as Arc<dyn ControlHost>));
            call(ControlRequest::Plots(PlotRequest::List { window: None }))
        });
        entered_rx.recv().unwrap();

        let (attached_tx, attached_rx) = sync_channel::<()>(1);
        let prober = std::thread::spawn(move || {
            Python::attach(|_py| {});
            let _ = attached_tx.send(());
        });
        let attached = attached_rx.recv_timeout(Duration::from_secs(10)).is_ok();

        let _ = release_tx.send(());
        prober.join().unwrap();
        assert_eq!(waiter.join().unwrap(), Ok(ControlResponse::Unit));
        assert!(
            attached,
            "another thread could not attach to Python while the call waited"
        );
    }
}
