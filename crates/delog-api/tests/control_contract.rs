use delog_api::ErrorKind;
use delog_api::control::{
    AnnotationGeometry, AnnotationKind, AnnotationStylePatch, ControlRequest, ControlResponse,
    LayoutRequest, MarkerFilter, MarkerOrigin, MarkerPatch, MarkerRequest, PlaybackRequest,
    PlotInfo, SplitDirection, TraceMode, TraceRequest, request_is_batchable, validate_layout_name,
    validate_layout_path,
};

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
    let plots = vec![PlotInfo {
        window: 0,
        tile: 7,
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
