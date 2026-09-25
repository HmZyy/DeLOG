use std::sync::Arc;

use delog_api::control::{
    AccessMode, AnnotationFilter, AnnotationGeometry, AnnotationRequest, AnnotationStylePatch,
    ControlCall, ControlPrincipal, ControlRequest, ControlResponse, GenerationRequest,
    LayoutRequest, MarkerFilter, MarkerPatch, MarkerRequest, PlaybackRequest, PlotRequest,
    ResolvedVehicleField, ResourceGuard, ResourceOwner, SplitDirection, TraceMode, TraceRequest,
    VehicleFilter, VehicleModel, VehicleOrientation, VehiclePatch, VehiclePosition,
    VehicleProfileRequest, VehicleRequest, VehicleSpec, WorkspaceRequest,
};
use delog_api::markers::PendingMarker;
use delog_api::{ErrorKind, Result};
use delog_cache::CacheManager;
use delog_core::identity::{FieldId, IdentityRegistry, SourceId};
use delog_core::snapshot::StoreSnapshot;

use super::authorize_and_stamp;
use crate::plotting::markers::Markers;
use crate::plotting::timeline::Playback;
use crate::shell::app::control_service::{self, AppControl};
use crate::shell::windows::ExtendedWindow;
use crate::shell::workspace::Workspace;

fn owner(name: &str, generation: u64) -> ResourceOwner {
    ResourceOwner {
        name: name.into(),
        generation,
    }
}

fn principal(access: AccessMode) -> ControlPrincipal {
    ControlPrincipal {
        owner: owner("flight-diagnosis", 7),
        access,
    }
}

struct PolicyFixture {
    markers: Markers,
    workspace: Workspace,
    windows: Vec<ExtendedWindow>,
    playback: Playback,
    next_window_id: u64,
    caches: CacheManager,
    snapshot: StoreSnapshot,
    source: SourceId,
    fields: [FieldId; 3],
    vehicles: Vec<crate::scene3d::vehicle::VehicleConfig>,
    next_vehicle_id: u64,
    vehicle_revision: u64,
    traj_dirty: bool,
}

impl PolicyFixture {
    fn new() -> Self {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "gps").unwrap();
        let fields = [
            identity.add_field(topic, "lat").unwrap(),
            identity.add_field(topic, "lon").unwrap(),
            identity.add_field(topic, "alt").unwrap(),
        ];
        Self {
            markers: Markers::new(),
            workspace: Workspace::new(),
            windows: Vec::new(),
            playback: Playback::default(),
            next_window_id: 1,
            caches: CacheManager::new(),
            snapshot: StoreSnapshot::from_registry(&identity, [], 0).unwrap(),
            source,
            fields,
            vehicles: Vec::new(),
            next_vehicle_id: 1,
            vehicle_revision: 0,
            traj_dirty: false,
        }
    }

    fn root(&self) -> u64 {
        self.workspace.tree.root().unwrap().0
    }

    fn control(&mut self) -> AppControl<'_> {
        AppControl {
            markers: &mut self.markers,
            workspace: &mut self.workspace,
            windows: &mut self.windows,
            playback: &mut self.playback,
            next_window_id: &mut self.next_window_id,
            caches: &mut self.caches,
            snapshot: &self.snapshot,
            vehicles: &mut self.vehicles,
            next_vehicle_id: &mut self.next_vehicle_id,
            vehicle_revision: &mut self.vehicle_revision,
            traj_dirty: &mut self.traj_dirty,
            vehicle_profiles: None,
        }
    }

    fn authorize(&mut self, access: AccessMode, request: ControlRequest) -> Result<ControlRequest> {
        authorize_and_stamp(&mut self.control(), &principal(access), request)
    }

    fn apply_trusted(&mut self, request: ControlRequest) -> ControlResponse {
        control_service::apply(&mut self.control(), request).unwrap()
    }

    fn apply_external(
        &mut self,
        access: AccessMode,
        request: ControlRequest,
    ) -> Result<ControlResponse> {
        control_service::apply_call(
            &mut self.control(),
            ControlCall::External {
                principal: principal(access),
                request,
            },
        )
    }

    fn add_trace(&mut self, trace_owner: Option<ResourceOwner>) {
        let root = self.root();
        let field_id = self.fields[0];
        self.apply_trusted(ControlRequest::Traces(TraceRequest::Add {
            window: 0,
            tile: root,
            field_id,
            field: "gps.lat".into(),
            color: None,
            width_px: None,
            mode: TraceMode::Line,
            owner: trace_owner,
        }));
    }

    fn add_annotation(&mut self, annotation_owner: Option<ResourceOwner>, label: &str) -> u64 {
        let root = self.root();
        self.apply_trusted(ControlRequest::Annotations(AnnotationRequest::Add {
            window: 0,
            tile: root,
            geometry: AnnotationGeometry::Text { at: (1, 1.0) },
            label: label.into(),
            style: AnnotationStylePatch::default(),
            owner: annotation_owner,
        }))
        .into_annotations()
        .unwrap()[0]
            .id
    }

    fn add_owned_marker(&mut self, marker_owner: &str) -> u64 {
        self.apply_trusted(ControlRequest::Markers(MarkerRequest::Append {
            owner: marker_owner.into(),
            generation: 1,
            markers: vec![PendingMarker {
                time_us: 10,
                label: marker_owner.into(),
                color: None,
                note: String::new(),
            }],
        }));
        self.markers
            .marker_infos()
            .into_iter()
            .find(|marker| marker.owner.as_deref() == Some(marker_owner))
            .unwrap()
            .id
    }

    fn vehicle_spec(&self, vehicle_owner: Option<ResourceOwner>) -> VehicleSpec {
        let field = |index: usize, name: &str| ResolvedVehicleField {
            id: self.fields[index],
            path: format!("flight/gps/{name}"),
        };
        VehicleSpec {
            source_id: self.source,
            source: "flight".into(),
            label: "vehicle".into(),
            show: true,
            show_path: true,
            position: VehiclePosition::Gps {
                lat: field(0, "lat"),
                lon: field(1, "lon"),
                alt: field(2, "alt"),
                lat_lon_dege7: false,
                alt_mm: false,
                alt_offset_m: 0.0,
            },
            orientation: VehicleOrientation::Static,
            model: VehicleModel::None,
            color: [1.0; 4],
            path_color: [1.0; 4],
            scale: 1.0,
            owner: vehicle_owner,
        }
    }

    fn add_vehicle(&mut self, vehicle_owner: Option<ResourceOwner>) -> u64 {
        let spec = self.vehicle_spec(vehicle_owner);
        self.apply_trusted(ControlRequest::Vehicles(Box::new(VehicleRequest::Add(
            spec,
        ))))
        .into_vehicles()
        .unwrap()[0]
            .id
    }
}

