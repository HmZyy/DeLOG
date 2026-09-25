use delog_api::ErrorKind;
use delog_api::control::{
    AnnotationGeometry, AnnotationKind, AnnotationRequest, AnnotationStylePatch, ControlRequest,
    ControlResponse, GenerationRequest, LayoutRequest, MarkerFilter, MarkerOrigin, MarkerPatch,
    MarkerRequest, PlaybackRequest, PlotInfo, ResolvedVehicleField, ScriptOwner, SplitDirection,
    TraceMode, TraceRequest, VehicleModel, VehicleNedReference, VehiclePatch, VehiclePosition,
    VehicleProfileRequest, VehicleRequest, VehicleSpec, WorkspaceRequest, request_is_batchable,
    validate_layout_name, validate_layout_path, validate_profile_name,
};
use delog_api::markers::PendingMarker;
use delog_core::identity::{FieldId, SourceId};

#[test]
fn mutations_and_response_returning_requests_keep_the_batch_policy() {
    assert!(request_is_batchable(&ControlRequest::Markers(
        MarkerRequest::Remove(MarkerFilter::ScriptAll),
    )));
    assert!(request_is_batchable(&ControlRequest::Playback(
        PlaybackRequest::Set {
            speed: Some(2.0),
            follow_live: None,
        },
    )));
    assert!(!request_is_batchable(&ControlRequest::Layouts(
        LayoutRequest::Current,
    )));
    assert!(!request_is_batchable(&ControlRequest::Batch(Vec::new())));
}

#[test]
fn response_extractors_keep_protocol_errors_stable() {
    let window = delog_api::control::WindowInfo {
        id: 9,
        title: "Analysis".into(),
        owner: Some(ScriptOwner {
            name: "external".into(),
            generation: 7,
        }),
    };
    assert_eq!(
        ControlResponse::Window(window.clone())
            .into_window()
            .unwrap(),
        9
    );
    assert_eq!(
        ControlResponse::Window(window.clone())
            .into_window_info()
            .unwrap(),
        window
    );
    assert_eq!(
        ControlResponse::Unit.into_window_info().unwrap_err().kind(),
        ErrorKind::Protocol
    );
    let plots = vec![PlotInfo {
        owner: None,
        window: 0,
        tile: 7,
        instance_id: 1,
        index: 0,
        label: "Plot 1".into(),
    }];
    assert_eq!(
        ControlResponse::Plots(plots.clone()).into_plots().unwrap(),
        plots
    );
    assert_eq!(
        ControlResponse::Unit.into_plots().unwrap_err().to_string(),
        "the DeLOG window answered with the wrong kind of result"
    );
    let error = ControlResponse::Unit.into_vehicle_profile().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Protocol);
    assert_eq!(error.to_string(), "vehicle profile request returned Unit");
}

#[test]
fn parsers_preserve_user_facing_messages() {
    assert_eq!(
        TraceMode::parse("curve").unwrap_err().to_string(),
        "trace mode must be 'line', 'scatter', or 'step', got \"curve\""
    );
    assert_eq!(
        SplitDirection::parse("diagonal").unwrap_err().to_string(),
        "split direction must be 'horizontal' or 'vertical', got \"diagonal\""
    );
}

#[test]
fn playback_rejects_non_finite_speed() {
    assert_eq!(
        PlaybackRequest::set(Some(f64::NAN), None)
            .unwrap_err()
            .to_string(),
        "playback speed must be finite, got NaN"
    );
}

#[test]
fn trace_removal_requires_exactly_one_selector() {
    assert_eq!(
        TraceRequest::remove(0, 1, None, None)
            .unwrap_err()
            .to_string(),
        "remove() needs exactly one of a position or field="
    );
}

#[test]
fn layout_paths_keep_the_portable_contract() {
    assert_eq!(validate_layout_name(" flight-1 ").unwrap(), "flight-1");
    assert!(validate_layout_name("../flight").is_err());
    assert_eq!(
        validate_layout_path(" layout.json ").unwrap(),
        " layout.json "
    );
    assert!(validate_layout_path("  ").is_err());
}

