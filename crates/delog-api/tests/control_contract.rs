use delog_api::ErrorKind;
use delog_api::control::{
    ControlRequest, ControlResponse, LayoutRequest, MarkerFilter, MarkerRequest, PlaybackRequest,
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