#[test]
fn stale_trace_slot_cannot_remove_the_trace_shifted_into_it_by_the_ui() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    fixture.add_trace(Some(owner("flight-diagnosis", 7)));
    fixture.apply_trusted(ControlRequest::Traces(TraceRequest::Add {
        window: 0,
        tile: root,
        field_id: fixture.fields[1],
        field: "gps.lon".into(),
        color: None,
        width_px: None,
        mode: TraceMode::Line,
        owner: Some(owner("flight-diagnosis", 7)),
    }));
    let plot_id = fixture.workspace.plot_infos(0)[0].instance_id;
    let trace_id = fixture.workspace.plot_panes().next().unwrap().traces[0].instance_id;
    fixture
        .workspace
        .plot_pane_mut(egui_tiles::TileId(root))
        .unwrap()
        .traces
        .remove(0);
    let error = fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Guarded {
                guard: ResourceGuard::Trace {
                    window: 0,
                    tile: root,
                    plot_instance_id: plot_id,
                    index: 0,
                    trace_instance_id: trace_id,
                },
                request: Box::new(ControlRequest::Traces(TraceRequest::Remove {
                    window: 0,
                    tile: root,
                    index: Some(0),
                    field_id: None,
                    field: None,
                })),
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::StaleHandle);
    assert_eq!(
        fixture.workspace.plot_panes().next().unwrap().traces[0].field,
        fixture.fields[1]
    );
}

#[test]
fn safe_policy_reports_foreign_owner_before_stale_trace_identity() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    fixture.add_trace(Some(owner("flight-diagnosis", 7)));
    fixture.apply_trusted(ControlRequest::Traces(TraceRequest::Add {
        window: 0,
        tile: root,
        field_id: fixture.fields[1],
        field: "gps.lon".into(),
        color: None,
        width_px: None,
        mode: TraceMode::Line,
        owner: None,
    }));
    let plot_id = fixture.workspace.plot_infos(0)[0].instance_id;
    let trace_id = fixture.workspace.plot_panes().next().unwrap().traces[0].instance_id;
    fixture
        .workspace
        .plot_pane_mut(egui_tiles::TileId(root))
        .unwrap()
        .traces
        .remove(0);
    let error = fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Guarded {
                guard: ResourceGuard::Trace {
                    window: 0,
                    tile: root,
                    plot_instance_id: plot_id,
                    index: 0,
                    trace_instance_id: trace_id,
                },
                request: Box::new(ControlRequest::Traces(TraceRequest::Remove {
                    window: 0,
                    tile: root,
                    index: Some(0),
                    field_id: None,
                    field: None,
                })),
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Forbidden);
    assert_eq!(
        fixture.workspace.plot_panes().next().unwrap().traces[0].field,
        fixture.fields[1]
    );
}

