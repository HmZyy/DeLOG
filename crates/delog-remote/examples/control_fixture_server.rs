use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use delog_api::control::{
    AccessMode, AnnotationInfo, AnnotationRequest, AuthorizedControlHost, ControlPrincipal,
    ControlRequest, ControlResponse, GenerationRequest, LayoutRequest, MarkerInfo, MarkerOrigin,
    MarkerRequest, PlaybackInfo, PlaybackRequest, PlotInfo, PlotRequest, ResourceGuard, TraceInfo,
    TraceRequest, VehicleInfo, VehicleRequest, WindowInfo, WorkspaceInfo, WorkspaceRequest,
};
use delog_api::{Error, Result};
use delog_core::ingest::{
    IngestSender, IngestSink, ParseSummary, ParsedBatch, SourceKind, ingest_channel,
};
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::DataStore;
use delog_remote::{
    ControlLimits, OwnerRegistry, RemoteConfig, RemoteServer, RemoteServices, UploadConfig,
};
use serde::Serialize;
use serde_json::{Value, json};

const DEFAULT_COLOR: [f32; 4] = [0.2, 0.6, 0.9, 1.0];

#[derive(Debug, Serialize)]
struct RecordedCall {
    access: &'static str,
    owner: String,
    generation: u64,
    caller_owner: Option<String>,
    request: String,
    query: bool,
}

#[derive(Default)]
struct FixtureState {
    calls: Vec<RecordedCall>,
    rejected: Vec<String>,
    windows: Vec<WindowInfo>,
    plots: Vec<PlotInfo>,
    traces: Vec<FixtureTrace>,
    annotations: Vec<AnnotationInfo>,
    vehicles: Vec<VehicleInfo>,
    markers: Vec<MarkerInfo>,
    next_window: u64,
    next_tile: u64,
    next_instance: u64,
    next_annotation: u64,
    next_vehicle: u64,
    next_marker: u64,
}

#[derive(Clone)]
struct FixtureTrace {
    window: u64,
    tile: u64,
    info: TraceInfo,
}

impl FixtureState {
    fn new() -> Self {
        Self {
            next_window: 10,
            next_tile: 20,
            next_instance: 30,
            next_annotation: 40,
            next_vehicle: 50,
            next_marker: 60,
            ..Self::default()
        }
    }

    fn next_window(&mut self) -> u64 {
        self.next_window += 1;
        self.next_window
    }

    fn next_tile(&mut self) -> u64 {
        self.next_tile += 1;
        self.next_tile
    }

    fn next_instance(&mut self) -> u64 {
        self.next_instance += 1;
        self.next_instance
    }

    fn next_annotation(&mut self) -> u64 {
        self.next_annotation += 1;
        self.next_annotation
    }

    fn next_vehicle(&mut self) -> u64 {
        self.next_vehicle += 1;
        self.next_vehicle
    }

    fn next_marker(&mut self) -> u64 {
        self.next_marker += 1;
        self.next_marker
    }

