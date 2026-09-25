use delog_api::Result;
use delog_api::control::{
    AccessMode, AuthorizedControlHost, ControlPrincipal, ControlRequest, ControlResponse,
    ResourceOwner, WindowInfo, WorkspaceRequest,
};
use delog_remote::ControlCommandDto;
use delog_remote::control::map::{ControlFieldResolver, ControlMapper, ResolvedControlField};
use delog_remote::{ControlHandleRegistry, OpaqueId};
use std::sync::Mutex;

struct WindowHost(Mutex<Vec<ControlRequest>>);

impl AuthorizedControlHost for WindowHost {
    fn call_as(&self, _: ControlPrincipal, request: ControlRequest) -> Result<ControlResponse> {
        self.0.lock().unwrap().push(request);
        Ok(ControlResponse::Window(WindowInfo {
            id: 17,
            title: "Flight diagnosis".into(),
            owner: None,
        }))
    }
}

struct NoFields;
impl ControlFieldResolver for NoFields {
    fn resolve(
        &self,
        _: &OpaqueId,
    ) -> std::result::Result<ResolvedControlField, delog_remote::ApiError> {
        Err(delog_remote::ApiError::stale_handle("field is stale"))
    }
}

#[test]
fn window_open_uses_authorized_host_and_returns_opaque_handle() {
    let host = WindowHost(Mutex::new(Vec::new()));
    let mut handles = ControlHandleRegistry::new();
    let principal = ControlPrincipal {
        owner: ResourceOwner {
            name: "client".into(),
            generation: 2,
        },
        access: AccessMode::Safe,
    };
    let mut mapper = ControlMapper::new(&host, principal, &mut handles, &NoFields);
    let result = mapper
        .execute(ControlCommandDto::WindowOpen {
            title: Some("Flight diagnosis".into()),
        })
        .unwrap();
    let delog_remote::ControlResultDto::Resource { handle, .. } = result else {
        panic!("expected resource")
    };
    assert_ne!(handle.as_str(), "17");
    assert_eq!(
        host.0.lock().unwrap().as_slice(),
        &[ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
            title: Some("Flight diagnosis".into()),
            owner: None
        })]
    );
}