#[test]
fn safe_reads_are_allowed_but_global_mutations_require_full_control() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    let reads = [
        ControlRequest::Markers(MarkerRequest::List),
        ControlRequest::Plots(PlotRequest::List { window: None }),
        ControlRequest::Traces(TraceRequest::List {
            window: 0,
            tile: root,
        }),
        ControlRequest::Annotations(AnnotationRequest::List { target: None }),
        ControlRequest::Vehicles(Box::new(VehicleRequest::List)),
        ControlRequest::VehicleProfiles(VehicleProfileRequest::List),
        ControlRequest::Layouts(LayoutRequest::Current),
    ];
    for request in reads {
        assert!(fixture.authorize(AccessMode::Safe, request).is_ok());
    }

    let globals = [
        ControlRequest::Playback(PlaybackRequest::Set {
            speed: Some(2.0),
            follow_live: None,
        }),
        ControlRequest::Workspace(WorkspaceRequest::ShowScene { visible: true }),
        ControlRequest::Workspace(WorkspaceRequest::Equalize { window: None }),
        ControlRequest::Layouts(LayoutRequest::Clear),
        ControlRequest::VehicleProfiles(VehicleProfileRequest::Delete {
            name: "profile".into(),
        }),
    ];
    for request in globals {
        assert_eq!(
            fixture
                .authorize(AccessMode::Safe, request.clone())
                .unwrap_err()
                .kind(),
            ErrorKind::Forbidden
        );
        assert!(fixture.authorize(AccessMode::Full, request).is_ok());
    }
}

#[test]
fn safe_creations_replace_every_caller_supplied_owner() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    let field_id = fixture.fields[0];
    let forged = Some(owner("forged", 99));

    let trace = fixture
        .authorize(
            AccessMode::Safe,
            ControlRequest::Traces(TraceRequest::Add {
                window: 0,
                tile: root,
                field_id,
                field: "gps.lat".into(),
                color: None,
                width_px: None,
                mode: TraceMode::Line,
                owner: forged.clone(),
            }),
        )
        .unwrap();
    let ControlRequest::Traces(TraceRequest::Add { owner, .. }) = trace else {
        panic!("wrong request")
    };
    assert_eq!(owner, Some(principal(AccessMode::Safe).owner));

    let annotation = fixture
        .authorize(
            AccessMode::Safe,
            ControlRequest::Annotations(AnnotationRequest::Add {
                window: 0,
                tile: root,
                geometry: AnnotationGeometry::Text { at: (1, 1.0) },
                label: "event".into(),
                style: AnnotationStylePatch::default(),
                owner: forged.clone(),
            }),
        )
        .unwrap();
    let ControlRequest::Annotations(AnnotationRequest::Add { owner, .. }) = annotation else {
        panic!("wrong request")
    };
    assert_eq!(owner, Some(principal(AccessMode::Safe).owner));

    let spec = fixture.vehicle_spec(forged.clone());
    let vehicle = fixture
        .authorize(
            AccessMode::Safe,
            ControlRequest::Vehicles(Box::new(VehicleRequest::Add(spec))),
        )
        .unwrap();
    let ControlRequest::Vehicles(request) = vehicle else {
        panic!("wrong request")
    };
    let VehicleRequest::Add(spec) = *request else {
        panic!("wrong request")
    };
    assert_eq!(spec.owner, Some(principal(AccessMode::Safe).owner));

    for request in [
        WorkspaceRequest::AddPlot {
            window: None,
            direction: SplitDirection::Horizontal,
            owner: forged.clone(),
        },
        WorkspaceRequest::Split {
            window: 0,
            tile: root,
            direction: SplitDirection::Vertical,
            owner: forged.clone(),
        },
        WorkspaceRequest::OpenWindow {
            title: Some("owned".into()),
            owner: forged,
        },
    ] {
        let stamped = fixture
            .authorize(AccessMode::Safe, ControlRequest::Workspace(request))
            .unwrap();
        let owner = match stamped {
            ControlRequest::Workspace(
                WorkspaceRequest::AddPlot { owner, .. }
                | WorkspaceRequest::Split { owner, .. }
                | WorkspaceRequest::OpenWindow { owner, .. },
            ) => owner,
            _ => panic!("wrong request"),
        };
        assert_eq!(owner, Some(principal(AccessMode::Safe).owner));
    }
    let targeted = fixture
        .authorize(
            AccessMode::Safe,
            ControlRequest::Workspace(WorkspaceRequest::AddPlot {
                window: Some(3),
                direction: SplitDirection::Vertical,
                owner: None,
            }),
        )
        .unwrap();
    assert!(matches!(
        targeted,
        ControlRequest::Workspace(WorkspaceRequest::AddPlot {
            window: Some(3),
            owner: Some(_),
            ..
        })
    ));
}

