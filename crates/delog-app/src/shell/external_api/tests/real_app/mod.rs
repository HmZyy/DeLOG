use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::ipc::writer::StreamWriter;
use arrow::record_batch::RecordBatch;
use delog_api::control::{
    AnnotationInfo, AnnotationRequest, ControlCall, ControlRequest, ControlResponse, PlotInfo,
    PlotRequest, ResolvedVehicleField, SplitDirection, TraceInfo, TraceMode, TraceRequest,
    VehicleInfo, VehicleModel, VehicleOrientation, VehiclePosition, VehicleRequest, VehicleSpec,
    WindowInfo, WorkspaceRequest,
};
use delog_cache::CacheManager;
use delog_core::identity::{FieldId, SourceId};
use delog_core::snapshot::StoreSnapshot;
use delog_remote::DiscoveryDescriptor;
use serde_json::{Value, json};

use super::*;
use crate::plotting::markers::Markers;
use crate::plotting::timeline::Playback;
use crate::scene3d::vehicle::VehicleConfig;
use crate::session::session::Session;
use crate::shell::app::{AppControl, AppControlHost, ControlQueue, apply_call};
use crate::shell::windows::ExtendedWindow;
use crate::shell::workspace::Workspace;

const OWNER: &str = "external/flight-diagnosis";

struct OwnedAppControlFixture {
    markers: Markers,
    workspace: Workspace,
    windows: Vec<ExtendedWindow>,
    playback: Playback,
    next_window_id: u64,
    caches: CacheManager,
    vehicles: Vec<VehicleConfig>,
    next_vehicle_id: u64,
    vehicle_revision: u64,
    traj_dirty: bool,
}

impl OwnedAppControlFixture {
    fn new() -> Self {
        Self {
            markers: Markers::new(),
            workspace: Workspace::new(),
            windows: Vec::new(),
            playback: Playback::default(),
            next_window_id: 1,
            caches: CacheManager::new(),
            vehicles: Vec::new(),
            next_vehicle_id: 1,
            vehicle_revision: 0,
            traj_dirty: false,
        }
    }

    fn borrow<'a>(&'a mut self, snapshot: &'a StoreSnapshot) -> AppControl<'a> {
        AppControl {
            markers: &mut self.markers,
            workspace: &mut self.workspace,
            windows: &mut self.windows,
            playback: &mut self.playback,
            next_window_id: &mut self.next_window_id,
            caches: &mut self.caches,
            snapshot,
            vehicles: &mut self.vehicles,
            next_vehicle_id: &mut self.next_vehicle_id,
            vehicle_revision: &mut self.vehicle_revision,
            traj_dirty: &mut self.traj_dirty,
            vehicle_profiles: None,
        }
    }
}

struct HeadlessExternalApp {
    controller: ExternalApiController,
    queue: ControlQueue,
    control: OwnedAppControlFixture,
    session: Session,
    discovery: tempfile::TempDir,
    applied: usize,
}

impl HeadlessExternalApp {
    fn new() -> Self {
        Self::with_control_timeout(Duration::from_secs(5))
    }

    fn with_control_timeout(timeout: Duration) -> Self {
        let ctx = egui::Context::default();
        let (host, queue) = AppControlHost::new(ctx.clone(), 128);
        let session = Session::new(ctx.clone());
        let discovery = tempfile::tempdir().unwrap();
        let mut app = Self {
            controller: ExternalApiController::new(ctx, host),
            queue,
            control: OwnedAppControlFixture::new(),
            session,
            discovery,
            applied: 0,
        };
        app.enable(timeout);
        app
    }

    fn enable(&mut self, timeout: Duration) {
        let mut config = build_config(
            &self.session.snapshot(),
            ExternalApiLimits::default(),
            Some(self.discovery.path().to_owned()),
        );
        config.control.timeout = timeout;
        self.controller
            .enable(config, self.session.store(), self.session.ingest_sender())
            .unwrap();
    }

    fn drain_once(&mut self) {
        let snapshot = self.session.snapshot();
        let mut control = self.control.borrow(&snapshot);
        let mut applied = 0;
        self.queue.drain_with(|call| {
            applied += 1;
            apply_call(&mut control, call)
        });
        self.applied += applied;
    }

    fn drain_slowly(&mut self, delay: Duration) {
        let snapshot = self.session.snapshot();
        let mut control = self.control.borrow(&snapshot);
        let mut applied = 0;
        self.queue.drain_with(|call| {
            thread::sleep(delay);
            applied += 1;
            apply_call(&mut control, call)
        });
        self.applied += applied;
    }