struct StateHost;
impl AuthorizedControlHost for StateHost {
    fn call_as(&self, _: ControlPrincipal, request: ControlRequest) -> Result<ControlResponse> {
        use delog_api::control::*;
        Ok(match request {
            ControlRequest::Workspace(WorkspaceRequest::ListWindows) => {
                ControlResponse::Windows(vec![
                    WindowInfo {
                        id: 0,
                        title: "Main".into(),
                        owner: None,
                    },
                    WindowInfo {
                        id: 8,
                        title: "Empty".into(),
                        owner: None,
                    },
                ])
            }
            ControlRequest::Workspace(WorkspaceRequest::GetState) => {
                ControlResponse::Workspace(WorkspaceInfo {
                    scene_visible: true,
                })
            }
            ControlRequest::Playback(PlaybackRequest::Get) => {
                ControlResponse::Playback(PlaybackInfo {
                    speed: 2.0,
                    follow_live: true,
                })
            }
            ControlRequest::Plots(PlotRequest::List { window: None }) => {
                ControlResponse::Plots(vec![])
            }
            ControlRequest::Annotations(AnnotationRequest::List { target: None }) => {
                ControlResponse::Annotations(vec![])
            }
            ControlRequest::Markers(MarkerRequest::List) => ControlResponse::Markers(vec![]),
            ControlRequest::Vehicles(_) => {
                let field = |name: &str| ResolvedVehicleField {
                    id: delog_core::identity::FieldId(9),
                    path: format!("flight/GPS/{name}"),
                };
                ControlResponse::Vehicles(vec![VehicleInfo {
                    id: 5,
                    index: 0,
                    spec: VehicleSpec {
                        source_id: delog_core::identity::SourceId(3),
                        source: "flight".into(),
                        label: "Aircraft".into(),
                        show: true,
                        show_path: false,
                        position: VehiclePosition::Gps {
                            lat: field("Lat"),
                            lon: field("Lon"),
                            alt: field("Alt"),
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
                    },
                }])
            }
            ControlRequest::Layouts(LayoutRequest::List) => {
                ControlResponse::Names(vec!["A".into()])
            }
            ControlRequest::Layouts(LayoutRequest::Current) => ControlResponse::Layout("{}".into()),
            other => panic!("unexpected query: {other:?}"),
        })
    }
}

#[test]
fn state_includes_empty_windows_and_native_scene_and_playback() {
    let mut handles = ControlHandleRegistry::new();
    let principal = ControlPrincipal {
        owner: ResourceOwner {
            name: "client".into(),
            generation: 2,
        },
        access: AccessMode::Safe,
    };
    let mut mapper = ControlMapper::new(&StateHost, principal, &mut handles, &NoFields);
    let state = mapper.state().unwrap();
    assert_eq!(state.windows.len(), 2);
    assert!(state.plots.is_empty());
    assert!(state.scene_visible);
    assert_eq!(state.playback.speed, 2.0);
    assert!(state.playback.follow_live);
    assert_eq!(state.layout_names, ["A"]);
    let vehicle = serde_json::to_value(&state.vehicles[0]).unwrap();
    assert_eq!(vehicle["position"]["lat"]["path"], "flight/GPS/Lat");
    assert_eq!(vehicle["orientation"]["kind"], "static");
}

#[test]
fn fractional_microsecond_marker_time_fails_before_host_call() {
    let host = WindowHost(Mutex::new(Vec::new()));
    let mut handles = ControlHandleRegistry::new();
    let principal = ControlPrincipal {
        owner: ResourceOwner {
            name: "client".into(),
            generation: 2,
        },
        access: AccessMode::Safe,
    };
    let mut mapper = ControlMapper::new(&host, principal, &mut handles, &NoFields);
    let error = mapper
        .execute(ControlCommandDto::MarkerAdd {
            time_ns: 1001,
            label: "x".into(),
            color: None,
            note: None,
        })
        .unwrap_err();
    assert_eq!(error.code(), "invalid_input");
    assert!(host.0.lock().unwrap().is_empty());
}

struct ProfileHost;
impl AuthorizedControlHost for ProfileHost {
    fn call_as(&self, _: ControlPrincipal, request: ControlRequest) -> Result<ControlResponse> {
        use delog_api::control::*;
        assert_eq!(
            request,
            ControlRequest::VehicleProfiles(VehicleProfileRequest::Load {
                name: "flight".into()
            })
        );
        let field = |name: &str| ProfileFieldRef {
            topic: "GPS".into(),
            field: name.into(),
        };
        Ok(ControlResponse::VehicleProfile(Box::new(
            VehicleProfileInfo {
                name: "flight".into(),
                label: "Aircraft".into(),
                show: true,
                show_path: false,
                position: ProfilePosition::Gps {
                    lat: field("Lat"),
                    lon: field("Lon"),
                    alt: field("Alt"),
                    lat_lon_dege7: true,
                    alt_mm: false,
                    alt_offset_m: 3.0,
                },
                orientation: ProfileOrientation::Euler {
                    roll: field("Roll"),
                    pitch: field("Pitch"),
                    yaw: field("Yaw"),
                    degrees: true,
                },
                model: VehicleModel::Quad,
                color: [1.0, 0.0, 0.0, 1.0],
                path_color: [0.0, 1.0, 0.0, 1.0],
                scale: 2.0,
            },
        )))
    }
}

#[test]
fn loaded_vehicle_profile_retains_all_field_bindings() {
    let mut handles = ControlHandleRegistry::new();
    let principal = ControlPrincipal {
        owner: ResourceOwner {
            name: "client".into(),
            generation: 2,
        },
        access: AccessMode::Safe,
    };
    let mut mapper = ControlMapper::new(&ProfileHost, principal, &mut handles, &NoFields);
    let value = serde_json::to_value(
        mapper
            .execute(ControlCommandDto::VehicleProfileLoad {
                name: "flight".into(),
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["profile"]["position"]["lat"],
        serde_json::json!({"topic":"GPS","field":"Lat"})
    );
    assert_eq!(
        value["profile"]["orientation"]["yaw"],
        serde_json::json!({"topic":"GPS","field":"Yaw"})
    );
    assert_eq!(value["profile"]["position"]["alt_offset_m"], 3.0);
}

struct InterleavedCreationHost(Mutex<Vec<ControlRequest>>);

impl AuthorizedControlHost for InterleavedCreationHost {
    fn call_as(&self, _: ControlPrincipal, request: ControlRequest) -> Result<ControlResponse> {
        use delog_api::control::{
            MarkerInfo, MarkerOrigin, MarkerRequest, TraceInfo, TraceMode, TraceRequest,
        };
        let mut calls = self.0.lock().unwrap();
        let inner = match &request {
            ControlRequest::Guarded { request, .. } => request.as_ref(),
            other => other,
        };
        let response = match inner {
            ControlRequest::Traces(TraceRequest::AddReturning { .. }) => {
                ControlResponse::Trace(TraceInfo {
                    instance_id: 42,
                    index: 1,
                    field_id: delog_core::identity::FieldId(9),
                    field: "flight/IMU/A".into(),
                    color: [1.0; 4],
                    width_px: 1.0,
                    mode: TraceMode::Line,
                    visible: true,
                    owner: None,
                })
            }
            ControlRequest::Markers(MarkerRequest::AppendReturning { .. }) => {
                ControlResponse::Marker(MarkerInfo {
                    id: 7,
                    index: 1,
                    t_us: 2,
                    label: "mine".into(),
                    color: [1.0; 4],
                    note: String::new(),
                    origin: MarkerOrigin::Script,
                    owner: None,
                })
            }
            other => panic!("unexpected request: {other:?}"),
        };
        calls.push(request);
        Ok(response)
    }
}

struct OneField;
impl ControlFieldResolver for OneField {
    fn resolve(
        &self,
        _: &OpaqueId,
    ) -> std::result::Result<ResolvedControlField, delog_remote::ApiError> {
        Ok(ResolvedControlField {
            id: delog_core::identity::FieldId(9),
            trace_path: "IMU.A".into(),
            vehicle_path: "flight/IMU/A".into(),
            source_id: delog_core::identity::SourceId(1),
            source: "flight".into(),
            topic: "IMU".into(),
            name: "A".into(),
        })
    }
}

#[test]
fn trace_creation_handle_is_the_exact_trace_created_by_that_call() {
    let host = InterleavedCreationHost(Mutex::new(Vec::new()));
    let mut handles = ControlHandleRegistry::new();
    let plot = handles.register_plot(delog_remote::NativePlotKey::with_instance_id(0, 7, 3));
    let principal = ControlPrincipal {
        owner: ResourceOwner {
            name: "client".into(),
            generation: 1,
        },
        access: AccessMode::Safe,
    };
    let mut mapper = ControlMapper::new(&host, principal, &mut handles, &OneField);
    let result = mapper
        .execute(ControlCommandDto::TraceAdd {
            plot,
            field: OpaqueId::new("field"),
            color: None,
            width_px: None,
            mode: "line".into(),
        })
        .unwrap();
    let delog_remote::ControlResultDto::Resource { handle, .. } = result else {
        panic!("expected trace handle")
    };
    let (key, index, field, trace_instance_id) = handles.resolve_trace(&handle).unwrap();
    assert_eq!(key, delog_remote::NativePlotKey::with_instance_id(0, 7, 3));
    assert_eq!(
        (index, field, trace_instance_id),
        (1, delog_core::identity::FieldId(9), 42)
    );
    let calls = host.0.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(matches!(
        &calls[0],
        ControlRequest::Guarded {
            guard: delog_api::control::ResourceGuard::Plot {
                window: 0,
                tile: 7,
                instance_id: 3
            },
            ..
        }
    ));
}

#[test]
fn marker_creation_handle_is_the_exact_marker_created_by_that_call() {
    let host = InterleavedCreationHost(Mutex::new(Vec::new()));
    let mut handles = ControlHandleRegistry::new();
    let principal = ControlPrincipal {
        owner: ResourceOwner {
            name: "client".into(),
            generation: 1,
        },
        access: AccessMode::Safe,
    };
    let mut mapper = ControlMapper::new(&host, principal, &mut handles, &NoFields);
    let result = mapper
        .execute(ControlCommandDto::MarkerAdd {
            time_ns: 2_000,
            label: "mine".into(),
            color: None,
            note: None,
        })
        .unwrap();
    let delog_remote::ControlResultDto::Resource { handle, .. } = result else {
        panic!("expected marker handle")
    };
    assert_eq!(handles.resolve_marker(&handle).unwrap(), 7);
    assert_eq!(host.0.lock().unwrap().len(), 1);
}

#[test]
fn window_open_json_is_stable_and_rejects_unknown_fields() {
    let json = r#"{"op":"window_open","title":"Flight diagnosis"}"#;
    let command: ControlCommandDto = serde_json::from_str(json).unwrap();
    assert_eq!(serde_json::to_string(&command).unwrap(), json);
    assert!(
        serde_json::from_str::<ControlCommandDto>(
            r#"{"op":"window_open","title":"x","owner":"forged"}"#
        )
        .is_err()
    );
}

#[test]
fn omitted_optional_control_fields_use_defaults_without_native_ids() {
    let command: ControlCommandDto = serde_json::from_str(r#"{"op":"window_open"}"#).unwrap();
    assert_eq!(command, ControlCommandDto::WindowOpen { title: None });
    assert_eq!(
        serde_json::to_string(&command).unwrap(),
        r#"{"op":"window_open"}"#
    );
    let command: ControlCommandDto =
        serde_json::from_str(r#"{"op":"trace_set","trace":"opaque"}"#).unwrap();
    assert!(matches!(
        command,
        ControlCommandDto::TraceSet {
            color: None,
            width_px: None,
            mode: None,
            visible: None,
            ..
        }
    ));
}

struct WindowedHost(Mutex<Vec<ControlRequest>>);

impl AuthorizedControlHost for WindowedHost {
    fn call_as(&self, _: ControlPrincipal, request: ControlRequest) -> Result<ControlResponse> {
        self.0.lock().unwrap().push(request.clone());
        Ok(match request {
            ControlRequest::Workspace(WorkspaceRequest::OpenWindow { .. }) => {
                ControlResponse::Window(WindowInfo {
                    id: 17,
                    title: "Second".into(),
                    owner: None,
                })
            }
            ControlRequest::Workspace(WorkspaceRequest::AddPlot { window, .. }) => {
                ControlResponse::Plots(vec![delog_api::control::PlotInfo {
                    window: window.unwrap_or(0),
                    tile: 4,
                    instance_id: 11,
                    index: 0,
                    label: "plot".into(),
                    owner: None,
                }])
            }
            _ => ControlResponse::Unit,
        })
    }
}

#[test]
fn add_plot_and_equalize_target_a_window_handle() {
    use delog_remote::ControlResultDto;
    let host = WindowedHost(Mutex::new(Vec::new()));
    let mut handles = ControlHandleRegistry::new();
    let principal = ControlPrincipal {
        owner: ResourceOwner {
            name: "client".into(),
            generation: 2,
        },
        access: AccessMode::Full,
    };
    let mut mapper = ControlMapper::new(&host, principal, &mut handles, &NoFields);
    let ControlResultDto::Resource { handle: second, .. } = mapper
        .execute(ControlCommandDto::WindowOpen { title: None })
        .unwrap()
    else {
        panic!("expected window")
    };
    let ControlResultDto::Resource {
        handle: plot,
        window,
    } = mapper
        .execute(ControlCommandDto::WorkspaceAddPlot {
            window: Some(second.clone()),
            direction: "vertical".into(),
        })
        .unwrap()
    else {
        panic!("expected plot")
    };
    assert_eq!(window.as_ref(), Some(&second));
    let ControlResultDto::Resource { window: main, .. } = mapper
        .execute(ControlCommandDto::WorkspaceAddPlot {
            window: None,
            direction: "vertical".into(),
        })
        .unwrap()
    else {
        panic!("expected plot")
    };
    assert!(main.is_some());
    assert_ne!(main, Some(second.clone()));
    mapper
        .execute(ControlCommandDto::WorkspaceEqualize {
            window: Some(second.clone()),
        })
        .unwrap();
    let error = mapper
        .execute(ControlCommandDto::WorkspaceAddPlot {
            window: Some(plot),
            direction: "vertical".into(),
        })
        .unwrap_err();
    assert_eq!(error.code(), "stale_handle");
    let requests = host.0.lock().unwrap();
    assert!(matches!(
        requests[1],
        ControlRequest::Workspace(WorkspaceRequest::AddPlot {
            window: Some(17),
            ..
        })
    ));
    assert!(matches!(
        requests[2],
        ControlRequest::Workspace(WorkspaceRequest::AddPlot { window: None, .. })
    ));
    assert_eq!(
        requests[3],
        ControlRequest::Workspace(WorkspaceRequest::Equalize { window: Some(17) })
    );
    assert_eq!(requests.len(), 4);
}

#[test]
fn window_targets_are_optional_on_the_wire() {
    for json in [
        r#"{"op":"workspace_add_plot","direction":"vertical"}"#,
        r#"{"op":"workspace_add_plot","window":"w1","direction":"vertical"}"#,
        r#"{"op":"workspace_equalize"}"#,
        r#"{"op":"workspace_equalize","window":"w1"}"#,
        r#"{"kind":"resource","handle":"h1"}"#,
    ] {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let encoded = if value.get("op").is_some() {
            let command: ControlCommandDto = serde_json::from_str(json).unwrap();
            serde_json::to_string(&command).unwrap()
        } else {
            let result: delog_remote::ControlResultDto = serde_json::from_str(json).unwrap();
            serde_json::to_string(&result).unwrap()
        };
        assert_eq!(encoded, json);
    }
    assert!(
        serde_json::from_str::<ControlCommandDto>(r#"{"op":"workspace_equalize","plot":"p"}"#)
            .is_err()
    );
}