#[test]
fn safe_trace_mutation_uses_actual_ownership_and_clear_keeps_manual_content() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    let field_id = fixture.fields[0];
    fixture.add_trace(None);
    fixture.add_trace(Some(owner("flight-diagnosis", 1)));
    fixture
        .caches
        .request(field_id, &Arc::new(fixture.snapshot.clone()));
    assert!(fixture.caches.is_pinned(field_id));
    let set = |index| {
        ControlRequest::Traces(TraceRequest::Set {
            window: 0,
            tile: root,
            index,
            field_id,
            color: None,
            width_px: None,
            mode: None,
            visible: Some(false),
        })
    };
    assert_eq!(
        fixture
            .authorize(AccessMode::Safe, set(0))
            .unwrap_err()
            .kind(),
        ErrorKind::Forbidden
    );
    assert!(fixture.authorize(AccessMode::Safe, set(1)).is_ok());

    fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Traces(TraceRequest::Clear {
                window: 0,
                tile: root,
            }),
        )
        .unwrap();
    let traces = fixture
        .apply_trusted(ControlRequest::Traces(TraceRequest::List {
            window: 0,
            tile: root,
        }))
        .into_traces()
        .unwrap();
    assert_eq!(traces.len(), 1);
    assert!(traces[0].owner.is_none());
    assert!(fixture.caches.is_pinned(field_id));
    assert!(fixture.authorize(AccessMode::Full, set(0)).is_ok());
}

#[test]
fn safe_annotation_and_marker_removal_preserve_manual_and_other_owner_items() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    let manual_annotation = fixture.add_annotation(None, "manual");
    let owned_annotation = fixture.add_annotation(Some(owner("flight-diagnosis", 1)), "owned");
    fixture.add_annotation(Some(owner("other", 1)), "other");
    let manual_marker = fixture.markers.add_at(0);
    let owned_marker = fixture.add_owned_marker("flight-diagnosis");
    fixture.add_owned_marker("other");

    for request in [
        ControlRequest::Annotations(AnnotationRequest::Set {
            window: 0,
            tile: root,
            id: manual_annotation,
            label: Some("changed".into()),
            geometry: None,
            style: AnnotationStylePatch::default(),
        }),
        ControlRequest::Markers(MarkerRequest::Set {
            id: manual_marker,
            patch: MarkerPatch {
                label: Some("changed".into()),
                ..MarkerPatch::default()
            },
        }),
    ] {
        assert_eq!(
            fixture
                .authorize(AccessMode::Safe, request)
                .unwrap_err()
                .kind(),
            ErrorKind::Forbidden
        );
    }
    assert!(
        fixture
            .authorize(
                AccessMode::Safe,
                ControlRequest::Annotations(AnnotationRequest::Set {
                    window: 0,
                    tile: root,
                    id: owned_annotation,
                    label: Some("changed".into()),
                    geometry: None,
                    style: AnnotationStylePatch::default()
                })
            )
            .is_ok()
    );
    assert!(
        fixture
            .authorize(
                AccessMode::Safe,
                ControlRequest::Markers(MarkerRequest::Set {
                    id: owned_marker,
                    patch: MarkerPatch::default()
                })
            )
            .is_ok()
    );

    fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: None,
                filter: AnnotationFilter::All,
            }),
        )
        .unwrap();
    fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Markers(MarkerRequest::Remove(MarkerFilter::All)),
        )
        .unwrap();
    let annotations = fixture
        .apply_trusted(ControlRequest::Annotations(AnnotationRequest::List {
            target: None,
        }))
        .into_annotations()
        .unwrap();
    let markers = fixture
        .apply_trusted(ControlRequest::Markers(MarkerRequest::List))
        .into_markers()
        .unwrap();
    assert_eq!(annotations.len(), 2);
    assert!(annotations.iter().any(|item| item.owner.is_none()));
    assert!(
        annotations
            .iter()
            .any(|item| item.owner.as_deref() == Some("other"))
    );
    assert_eq!(markers.len(), 2);
    assert!(markers.iter().any(|item| item.owner.is_none()));
    assert!(
        markers
            .iter()
            .any(|item| item.owner.as_deref() == Some("other"))
    );
}

#[test]
fn safe_vehicle_mutation_uses_runtime_ownership_and_remove_all_is_owner_scoped() {
    let mut fixture = PolicyFixture::new();
    let manual = fixture.add_vehicle(None);
    let owned = fixture.add_vehicle(Some(owner("flight-diagnosis", 1)));
    fixture.add_vehicle(Some(owner("other", 1)));
    let set = |id| {
        ControlRequest::Vehicles(Box::new(VehicleRequest::Set {
            id,
            patch: VehiclePatch {
                show: Some(false),
                ..VehiclePatch::default()
            },
        }))
    };
    assert_eq!(
        fixture
            .authorize(AccessMode::Safe, set(manual))
            .unwrap_err()
            .kind(),
        ErrorKind::Forbidden
    );
    assert!(fixture.authorize(AccessMode::Safe, set(owned)).is_ok());

    fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Vehicles(Box::new(VehicleRequest::Remove(VehicleFilter::All))),
        )
        .unwrap();
    let vehicles = fixture
        .apply_trusted(ControlRequest::Vehicles(Box::new(VehicleRequest::List)))
        .into_vehicles()
        .unwrap();
    assert_eq!(vehicles.len(), 2);
    assert!(vehicles.iter().any(|vehicle| vehicle.spec.owner.is_none()));
    assert!(vehicles.iter().any(|vehicle| {
        vehicle.spec.owner.as_ref().map(|owner| owner.name.as_str()) == Some("other")
    }));
}