#[test]
fn geometry_and_style_reject_non_finite_values() {
    assert_eq!(
        AnnotationGeometry::hline(f64::NAN).unwrap_err().to_string(),
        "y must be finite"
    );
    assert_eq!(
        AnnotationStylePatch::new(None, Some(f32::INFINITY), None, None, None)
            .unwrap_err()
            .to_string(),
        "stroke_px must be finite"
    );
}

#[test]
fn marker_time_ranges_preserve_order_validation() {
    assert_eq!(
        MarkerFilter::time_range(Some(20), Some(10))
            .unwrap_err()
            .to_string(),
        "marker time range requires after <= before"
    );
}

#[test]
fn annotation_constructors_and_movement_preserve_geometry() {
    assert_eq!(
        AnnotationKind::parse("polygon").unwrap_err().to_string(),
        "annotation kind must be 'text', 'segment', 'rect', 'ellipse', or 'hline', got \"polygon\""
    );
    assert_eq!(
        AnnotationGeometry::text((1, f64::INFINITY))
            .unwrap_err()
            .to_string(),
        "at.y must be finite"
    );
    let moved = AnnotationGeometry::rect((10, 1.0), (20, 3.0))
        .unwrap()
        .moved_to((30, 4.0))
        .unwrap();
    assert_eq!(
        moved,
        AnnotationGeometry::Rect {
            a: (30, 4.0),
            b: (40, 6.0),
        }
    );
}

#[test]
fn marker_origin_and_patch_validation_preserve_messages() {
    assert_eq!(MarkerOrigin::parse("script").unwrap().as_str(), "script");
    assert_eq!(
        MarkerOrigin::parse("imported").unwrap_err().to_string(),
        "marker origin must be 'manual' or 'script', got \"imported\""
    );
    assert_eq!(
        MarkerPatch {
            label: Some(String::new()),
            ..MarkerPatch::default()
        }
        .validate()
        .unwrap_err()
        .to_string(),
        "marker label must not be empty"
    );
    assert_eq!(
        MarkerPatch {
            color: Some([1.0, f32::NAN, 0.0, 1.0]),
            ..MarkerPatch::default()
        }
        .validate()
        .unwrap_err()
        .to_string(),
        "marker color components must be finite and between 0 and 1"
    );
}

#[test]
fn manual_georeferences_validate_latitude_longitude_and_finiteness() {
    assert_eq!(
        VehicleNedReference::manual(91.0, 0.0, 0.0)
            .unwrap_err()
            .to_string(),
        "lat_deg must be between -90 and 90"
    );
    assert_eq!(
        VehicleNedReference::manual(0.0, f64::NAN, 0.0)
            .unwrap_err()
            .to_string(),
        "lon_deg must be finite"
    );
}

#[test]
fn model_parser_preserves_supported_names() {
    assert_eq!(
        VehicleModel::parse("fixedwing").unwrap(),
        VehicleModel::FixedWing
    );
    assert!(VehicleModel::parse("rocket").is_err());
}

#[test]
fn vehicle_position_and_patch_validation_preserve_messages() {
    let field = |id| ResolvedVehicleField {
        id: FieldId(id),
        path: format!("flight/GPS/{id}"),
    };
    assert_eq!(
        VehiclePosition::gps(field(0), field(1), field(2), false, false, f64::INFINITY)
            .unwrap_err()
            .to_string(),
        "alt_offset_m must be finite"
    );
    assert_eq!(
        VehiclePatch {
            scale: Some(0.0),
            ..VehiclePatch::default()
        }
        .validate()
        .unwrap_err()
        .to_string(),
        "vehicle scale must be finite and > 0"
    );
}

#[test]
fn vehicle_profile_names_are_normalized_and_portable() {
    assert_eq!(validate_profile_name(" survey ").unwrap(), "survey");
    assert_eq!(
        validate_profile_name("../survey").unwrap_err().to_string(),
        "vehicle profile name must not be empty or contain path separators/traversal"
    );
}