    fn serve<T: Send + 'static>(&mut self, work: impl FnOnce(Api) -> T + Send + 'static) -> T {
        let api = self.api();
        let worker = thread::spawn(move || work(api));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !worker.is_finished() {
            assert!(Instant::now() < deadline, "HTTP worker did not finish");
            self.drain_once();
            thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap()
    }

    fn api(&self) -> Api {
        let endpoint = match self.controller.status() {
            ExternalApiStatus::Running(running) => running.endpoint,
            ExternalApiStatus::Disabled => panic!("external API is disabled"),
        };
        let descriptor = std::fs::read_dir(self.discovery.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "json"))
            .expect("discovery descriptor");
        let descriptor = DiscoveryDescriptor::parse(&std::fs::read(descriptor).unwrap()).unwrap();
        Api {
            http: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap(),
            base: format!("http://{endpoint}"),
            bootstrap: descriptor.bootstrap_token().expose(),
        }
    }

    fn trusted(&mut self, request: ControlRequest) -> ControlResponse {
        let snapshot = self.session.snapshot();
        let mut control = self.control.borrow(&snapshot);
        apply_call(&mut control, ControlCall::Trusted(request)).unwrap()
    }

    fn windows(&mut self) -> Vec<WindowInfo> {
        self.trusted(ControlRequest::Workspace(WorkspaceRequest::ListWindows))
            .into_windows()
            .unwrap()
    }

    fn plots(&mut self) -> Vec<PlotInfo> {
        self.trusted(ControlRequest::Plots(PlotRequest::List { window: None }))
            .into_plots()
            .unwrap()
    }

    fn traces(&mut self) -> Vec<TraceInfo> {
        let plots = self.plots();
        plots
            .iter()
            .flat_map(|plot| {
                self.trusted(ControlRequest::Traces(TraceRequest::List {
                    window: plot.window,
                    tile: plot.tile,
                }))
                .into_traces()
                .unwrap()
            })
            .collect()
    }

    fn annotations(&mut self) -> Vec<AnnotationInfo> {
        self.trusted(ControlRequest::Annotations(AnnotationRequest::List {
            target: None,
        }))
        .into_annotations()
        .unwrap()
    }

    fn vehicles(&mut self) -> Vec<VehicleInfo> {
        self.trusted(ControlRequest::Vehicles(Box::new(VehicleRequest::List)))
            .into_vehicles()
            .unwrap()
    }

    fn marker_labels(&self) -> Vec<String> {
        self.control
            .markers
            .as_slice()
            .iter()
            .map(|marker| marker.label.clone())
            .collect()
    }

    fn published_field(&self, topic: &str, name: &str) -> PublishedField {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = self.session.snapshot();
            if let Some(found) = find_published_field(&snapshot, topic, name) {
                return found;
            }
            assert!(Instant::now() < deadline, "{topic}/{name} never published");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn seed_manual(&mut self) -> ManualSeed {
        let ControlResponse::Window(window) =
            self.trusted(ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                title: Some("manual window".into()),
                owner: None,
            }))
        else {
            panic!("open window answered with the wrong kind of result")
        };
        let window = window.id;
        let plot = self
            .trusted(ControlRequest::Workspace(WorkspaceRequest::AddPlot {
                window: None,
                direction: SplitDirection::Horizontal,
                owner: None,
            }))
            .into_plots()
            .unwrap()[0]
            .clone();
        let error = self.published_field("diagnosis", "error");
        self.trusted(ControlRequest::Traces(TraceRequest::AddReturning {
            window: plot.window,
            tile: plot.tile,
            field_id: error.id,
            field: error.trace_path.clone(),
            color: None,
            width_px: None,
            mode: TraceMode::Line,
            owner: None,
        }));
        self.control.markers.add_at(1_000);
        let vehicle = self
            .trusted(ControlRequest::Vehicles(Box::new(VehicleRequest::Add(
                manual_vehicle(self, &error),
            ))))
            .into_vehicles()
            .unwrap()[0]
            .id;
        ManualSeed {
            window,
            plot_label: plot.label,
            vehicle,
        }
    }
}

struct ManualSeed {
    window: u64,
    plot_label: String,
    vehicle: u64,
}

fn manual_vehicle(app: &HeadlessExternalApp, source: &PublishedField) -> VehicleSpec {
    let field = |name: &str| {
        let field = app.published_field("diagnosis", name);
        ResolvedVehicleField {
            id: field.id,
            path: field.vehicle_path,
        }
    };
    VehicleSpec {
        source_id: source.source,
        source: source.source_label.clone(),
        label: "manual vehicle".into(),
        show: true,
        show_path: true,
        position: VehiclePosition::Gps {
            lat: field("lat"),
            lon: field("lon"),
            alt: field("alt"),
            lat_lon_dege7: false,
            alt_mm: false,
            alt_offset_m: 0.0,
        },
        orientation: VehicleOrientation::Static,
        model: VehicleModel::Quad,
        color: [1.0, 0.0, 0.0, 1.0],
        path_color: [0.0, 1.0, 0.0, 1.0],
        scale: 1.0,
        owner: None,
    }
}