#[test]
fn safe_pane_close_checks_plot_ownership_and_generation_owner_is_forced() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    let manual_close = ControlRequest::Workspace(WorkspaceRequest::Close {
        window: 0,
        tile: root,
    });
    assert_eq!(
        fixture
            .authorize(AccessMode::Safe, manual_close.clone())
            .unwrap_err()
            .kind(),
        ErrorKind::Forbidden
    );
    assert!(fixture.authorize(AccessMode::Full, manual_close).is_ok());

    let mut created = fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Workspace(WorkspaceRequest::Split {
                window: 0,
                tile: root,
                direction: SplitDirection::Horizontal,
                owner: Some(owner("forged", 99)),
            }),
        )
        .unwrap()
        .into_plots()
        .unwrap();
    let created = created.remove(0);
    assert_eq!(created.owner, Some(principal(AccessMode::Safe).owner));
    fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Workspace(WorkspaceRequest::Close {
                window: 0,
                tile: created.tile,
            }),
        )
        .unwrap();

    let stamped = fixture
        .authorize(
            AccessMode::Safe,
            ControlRequest::Generation(GenerationRequest::RemoveOwned {
                owner: "forged".into(),
            }),
        )
        .unwrap();
    assert_eq!(
        stamped,
        ControlRequest::Generation(GenerationRequest::RemoveOwned {
            owner: "flight-diagnosis".into()
        })
    );
    for request in [
        GenerationRequest::Commit {
            owner: "flight-diagnosis".into(),
            generation: 7,
        },
        GenerationRequest::Rollback {
            owner: "flight-diagnosis".into(),
            generation: 7,
        },
    ] {
        assert_eq!(
            fixture
                .authorize(AccessMode::Full, ControlRequest::Generation(request))
                .unwrap_err()
                .kind(),
            ErrorKind::Forbidden
        );
    }
}

#[test]
fn trusted_calls_preserve_embedded_owner_and_external_batches_are_authorized() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    let embedded = owner("embedded.py", 3);
    let mut created = control_service::apply_call(
        &mut fixture.control(),
        ControlCall::Trusted(ControlRequest::Workspace(WorkspaceRequest::Split {
            window: 0,
            tile: root,
            direction: SplitDirection::Horizontal,
            owner: Some(embedded.clone()),
        })),
    )
    .unwrap()
    .into_plots()
    .unwrap();
    assert_eq!(created.remove(0).owner, Some(embedded));

    fixture.add_owned_marker("flight-diagnosis");
    fixture.add_owned_marker("other");
    fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Batch(vec![ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: "forged".into(),
            })]),
        )
        .unwrap();
    let markers = fixture.markers.marker_infos();
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].owner.as_deref(), Some("other"));

    let denied = fixture.apply_external(
        AccessMode::Safe,
        ControlRequest::Batch(vec![ControlRequest::Playback(PlaybackRequest::Set {
            speed: Some(1.5),
            follow_live: None,
        })]),
    );
    assert_eq!(denied.unwrap_err().kind(), ErrorKind::Forbidden);
}

#[test]
fn safe_batches_reauthorize_each_request_after_prior_index_shifts() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    let field_id = fixture.fields[0];
    fixture.add_trace(Some(owner("flight-diagnosis", 1)));
    fixture.add_trace(None);

    let result = fixture.apply_external(
        AccessMode::Safe,
        ControlRequest::Batch(vec![
            ControlRequest::Traces(TraceRequest::Remove {
                window: 0,
                tile: root,
                index: Some(0),
                field_id: None,
                field: None,
            }),
            ControlRequest::Traces(TraceRequest::Set {
                window: 0,
                tile: root,
                index: 0,
                field_id,
                color: None,
                width_px: None,
                mode: None,
                visible: Some(false),
            }),
        ]),
    );

    assert_eq!(result.unwrap_err().kind(), ErrorKind::Forbidden);
    let traces = fixture
        .apply_trusted(ControlRequest::Traces(TraceRequest::List {
            window: 0,
            tile: root,
        }))
        .into_traces()
        .unwrap();
    assert_eq!(traces.len(), 2, "a rejected batch must remain atomic");
    assert!(traces[1].visible, "the manual trace must not be changed");
}

#[test]
fn external_batch_stamps_owner_before_validating_it() {
    let mut fixture = PolicyFixture::new();
    fixture.add_owned_marker("flight-diagnosis");
    fixture.add_owned_marker("other");

    fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Batch(vec![ControlRequest::Markers(MarkerRequest::RemoveOwned {
                owner: String::new(),
            })]),
        )
        .unwrap();

    let markers = fixture.markers.marker_infos();
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].owner.as_deref(), Some("other"));
}