    fn apply(
        &mut self,
        principal: &ControlPrincipal,
        request: ControlRequest,
    ) -> Result<ControlResponse> {
        match request {
            ControlRequest::Guarded { guard, request } => {
                self.validate_guard(guard)?;
                self.apply(principal, *request)
            }
            ControlRequest::Workspace(WorkspaceRequest::ListWindows) => {
                Ok(ControlResponse::Windows(self.windows.clone()))
            }
            ControlRequest::Workspace(WorkspaceRequest::GetState) => {
                Ok(ControlResponse::Workspace(WorkspaceInfo {
                    scene_visible: true,
                }))
            }
            ControlRequest::Workspace(WorkspaceRequest::OpenWindow { title, owner: _ }) => {
                let info = WindowInfo {
                    id: self.next_window(),
                    title: title.unwrap_or_else(|| "Window".into()),
                    owner: Some(principal.owner.clone()),
                };
                self.windows.push(info.clone());
                Ok(ControlResponse::Window(info))
            }
            ControlRequest::Workspace(WorkspaceRequest::AddPlot {
                window,
                direction: _,
                owner: _,
            }) => {
                let window = window.unwrap_or(0);
                if window != 0 && !self.windows.iter().any(|item| item.id == window) {
                    return Err(Error::not_found("fixture window does not exist"));
                }
                let info = PlotInfo {
                    window,
                    tile: self.next_tile(),
                    instance_id: self.next_instance(),
                    index: self
                        .plots
                        .iter()
                        .filter(|item| item.window == window)
                        .count(),
                    label: "Plot".into(),
                    owner: Some(principal.owner.clone()),
                };
                self.plots.push(info.clone());
                Ok(ControlResponse::Plots(vec![info]))
            }
            ControlRequest::Playback(PlaybackRequest::Get) => {
                Ok(ControlResponse::Playback(PlaybackInfo {
                    speed: 1.0,
                    follow_live: false,
                }))
            }
            ControlRequest::Plots(PlotRequest::List { window }) => Ok(ControlResponse::Plots(
                self.plots
                    .iter()
                    .filter(|item| window.is_none_or(|window| item.window == window))
                    .cloned()
                    .collect(),
            )),
            ControlRequest::Traces(TraceRequest::List { window, tile }) => {
                Ok(ControlResponse::Traces(
                    self.traces
                        .iter()
                        .filter(|item| item.window == window && item.tile == tile)
                        .map(|item| item.info.clone())
                        .collect(),
                ))
            }
            ControlRequest::Traces(TraceRequest::AddReturning {
                window,
                tile,
                field_id,
                field,
                color,
                width_px,
                mode,
                owner: _,
            }) => {
                self.plot(window, tile)?;
                let info = TraceInfo {
                    instance_id: self.next_instance(),
                    index: self.traces.len(),
                    field_id,
                    field,
                    color: color.unwrap_or(DEFAULT_COLOR),
                    width_px: width_px.unwrap_or(1.5),
                    mode,
                    visible: true,
                    owner: Some(principal.owner.clone()),
                };
                self.traces.push(FixtureTrace {
                    window,
                    tile,
                    info: info.clone(),
                });
                Ok(ControlResponse::Trace(info))
            }
            ControlRequest::Annotations(AnnotationRequest::List { target }) => {
                Ok(ControlResponse::Annotations(
                    self.annotations
                        .iter()
                        .filter(|item| {
                            target.is_none_or(|(window, tile)| {
                                item.window == window && item.tile == tile
                            })
                        })
                        .cloned()
                        .collect(),
                ))
            }
            ControlRequest::Annotations(AnnotationRequest::Add {
                window,
                tile,
                geometry,
                label,
                style,
                owner: _,
            }) => {
                let plot = self.plot(window, tile)?.clone();
                let info = AnnotationInfo {
                    plot_instance_id: plot.instance_id,
                    window,
                    tile,
                    id: self.next_annotation(),
                    index: self.annotations.len(),
                    kind: geometry.kind(),
                    geometry,
                    label,
                    color: style.color.unwrap_or(DEFAULT_COLOR),
                    owner: Some(principal.owner.name.clone()),
                };
                self.annotations.push(info.clone());
                Ok(ControlResponse::Annotations(vec![info]))
            }
            ControlRequest::Markers(MarkerRequest::List) => {
                Ok(ControlResponse::Markers(self.markers.clone()))
            }
            ControlRequest::Markers(MarkerRequest::AppendReturning { owner, marker, .. }) => {
                let info = MarkerInfo {
                    id: self.next_marker(),
                    index: self.markers.len(),
                    t_us: marker.time_us,
                    label: marker.label,
                    color: marker.color.unwrap_or(DEFAULT_COLOR),
                    note: marker.note,
                    origin: MarkerOrigin::Script,
                    owner: Some(owner),
                };
                self.markers.push(info.clone());
                Ok(ControlResponse::Marker(info))
            }
            ControlRequest::Vehicles(request) => match *request {
                VehicleRequest::List => Ok(ControlResponse::Vehicles(self.vehicles.clone())),
                VehicleRequest::Add(mut spec) => {
                    spec.owner = Some(principal.owner.clone());
                    let info = VehicleInfo {
                        id: self.next_vehicle(),
                        index: self.vehicles.len(),
                        spec,
                    };
                    self.vehicles.push(info.clone());
                    Ok(ControlResponse::Vehicles(vec![info]))
                }
                _ => Err(Error::protocol("unsupported vehicle request in fixture")),
            },
            ControlRequest::Layouts(LayoutRequest::List) => Ok(ControlResponse::Names(vec![])),
            ControlRequest::Layouts(LayoutRequest::Current) => {
                Ok(ControlResponse::Layout("{}".into()))
            }
            ControlRequest::Generation(GenerationRequest::RemoveOwned { owner }) => {
                if owner != principal.owner.name {
                    return Err(Error::forbidden(
                        "fixture cleanup owner does not match principal",
                    ));
                }
                let before = self.windows.len()
                    + self.plots.len()
                    + self.traces.len()
                    + self.annotations.len()
                    + self.vehicles.len()
                    + self.markers.len();
                self.windows
                    .retain(|item| item.owner.as_ref().is_none_or(|item| item.name != owner));
                self.plots
                    .retain(|item| item.owner.as_ref().is_none_or(|item| item.name != owner));
                self.traces.retain(|item| {
                    item.info
                        .owner
                        .as_ref()
                        .is_none_or(|item| item.name != owner)
                });
                self.annotations
                    .retain(|item| item.owner.as_deref() != Some(owner.as_str()));
                self.vehicles.retain(|item| {
                    item.spec
                        .owner
                        .as_ref()
                        .is_none_or(|item| item.name != owner)
                });
                self.markers
                    .retain(|item| item.owner.as_deref() != Some(owner.as_str()));
                let after = self.windows.len()
                    + self.plots.len()
                    + self.traces.len()
                    + self.annotations.len()
                    + self.vehicles.len()
                    + self.markers.len();
                Ok(ControlResponse::Removed(before - after))
            }
            _ => Err(Error::protocol("unsupported control request in fixture")),
        }
    }