struct PublishedField {
    id: FieldId,
    trace_path: String,
    vehicle_path: String,
    source: SourceId,
    source_label: String,
}

fn find_published_field(
    snapshot: &StoreSnapshot,
    topic: &str,
    name: &str,
) -> Option<PublishedField> {
    snapshot.fields.iter().find_map(|field| {
        if field.removed || field.name != name {
            return None;
        }
        let entry = snapshot.topic(field.topic)?;
        let source = snapshot.source(entry.entry.source)?;
        let provenance = source.entry.derived_provenance.as_ref()?;
        (!entry.entry.removed && !source.entry.removed && provenance.logical_topic == topic).then(
            || PublishedField {
                id: field.id,
                trace_path: format!("{}.{}", entry.entry.name, field.name),
                vehicle_path: format!("{}/{}/{}", source.entry.label, entry.entry.name, field.name),
                source: source.entry.id,
                source_label: source.entry.label.clone(),
            },
        )
    })
}

static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

fn next_key() -> String {
    format!("real-app-{}", NEXT_KEY.fetch_add(1, Ordering::Relaxed))
}

fn diagnosis_arrow() -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("__delog_time_ns", DataType::Int64, false),
        Field::new("error", DataType::Float64, false),
        Field::new("lat", DataType::Float64, false),
        Field::new("lon", DataType::Float64, false),
        Field::new("alt", DataType::Float64, false),
    ]));
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(vec![1_000_000, 2_000_000, 3_000_000])),
        Arc::new(Float64Array::from(vec![0.5, 1.5, 0.25])),
        Arc::new(Float64Array::from(vec![47.0, 47.0001, 47.0002])),
        Arc::new(Float64Array::from(vec![8.0, 8.0001, 8.0002])),
        Arc::new(Float64Array::from(vec![400.0, 401.0, 402.0])),
    ];
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).unwrap();
    let mut bytes = Vec::new();
    {
        let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
        writer.write(&batch).unwrap();
        writer.finish().unwrap();
    }
    bytes
}

#[derive(Clone)]
struct Api {
    http: reqwest::blocking::Client,
    base: String,
    bootstrap: String,
}

#[derive(Clone)]
struct Client {
    token: String,
    client_id: String,
}

impl Api {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn connect(&self, takeover: bool) -> Client {
        self.connect_as("flight-diagnosis", takeover)
    }

    fn connect_as(&self, name: &str, takeover: bool) -> Client {
        let (status, value) = decode(
            self.http
                .post(self.url("/v1/clients"))
                .bearer_auth(&self.bootstrap)
                .body(json!({"name": name, "takeover": takeover}).to_string())
                .send(),
        );
        assert_eq!(status, 200, "{value}");
        Client {
            token: value["token"].as_str().unwrap().into(),
            client_id: value["client_id"].as_str().unwrap().into(),
        }
    }

    fn publish(&self, client: &Client, topic: &str) -> Value {
        let (status, value) = decode(
            self.http
                .put(self.url(&format!("/v1/publications/{topic}")))
                .bearer_auth(&client.token)
                .header("content-type", "application/vnd.apache.arrow.stream")
                .header("idempotency-key", next_key())
                .body(diagnosis_arrow())
                .send(),
        );
        assert_eq!(status, 201, "{value}");
        value
    }

    fn command_with_key(&self, client: &Client, key: &str, body: Value) -> (u16, Value) {
        decode(
            self.http
                .post(self.url("/v1/control"))
                .bearer_auth(&client.token)
                .header("content-type", "application/json")
                .header("idempotency-key", key)
                .body(body.to_string())
                .send(),
        )
    }

    fn command(&self, client: &Client, body: Value) -> (u16, Value) {
        self.command_with_key(client, &next_key(), body)
    }

    fn resource(&self, client: &Client, body: Value) -> String {
        let (status, value) = self.command(client, body);
        assert_eq!(status, 200, "{value}");
        assert_eq!(value["kind"], "resource", "{value}");
        value["handle"].as_str().unwrap().into()
    }

    fn state(&self, client: &Client) -> Value {
        let (status, value) = decode(
            self.http
                .get(self.url("/v1/control/state"))
                .bearer_auth(&client.token)
                .send(),
        );
        assert_eq!(status, 200, "{value}");
        value
    }
}

fn decode(response: reqwest::Result<reqwest::blocking::Response>) -> (u16, Value) {
    let response = response.unwrap();
    let status = response.status().as_u16();
    let text = response.text().unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

fn manual_handles(state: &Value, kind: &str) -> Vec<String> {
    state[kind]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["owner"].is_null())
        .map(|item| item["handle"].as_str().unwrap().to_owned())
        .collect()
}

mod acceptance;
mod creation;
mod policy;
mod races;