#[test]
fn safe_external_trace_calls_change_owned_traces_without_touching_manual_traces() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    let field_id = fixture.fields[0];
    fixture.add_trace(None);
    fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Traces(TraceRequest::Add {
                window: 0,
                tile: root,
                field_id,
                field: "gps.lat".into(),
                color: None,
                width_px: None,
                mode: TraceMode::Line,
                owner: Some(owner("forged", 99)),
            }),
        )
        .unwrap();

    let manual_removal = ControlRequest::Traces(TraceRequest::Remove {
        window: 0,
        tile: root,
        index: Some(0),
        field_id: None,
        field: None,
    });
    assert_eq!(
        fixture
            .apply_external(AccessMode::Safe, manual_removal.clone())
            .unwrap_err()
            .kind(),
        ErrorKind::Forbidden
    );
    fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Traces(TraceRequest::Set {
                window: 0,
                tile: root,
                index: 1,
                field_id,
                color: None,
                width_px: None,
                mode: None,
                visible: Some(false),
            }),
        )
        .unwrap();
    let traces = fixture
        .apply_trusted(ControlRequest::Traces(TraceRequest::List {
            window: 0,
            tile: root,
        }))
        .into_traces()
        .unwrap();
    assert_eq!(traces.len(), 2);
    assert!(traces[0].owner.is_none());
    assert!(traces[0].visible);
    assert_eq!(traces[1].owner, Some(owner("flight-diagnosis", 7)));
    assert!(!traces[1].visible);

    fixture
        .apply_external(AccessMode::Full, manual_removal)
        .unwrap();
    assert_eq!(
        fixture.workspace.plot_panes().next().unwrap().traces.len(),
        1
    );
}

#[test]
fn safe_remove_owned_closes_only_the_authenticated_clients_windows() {
    let mut fixture = PolicyFixture::new();
    let manual_window = fixture
        .apply_trusted(ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
            title: Some("manual".into()),
            owner: None,
        }))
        .into_window_info()
        .unwrap();
    let owned_window = fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                title: Some("owned".into()),
                owner: Some(owner("forged", 99)),
            }),
        )
        .unwrap()
        .into_window_info()
        .unwrap();
    assert_eq!(owned_window.owner, Some(owner("flight-diagnosis", 7)));

    fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Generation(GenerationRequest::RemoveOwned {
                owner: "forged".into(),
            }),
        )
        .unwrap();
    assert_eq!(fixture.windows.len(), 1);
    assert_eq!(fixture.windows[0].id.0, manual_window.id);
}

#[test]
fn safe_targeted_removals_reject_other_owner_resources_without_mutation() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    fixture.add_trace(Some(owner("other", 1)));
    let annotation = fixture.add_annotation(Some(owner("other", 1)), "other");
    let marker = fixture.add_owned_marker("other");
    let vehicle = fixture.add_vehicle(Some(owner("other", 1)));

    for request in [
        ControlRequest::Traces(TraceRequest::Remove {
            window: 0,
            tile: root,
            index: Some(0),
            field_id: None,
            field: None,
        }),
        ControlRequest::Annotations(AnnotationRequest::Remove {
            target: Some((0, root)),
            filter: AnnotationFilter::Id(annotation),
        }),
        ControlRequest::Markers(MarkerRequest::Remove(MarkerFilter::Id(marker))),
        ControlRequest::Vehicles(Box::new(VehicleRequest::Remove(VehicleFilter::Id(vehicle)))),
    ] {
        assert_eq!(
            fixture
                .apply_external(AccessMode::Safe, request)
                .unwrap_err()
                .kind(),
            ErrorKind::Forbidden
        );
    }

    assert_eq!(
        fixture.workspace.plot_panes().next().unwrap().traces.len(),
        1
    );
    assert_eq!(
        fixture
            .workspace
            .plot_panes()
            .next()
            .unwrap()
            .annotations
            .items()
            .len(),
        1
    );
    assert_eq!(fixture.markers.marker_infos().len(), 1);
    assert_eq!(fixture.vehicles.len(), 1);
}

