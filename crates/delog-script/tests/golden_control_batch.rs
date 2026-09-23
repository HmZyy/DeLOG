#![cfg(feature = "python")]

use std::sync::Arc;

use delog_script::control::RecordingHost;
use delog_script::{
    ControlRequest, MarkerRequest, PendingMarker, PlaybackRequest, TraceMode, TraceRequest,
};

#[test]
fn a_successful_batch_stages_supported_requests_in_issue_order() {
    let host = Arc::new(RecordingHost::default());
    let batches = delog_script::control::testing::eval_with_host_and_staged_batches(
        host.clone(),
        "with delog.batch():\n\
         \x20   delog.playback.speed = 2.0\n\
         \x20   delog.markers.add(10, 'armed')",
    )
    .unwrap();

    assert!(host.taken().is_empty());
    assert_eq!(
        batches,
        vec![vec![
            ControlRequest::Playback(PlaybackRequest::Set {
                speed: Some(2.0),
                follow_live: None,
            }),
            ControlRequest::Markers(MarkerRequest::Append {
                owner: "flight.py".into(),
                generation: 1,
                markers: vec![PendingMarker {
                    time_us: 10,
                    label: "armed".into(),
                    color: None,
                    note: String::new(),
                }],
            }),
        ]]
    );
}

#[test]
fn a_python_exception_discards_the_block_and_nested_batches_are_rejected() {
    let host = Arc::new(RecordingHost::default());
    let error = delog_script::control::testing::eval_with_host_and_staged_batches(
        host.clone(),
        "try:\n\
         \x20   with delog.batch():\n\
         \x20       delog.playback.follow_live = True\n\
         \x20       raise RuntimeError('boom')\n\
         except RuntimeError:\n\
         \x20   pass\n\
         with delog.batch():\n\
         \x20   with delog.batch():\n\
         \x20       pass",
    )
    .unwrap_err();
    assert!(
        error.contains("ValueError") && error.contains("nested"),
        "{error}"
    );
    assert!(host.taken().is_empty());
}

#[test]
fn response_returning_operations_are_rejected_before_staging() {
    let host = Arc::new(RecordingHost::default());
    let error = delog_script::control::testing::eval_with_host_and_staged_batches(
        host.clone(),
        "with delog.batch():\n\
         \x20   delog.plots()",
    )
    .unwrap_err();
    assert!(
        error.contains("ValueError") && error.contains("batch"),
        "{error}"
    );
    assert!(host.taken().is_empty());
}

#[test]
fn protocol_keeps_trace_mutations_batchable_without_matching_reads() {
    let add = ControlRequest::Traces(TraceRequest::Add {
        window: 0,
        tile: 1,
        field_id: delog_core::identity::FieldId(2),
        field: "flight/IMU/x".into(),
        color: None,
        width_px: None,
        mode: TraceMode::Line,
        owner: None,
    });
    assert!(delog_script::control::request_is_batchable(&add));
    assert!(!delog_script::control::request_is_batchable(
        &ControlRequest::Traces(TraceRequest::List { window: 0, tile: 1 })
    ));
}
