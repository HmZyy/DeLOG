use delog_api::ErrorKind;
use delog_api::control::{
    ControlRequest, ControlResponse, LayoutRequest, MarkerFilter, MarkerRequest, PlaybackRequest,
    PlotInfo, request_is_batchable,
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