#[test]
fn safe_close_rejects_owned_plot_with_manual_or_other_owner_trace() {
    for content_owner in [None, Some(owner("other", 1))] {
        let mut fixture = PolicyFixture::new();
        let mut created = fixture
            .apply_external(
                AccessMode::Safe,
                ControlRequest::Workspace(WorkspaceRequest::Split {
                    window: 0,
                    tile: fixture.root(),
                    direction: SplitDirection::Horizontal,
                    owner: None,
                }),
            )
            .unwrap()
            .into_plots()
            .unwrap();
        let tile = created.remove(0).tile;
        fixture.apply_trusted(ControlRequest::Traces(TraceRequest::Add {
            window: 0,
            tile,
            field_id: fixture.fields[0],
            field: "gps.lat".into(),
            color: None,
            width_px: None,
            mode: TraceMode::Line,
            owner: content_owner,
        }));

        let error = fixture
            .apply_external(
                AccessMode::Safe,
                ControlRequest::Workspace(WorkspaceRequest::Close { window: 0, tile }),
            )
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Forbidden);
        assert_eq!(
            fixture
                .workspace
                .plot_pane_mut(egui_tiles::TileId(tile))
                .unwrap()
                .traces
                .len(),
            1
        );
        fixture
            .apply_external(
                AccessMode::Full,
                ControlRequest::Workspace(WorkspaceRequest::Close { window: 0, tile }),
            )
            .unwrap();
        assert!(
            fixture
                .workspace
                .plot_pane_mut(egui_tiles::TileId(tile))
                .is_none()
        );
    }
}

#[test]
fn safe_remove_owned_rejects_plot_with_other_owner_content() {
    let mut fixture = PolicyFixture::new();
    let mut created = fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Workspace(WorkspaceRequest::Split {
                window: 0,
                tile: fixture.root(),
                direction: SplitDirection::Horizontal,
                owner: None,
            }),
        )
        .unwrap()
        .into_plots()
        .unwrap();
    let tile = created.remove(0).tile;
    fixture.apply_trusted(ControlRequest::Traces(TraceRequest::Add {
        window: 0,
        tile,
        field_id: fixture.fields[0],
        field: "gps.lat".into(),
        color: None,
        width_px: None,
        mode: TraceMode::Line,
        owner: Some(owner("other", 1)),
    }));

    let error = fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Generation(GenerationRequest::RemoveOwned {
                owner: "forged".into(),
            }),
        )
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Forbidden);
    assert_eq!(
        fixture
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap()
            .traces
            .len(),
        1
    );
    fixture
        .apply_external(
            AccessMode::Full,
            ControlRequest::Generation(GenerationRequest::RemoveOwned {
                owner: "forged".into(),
            }),
        )
        .unwrap();
    assert!(
        fixture
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .is_none()
    );
}

#[test]
fn safe_remove_owned_rejects_window_containing_another_owners_plot() {
    let mut fixture = PolicyFixture::new();
    let window = fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                title: None,
                owner: None,
            }),
        )
        .unwrap()
        .into_window_info()
        .unwrap();
    let root = fixture.windows[0].workspace.tree.root().unwrap().0;
    fixture.apply_trusted(ControlRequest::Workspace(WorkspaceRequest::Split {
        window: window.id,
        tile: root,
        direction: SplitDirection::Horizontal,
        owner: Some(owner("other", 1)),
    }));

    let error = fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Generation(GenerationRequest::RemoveOwned {
                owner: "forged".into(),
            }),
        )
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Forbidden);
    assert_eq!(fixture.windows.len(), 1);
    assert_eq!(fixture.windows[0].workspace.plot_panes().count(), 2);
}

#[test]
fn shifted_trace_index_with_a_different_field_is_forbidden_if_foreign() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    fixture.add_trace(Some(owner("flight-diagnosis", 1)));
    fixture.apply_trusted(ControlRequest::Traces(TraceRequest::Add {
        window: 0,
        tile: root,
        field_id: fixture.fields[1],
        field: "gps.lon".into(),
        color: None,
        width_px: None,
        mode: TraceMode::Line,
        owner: None,
    }));
    fixture.apply_trusted(ControlRequest::Traces(TraceRequest::Remove {
        window: 0,
        tile: root,
        index: Some(0),
        field_id: None,
        field: None,
    }));

    let error = fixture
        .apply_external(
            AccessMode::Safe,
            ControlRequest::Traces(TraceRequest::Set {
                window: 0,
                tile: root,
                index: 0,
                field_id: fixture.fields[0],
                color: None,
                width_px: None,
                mode: None,
                visible: Some(false),
            }),
        )
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Forbidden);
    assert!(fixture.workspace.plot_panes().next().unwrap().traces[0].visible);
}