    fn plot(&self, window: u64, tile: u64) -> Result<&PlotInfo> {
        self.plots
            .iter()
            .find(|item| item.window == window && item.tile == tile)
            .ok_or_else(|| Error::not_found("fixture plot does not exist"))
    }

    fn validate_guard(&self, guard: ResourceGuard) -> Result<()> {
        let valid = match guard {
            ResourceGuard::Plot {
                window,
                tile,
                instance_id,
            } => self.plots.iter().any(|item| {
                item.window == window && item.tile == tile && item.instance_id == instance_id
            }),
            ResourceGuard::Trace {
                window,
                tile,
                plot_instance_id,
                index,
                trace_instance_id,
            } => {
                self.plots.iter().any(|item| {
                    item.window == window
                        && item.tile == tile
                        && item.instance_id == plot_instance_id
                }) && self.traces.iter().any(|item| {
                    item.window == window
                        && item.tile == tile
                        && item.info.index == index
                        && item.info.instance_id == trace_instance_id
                })
            }
            ResourceGuard::Annotation {
                window,
                tile,
                plot_instance_id,
                id,
            } => self.annotations.iter().any(|item| {
                item.window == window
                    && item.tile == tile
                    && item.plot_instance_id == plot_instance_id
                    && item.id == id
            }),
        };
        if valid {
            Ok(())
        } else {
            Err(Error::stale_handle("fixture resource guard is stale"))
        }
    }
}

struct RecordingControlHost {
    state: Mutex<FixtureState>,
}

impl RecordingControlHost {
    fn new() -> Self {
        Self {
            state: Mutex::new(FixtureState::new()),
        }
    }

    fn recording(&self) -> Value {
        let state = self.state.lock().expect("fixture state poisoned");
        json!({"calls": state.calls, "rejected": state.rejected})
    }
}