fn assert_invalid(request: ControlRequest) {
    assert_eq!(
        request.validate().unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn raw_requests_cannot_bypass_validation() {
    let invalid = [
        ControlRequest::Playback(PlaybackRequest::Set {
            speed: Some(f64::NAN),
            follow_live: None,
        }),
        ControlRequest::Traces(TraceRequest::Add {
            window: 0,
            tile: 1,
            field_id: FieldId(0),
            field: String::new(),
            color: Some([2.0, 0.0, 0.0, 1.0]),
            width_px: Some(0.0),
            mode: TraceMode::Line,
            owner: None,
        }),
        ControlRequest::Batch(vec![ControlRequest::Batch(Vec::new())]),
    ];
    for request in invalid {
        assert_invalid(request);
    }
}

#[test]
fn raw_marker_and_generation_requests_validate_owners_labels_ranges_and_colors() {
    let marker = PendingMarker {
        time_us: 0,
        label: String::new(),
        color: None,
        note: String::new(),
    };
    assert_invalid(ControlRequest::Markers(MarkerRequest::Append {
        owner: "script".into(),
        generation: 0,
        markers: vec![marker],
    }));
    let marker = PendingMarker {
        time_us: 0,
        label: "event".into(),
        color: Some([0.0, 0.0, 0.0, 2.0]),
        note: String::new(),
    };
    assert_invalid(ControlRequest::Markers(MarkerRequest::Append {
        owner: "script".into(),
        generation: 0,
        markers: vec![marker],
    }));
    assert_invalid(ControlRequest::Markers(MarkerRequest::Append {
        owner: String::new(),
        generation: 0,
        markers: vec![],
    }));
    assert_invalid(ControlRequest::Markers(MarkerRequest::RemoveOwned {
        owner: String::new(),
    }));
    assert_invalid(ControlRequest::Markers(MarkerRequest::Remove(
        MarkerFilter::Owner(String::new()),
    )));
    assert_invalid(ControlRequest::Markers(MarkerRequest::Remove(
        MarkerFilter::ScriptLabel(String::new()),
    )));
    assert_invalid(ControlRequest::Markers(MarkerRequest::Remove(
        MarkerFilter::ScriptTimeRange {
            after: Some(2),
            before: Some(1),
        },
    )));
    assert_invalid(ControlRequest::Markers(MarkerRequest::Set {
        id: 0,
        patch: MarkerPatch {
            color: Some([f32::NAN, 0.0, 0.0, 1.0]),
            ..Default::default()
        },
    }));
    assert_invalid(ControlRequest::Generation(GenerationRequest::Commit {
        owner: String::new(),
        generation: 0,
    }));
    assert_invalid(ControlRequest::Generation(GenerationRequest::Rollback {
        owner: String::new(),
        generation: 0,
    }));
}

#[test]
fn raw_trace_requests_validate_style_selectors_and_field_identity() {
    let add = |color, width_px| {
        ControlRequest::Traces(TraceRequest::Add {
            window: 0,
            tile: 1,
            field_id: FieldId(0),
            field: "field".into(),
            color,
            width_px,
            mode: TraceMode::Line,
            owner: None,
        })
    };
    assert_invalid(add(Some([f32::INFINITY, 0.0, 0.0, 1.0]), None));
    assert_invalid(add(None, Some(0.0)));
    assert_invalid(ControlRequest::Traces(TraceRequest::Remove {
        window: 0,
        tile: 1,
        index: None,
        field_id: None,
        field: None,
    }));
    assert_invalid(ControlRequest::Traces(TraceRequest::Remove {
        window: 0,
        tile: 1,
        index: Some(0),
        field_id: Some(FieldId(1)),
        field: Some("field".into()),
    }));
    assert_invalid(ControlRequest::Traces(TraceRequest::Remove {
        window: 0,
        tile: 1,
        index: None,
        field_id: Some(FieldId(1)),
        field: None,
    }));
    assert_invalid(ControlRequest::Traces(TraceRequest::Remove {
        window: 0,
        tile: 1,
        index: None,
        field_id: Some(FieldId(1)),
        field: Some(String::new()),
    }));
    assert_invalid(ControlRequest::Traces(TraceRequest::Set {
        window: 0,
        tile: 1,
        index: 0,
        field_id: FieldId(0),
        color: None,
        width_px: Some(f32::NAN),
        mode: None,
        visible: None,
    }));
    assert_invalid(ControlRequest::Traces(TraceRequest::Add {
        window: 0,
        tile: 1,
        field_id: FieldId(0),
        field: "field".into(),
        color: None,
        width_px: None,
        mode: TraceMode::Line,
        owner: Some(ScriptOwner {
            name: String::new(),
            generation: 0,
        }),
    }));
}

#[test]
fn raw_annotation_requests_validate_geometry_style_and_owner() {
    let geometry = AnnotationGeometry::HLine { y: f64::NAN };
    assert_invalid(ControlRequest::Annotations(AnnotationRequest::Add {
        window: 0,
        tile: 1,
        geometry,
        label: String::new(),
        style: Default::default(),
        owner: None,
    }));
    assert_invalid(ControlRequest::Annotations(AnnotationRequest::Set {
        window: 0,
        tile: 1,
        id: 0,
        label: None,
        geometry: None,
        style: AnnotationStylePatch {
            color: Some([2.0, 0.0, 0.0, 1.0]),
            ..Default::default()
        },
    }));
    assert_invalid(ControlRequest::Annotations(AnnotationRequest::Set {
        window: 0,
        tile: 1,
        id: 0,
        label: None,
        geometry: Some(AnnotationGeometry::Segment {
            from: (0, 0.0),
            to: (1, f64::INFINITY),
        }),
        style: Default::default(),
    }));
    for style in [
        AnnotationStylePatch {
            stroke_px: Some(0.0),
            ..Default::default()
        },
        AnnotationStylePatch {
            fill_opacity: Some(2.0),
            ..Default::default()
        },
        AnnotationStylePatch {
            font_px: Some(0.0),
            ..Default::default()
        },
    ] {
        assert_invalid(ControlRequest::Annotations(AnnotationRequest::Set {
            window: 0,
            tile: 1,
            id: 0,
            label: None,
            geometry: None,
            style,
        }));
    }
    assert_invalid(ControlRequest::Annotations(AnnotationRequest::Add {
        window: 0,
        tile: 1,
        geometry: AnnotationGeometry::HLine { y: 0.0 },
        label: String::new(),
        style: Default::default(),
        owner: Some(ScriptOwner {
            name: String::new(),
            generation: 0,
        }),
    }));
    assert!(
        ControlRequest::Annotations(AnnotationRequest::Add {
            window: 0,
            tile: 1,
            geometry: AnnotationGeometry::HLine { y: 0.0 },
            label: String::new(),
            style: Default::default(),
            owner: None
        })
        .validate()
        .is_ok()
    );
}

#[test]
fn raw_vehicle_requests_validate_specs_patches_and_profile_inputs() {
    let field = ResolvedVehicleField {
        id: FieldId(0),
        path: "position".into(),
    };
    let spec = VehicleSpec {
        source_id: SourceId(0),
        source: "source".into(),
        label: "Vehicle".into(),
        show: true,
        show_path: true,
        position: VehiclePosition::Gps {
            lat: field.clone(),
            lon: field.clone(),
            alt: field.clone(),
            lat_lon_dege7: false,
            alt_mm: false,
            alt_offset_m: f64::NAN,
        },
        orientation: delog_api::control::VehicleOrientation::Static,
        model: VehicleModel::Quad,
        color: [1.0; 4],
        path_color: [1.0; 4],
        scale: 1.0,
        owner: None,
    };
    assert_invalid(ControlRequest::Vehicles(Box::new(VehicleRequest::Add(
        spec.clone(),
    ))));
    let mut spec = spec;
    spec.position = VehiclePosition::Gps {
        lat: field.clone(),
        lon: field.clone(),
        alt: field,
        lat_lon_dege7: false,
        alt_mm: false,
        alt_offset_m: 0.0,
    };
    spec.label.clear();
    assert_invalid(ControlRequest::Vehicles(Box::new(VehicleRequest::Add(
        spec,
    ))));
    assert_invalid(ControlRequest::Vehicles(Box::new(VehicleRequest::Set {
        id: 0,
        patch: VehiclePatch {
            scale: Some(0.0),
            ..Default::default()
        },
    })));
    assert_invalid(ControlRequest::Vehicles(Box::new(VehicleRequest::Set {
        id: 0,
        patch: VehiclePatch {
            label: Some(String::new()),
            ..Default::default()
        },
    })));
    assert_invalid(ControlRequest::Vehicles(Box::new(VehicleRequest::Set {
        id: 0,
        patch: VehiclePatch {
            position: Some(VehiclePosition::Gps {
                lat: ResolvedVehicleField {
                    id: FieldId(0),
                    path: String::new(),
                },
                lon: ResolvedVehicleField {
                    id: FieldId(1),
                    path: "lon".into(),
                },
                alt: ResolvedVehicleField {
                    id: FieldId(2),
                    path: "alt".into(),
                },
                lat_lon_dege7: false,
                alt_mm: false,
                alt_offset_m: 0.0,
            }),
            ..Default::default()
        },
    })));
    assert_invalid(ControlRequest::VehicleProfiles(
        VehicleProfileRequest::Load {
            name: "../bad".into(),
        },
    ));
    assert_invalid(ControlRequest::VehicleProfiles(
        VehicleProfileRequest::Apply {
            name: "valid".into(),
            source_id: SourceId(0),
            source: String::new(),
            owner: None,
        },
    ));
}

#[test]
fn raw_layout_workspace_and_batch_requests_validate_content_and_policy() {
    assert_invalid(ControlRequest::Layouts(LayoutRequest::Save {
        name: "../bad".into(),
    }));
    assert_invalid(ControlRequest::Layouts(LayoutRequest::ImportFile {
        path: " ".into(),
    }));
    assert_invalid(ControlRequest::Layouts(LayoutRequest::ExportFile {
        name: "valid".into(),
        path: " ".into(),
    }));
    assert_invalid(ControlRequest::Layouts(LayoutRequest::Rename {
        from: "valid".into(),
        to: "../bad".into(),
    }));
    assert_invalid(ControlRequest::Layouts(LayoutRequest::Apply {
        json: " ".into(),
    }));
    assert_invalid(ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
        owner: None,
        title: Some(" ".into()),
    }));
    assert_invalid(ControlRequest::Batch(vec![ControlRequest::Layouts(
        LayoutRequest::Current,
    )]));
    let error = ControlRequest::Batch(vec![ControlRequest::Playback(PlaybackRequest::Set {
        speed: Some(f64::NAN),
        follow_live: None,
    })])
    .validate()
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert!(error.to_string().contains("batch request 0"));
    assert!(
        ControlRequest::Batch(vec![ControlRequest::Playback(PlaybackRequest::Set {
            speed: Some(-2.0),
            follow_live: Some(true)
        })])
        .validate()
        .is_ok()
    );
}
#[test]
fn native_workspace_and_cleanup_validate_resource_owners() {
    use delog_api::control::{GenerationRequest, ResourceOwner, SplitDirection, WorkspaceRequest};
    let invalid_owner = Some(ResourceOwner {
        name: String::new(),
        generation: 0,
    });
    for request in [
        WorkspaceRequest::AddPlot {
            window: None,
            direction: SplitDirection::Horizontal,
            owner: invalid_owner.clone(),
        },
        WorkspaceRequest::Split {
            window: 0,
            tile: 1,
            direction: SplitDirection::Vertical,
            owner: invalid_owner.clone(),
        },
        WorkspaceRequest::OpenWindow {
            title: None,
            owner: invalid_owner,
        },
    ] {
        assert_invalid(ControlRequest::Workspace(request));
    }
    assert_invalid(ControlRequest::Generation(GenerationRequest::RemoveOwned {
        owner: String::new(),
    }));
    assert!(
        ControlRequest::Generation(GenerationRequest::RemoveOwned {
            owner: "external".into()
        })
        .validate()
        .is_ok()
    );
}