#[test]
fn external_client_named_like_a_script_owner_cannot_modify_that_scripts_resources() {
    let mut fixture = PolicyFixture::new();
    let root = fixture.root();
    let marker = fixture.add_owned_marker("flight-diagnosis");
    fixture.add_trace(Some(owner("flight-diagnosis", 1)));
    let external = ControlPrincipal {
        owner: owner(&delog_remote::external_owner_name("flight-diagnosis"), 1),
        access: AccessMode::Safe,
    };

    for request in [
        ControlRequest::Markers(MarkerRequest::Remove(MarkerFilter::Id(marker))),
        ControlRequest::Traces(TraceRequest::Remove {
            window: 0,
            tile: root,
            index: Some(0),
            field_id: None,
            field: None,
        }),
    ] {
        let error = control_service::apply_call(
            &mut fixture.control(),
            ControlCall::External {
                principal: external.clone(),
                request,
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Forbidden);
    }

    assert_eq!(fixture.markers.marker_infos().len(), 1);
    assert_eq!(
        fixture.workspace.plot_panes().next().unwrap().traces.len(),
        1
    );
}

fn external(name: &str, access: AccessMode) -> ControlPrincipal {
    ControlPrincipal {
        owner: owner(&delog_remote::external_owner_name(name), 1),
        access,
    }
}

fn apply_as(
    fixture: &mut PolicyFixture,
    principal: &ControlPrincipal,
    request: ControlRequest,
) -> Result<ControlResponse> {
    control_service::apply_call(
        &mut fixture.control(),
        ControlCall::External {
            principal: principal.clone(),
            request,
        },
    )
}

fn remove_owned_request() -> ControlRequest {
    ControlRequest::Generation(GenerationRequest::RemoveOwned {
        owner: "forged".into(),
    })
}

#[test]
fn remove_owned_reports_removed_ui_resources_and_ignores_matching_labels() {
    let mut fixture = PolicyFixture::new();
    let client = external("flight-diagnosis", AccessMode::Safe);
    fixture.apply_trusted(ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
        title: Some(delog_remote::external_owner_name("flight-diagnosis")),
        owner: None,
    }));
    apply_as(
        &mut fixture,
        &client,
        ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
            title: Some("owned".into()),
            owner: None,
        }),
    )
    .unwrap();
    let root = fixture.root();
    apply_as(
        &mut fixture,
        &client,
        ControlRequest::Workspace(WorkspaceRequest::Split {
            window: 0,
            tile: root,
            direction: SplitDirection::Horizontal,
            owner: None,
        }),
    )
    .unwrap();
    fixture.add_owned_marker(&delog_remote::external_owner_name("flight-diagnosis"));
    fixture.add_owned_marker("flight-diagnosis");

    let removed = apply_as(&mut fixture, &client, remove_owned_request())
        .unwrap()
        .into_removed()
        .unwrap();

    assert_eq!(removed, 4);
    assert_eq!(fixture.windows.len(), 1);
    assert_eq!(fixture.windows[0].owner, None);
    assert_eq!(fixture.workspace.plot_panes().count(), 1);
    assert_eq!(fixture.markers.marker_infos().len(), 1);
    assert_eq!(
        apply_as(&mut fixture, &client, remove_owned_request())
            .unwrap()
            .into_removed()
            .unwrap(),
        0
    );
}

#[test]
fn a_reconnecting_client_reclaims_ui_restored_under_its_persisted_owner_name() {
    let mut fixture = PolicyFixture::new();
    let persisted = owner(&delog_remote::external_owner_name("flight-diagnosis"), 1);
    let tile = fixture
        .apply_trusted(ControlRequest::Workspace(WorkspaceRequest::Split {
            window: 0,
            tile: fixture.root(),
            direction: SplitDirection::Vertical,
            owner: Some(persisted.clone()),
        }))
        .into_plots()
        .unwrap()[0]
        .tile;
    let field_id = fixture.fields[0];
    fixture.apply_trusted(ControlRequest::Traces(TraceRequest::Add {
        window: 0,
        tile,
        field_id,
        field: "gps.lat".into(),
        color: None,
        width_px: None,
        mode: TraceMode::Line,
        owner: Some(persisted),
    }));

    let stranger = external("other-analysis", AccessMode::Safe);
    assert_eq!(
        apply_as(&mut fixture, &stranger, remove_owned_request())
            .unwrap()
            .into_removed()
            .unwrap(),
        0
    );
    assert!(
        fixture
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .is_some()
    );

    let returning = external("flight-diagnosis", AccessMode::Safe);
    let removed = apply_as(&mut fixture, &returning, remove_owned_request())
        .unwrap()
        .into_removed()
        .unwrap();

    assert_eq!(removed, 1);
    assert!(
        fixture
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .is_none()
    );
    assert!(
        fixture
            .workspace
            .plot_panes()
            .all(|pane| pane.traces.is_empty())
    );
}

#[test]
fn full_access_may_remove_manual_state_that_safe_access_protects() {
    let mut fixture = PolicyFixture::new();
    fixture.add_trace(None);
    let root = fixture.root();
    let remove = || {
        ControlRequest::Traces(TraceRequest::Remove {
            window: 0,
            tile: root,
            index: Some(0),
            field_id: None,
            field: None,
        })
    };

    let error = apply_as(
        &mut fixture,
        &external("flight-diagnosis", AccessMode::Safe),
        remove(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Forbidden);
    assert_eq!(
        fixture.workspace.plot_panes().next().unwrap().traces.len(),
        1
    );

    apply_as(
        &mut fixture,
        &external("flight-diagnosis", AccessMode::Full),
        remove(),
    )
    .unwrap();
    assert!(
        fixture
            .workspace
            .plot_panes()
            .next()
            .unwrap()
            .traces
            .is_empty()
    );
}