impl AuthorizedControlHost for RecordingControlHost {
    fn call_as(
        &self,
        principal: ControlPrincipal,
        request: ControlRequest,
    ) -> Result<ControlResponse> {
        principal.owner.validate()?;
        request.validate()?;
        let record = RecordedCall {
            access: match principal.access {
                AccessMode::Safe => "safe",
                AccessMode::Full => "full",
            },
            owner: principal.owner.name.clone(),
            generation: principal.owner.generation,
            caller_owner: caller_owner(&request),
            request: request_name(&request),
            query: is_query(&request),
        };
        let mut state = self.state.lock().expect("fixture state poisoned");
        state.calls.push(record);
        let result = state.apply(&principal, request);
        if let Err(error) = &result {
            state.rejected.push(error.to_string());
        }
        result
    }
}

fn caller_owner(request: &ControlRequest) -> Option<String> {
    match request {
        ControlRequest::Guarded { request, .. } => caller_owner(request),
        ControlRequest::Workspace(WorkspaceRequest::OpenWindow { owner, .. })
        | ControlRequest::Workspace(WorkspaceRequest::AddPlot { owner, .. })
        | ControlRequest::Workspace(WorkspaceRequest::Split { owner, .. }) => {
            owner.as_ref().map(|item| item.name.clone())
        }
        ControlRequest::Traces(TraceRequest::Add { owner, .. })
        | ControlRequest::Traces(TraceRequest::AddReturning { owner, .. }) => {
            owner.as_ref().map(|item| item.name.clone())
        }
        ControlRequest::Annotations(AnnotationRequest::Add { owner, .. }) => {
            owner.as_ref().map(|item| item.name.clone())
        }
        ControlRequest::Vehicles(request) => match request.as_ref() {
            VehicleRequest::Add(spec) => spec.owner.as_ref().map(|item| item.name.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn is_query(request: &ControlRequest) -> bool {
    match request {
        ControlRequest::Guarded { request, .. } => is_query(request),
        ControlRequest::Workspace(WorkspaceRequest::ListWindows | WorkspaceRequest::GetState)
        | ControlRequest::Playback(PlaybackRequest::Get)
        | ControlRequest::Plots(PlotRequest::List { .. } | PlotRequest::Focused)
        | ControlRequest::Traces(TraceRequest::List { .. })
        | ControlRequest::Annotations(AnnotationRequest::List { .. })
        | ControlRequest::Markers(MarkerRequest::List)
        | ControlRequest::Layouts(LayoutRequest::List | LayoutRequest::Current) => true,
        ControlRequest::Vehicles(request) => matches!(request.as_ref(), VehicleRequest::List),
        _ => false,
    }
}

fn request_name(request: &ControlRequest) -> String {
    match request {
        ControlRequest::Guarded { request, .. } => format!("guarded.{}", request_name(request)),
        ControlRequest::Workspace(WorkspaceRequest::ListWindows) => "workspace.list_windows".into(),
        ControlRequest::Workspace(WorkspaceRequest::GetState) => "workspace.get_state".into(),
        ControlRequest::Workspace(WorkspaceRequest::OpenWindow { .. }) => {
            "workspace.open_window".into()
        }
        ControlRequest::Workspace(WorkspaceRequest::AddPlot { .. }) => "workspace.add_plot".into(),
        ControlRequest::Playback(PlaybackRequest::Get) => "playback.get".into(),
        ControlRequest::Plots(PlotRequest::List { .. }) => "plots.list".into(),
        ControlRequest::Traces(TraceRequest::List { .. }) => "traces.list".into(),
        ControlRequest::Traces(TraceRequest::AddReturning { .. }) => "traces.add_returning".into(),
        ControlRequest::Annotations(AnnotationRequest::List { .. }) => "annotations.list".into(),
        ControlRequest::Annotations(AnnotationRequest::Add { .. }) => "annotations.add".into(),
        ControlRequest::Markers(MarkerRequest::List) => "markers.list".into(),
        ControlRequest::Markers(MarkerRequest::AppendReturning { .. }) => {
            "markers.append_returning".into()
        }
        ControlRequest::Vehicles(request) => match request.as_ref() {
            VehicleRequest::List => "vehicles.list".into(),
            VehicleRequest::Add(_) => "vehicles.add".into(),
            _ => "vehicles.unsupported".into(),
        },
        ControlRequest::Layouts(LayoutRequest::List) => "layouts.list".into(),
        ControlRequest::Layouts(LayoutRequest::Current) => "layouts.current".into(),
        ControlRequest::Generation(GenerationRequest::RemoveOwned { .. }) => {
            "generation.remove_owned".into()
        }
        _ => "unsupported".into(),
    }
}

fn schema(name: &str, fields: &[(&str, Option<&str>)]) -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            name,
            fields.iter().map(|(name, unit)| {
                FieldSchema::new(*name, DataType::Float64, *unit, 1.0).unwrap()
            }),
        )
        .unwrap(),
    )
}

fn batch(
    source: delog_core::identity::SourceId,
    schema: Arc<TopicSchema>,
    columns: &[&[f64]],
) -> ParsedBatch {
    ParsedBatch::new(
        source,
        schema,
        Int64Array::from(vec![1_000, 2_000, 3_000]),
        columns
            .iter()
            .map(|values| Arc::new(Float64Array::from(values.to_vec())) as ArrayRef)
            .collect(),
    )
}

fn build_store() -> (Arc<DataStore>, IngestSender, thread::JoinHandle<()>) {
    let ingestor = Ingestor::new(NullObserver);
    let store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let writer = thread::spawn(move || ingestor.run(receiver));

    let mut sink = sender.file_sink();
    let source = sink.open_source("flight", SourceKind::File);
    let attitude = schema(
        "vehicle_attitude",
        &[
            ("roll", Some("rad")),
            ("pitch", Some("rad")),
            ("yaw", Some("rad")),
            ("roll_setpoint", Some("rad")),
        ],
    );
    sink.submit(batch(
        source,
        attitude,
        &[
            &[0.1, 0.2, 0.3],
            &[0.0, 0.1, 0.2],
            &[0.4, 0.5, 0.6],
            &[0.15, 0.35, 0.55],
        ],
    ));
    let position = schema(
        "vehicle_local_position",
        &[("x", Some("m")), ("y", Some("m")), ("z", Some("m"))],
    );
    sink.submit(batch(
        source,
        position,
        &[&[0.0, 1.0, 2.0], &[0.0, -1.0, -2.0], &[-10.0, -11.0, -12.0]],
    ));
    sink.close_source(
        source,
        ParseSummary {
            topic_count: 2,
            row_count: 6,
            ..ParseSummary::default()
        },
    );
    drop(sink);
    sender
        .publication_barrier()
        .expect("fixture ingest writer disconnected")
        .recv_timeout(Duration::from_secs(5))
        .expect("fixture publication barrier timed out")
        .expect("fixture publication barrier failed");
    (store, sender, writer)
}

fn main() {
    let discovery_root: PathBuf = env::var("DELOG_TEST_DISCOVERY_DIR")
        .map(PathBuf::from)
        .expect("DELOG_TEST_DISCOVERY_DIR must be set");
    let (store, ingest, writer) = build_store();
    let host = Arc::new(RecordingControlHost::new());
    let server = RemoteServer::spawn(
        RemoteConfig {
            label: "control fixture".into(),
            loaded_file: Some("flight.ulg".into()),
            lease_idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(5),
            max_concurrent_downloads: 4,
            discovery_root: Some(discovery_root),
            control: ControlLimits::default(),
            uploads: UploadConfig::default(),
        },
        RemoteServices {
            store,
            ingest: ingest.clone(),
            control: host.clone(),
            owners: Arc::new(OwnerRegistry::new()),
        },
    )
    .expect("control fixture server failed to start");

    println!("READY {}", server.instance_id());
    io::stdout().flush().expect("failed to flush READY");
    for line in io::stdin().lock().lines() {
        match line.as_deref() {
            Ok("calls") => {
                println!("{}", host.recording());
                io::stdout()
                    .flush()
                    .expect("failed to flush recorded calls");
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    server
        .shutdown()
        .expect("control fixture server failed to shut down");
    drop(ingest);
    writer.join().expect("fixture ingest thread failed");
}
