#![cfg(feature = "python")]

use std::sync::{Arc, Mutex};

use delog_api::control::{
    ControlHost, ControlRequest, ControlResponse, PlotInfo, TraceInfo, TraceMode, TraceRequest,
};
use delog_core::identity::{FieldId, IdentityRegistry};
use delog_core::snapshot::StoreSnapshot;

#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<ControlRequest>>,
}

impl ControlHost for Recorder {
    fn call(&self, request: ControlRequest) -> delog_api::Result<ControlResponse> {
        self.seen.lock().unwrap().push(request.clone());
        match request {
            ControlRequest::Traces(TraceRequest::List { .. }) => {
                Ok(ControlResponse::Traces(vec![TraceInfo {
                    index: 0,
                    field_id: FieldId(0),
                    field: "IMU.AccX".into(),
                    color: [0.3, 0.6, 1.0, 1.0],
                    width_px: 1.5,
                    mode: TraceMode::Line,
                    visible: true,
                }]))
            }
            ControlRequest::Plots(_) => Ok(ControlResponse::Plots(vec![plot_fixture()])),
            _ => Ok(ControlResponse::Unit),
        }
    }
}

fn plot_fixture() -> PlotInfo {
    PlotInfo {
        window: 0,
        tile: 7,
        index: 0,
        label: "Plot 1".into(),
    }
}

fn one_source_snapshot() -> (Arc<StoreSnapshot>, FieldId) {
    let mut ids = IdentityRegistry::new();
    let source = ids.add_source("flight");
    let topic = ids.add_topic(source, "IMU").unwrap();
    let field = ids.add_field(topic, "AccX").unwrap();
    (
        Arc::new(StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot")),
        field,
    )
}

fn two_source_snapshot() -> (Arc<StoreSnapshot>, FieldId, FieldId) {
    let mut ids = IdentityRegistry::new();
    let source_a = ids.add_source("flight_a");
    let topic_a = ids.add_topic(source_a, "IMU").unwrap();
    let field_a = ids.add_field(topic_a, "AccX").unwrap();
    let source_b = ids.add_source("flight_b");
    let topic_b = ids.add_topic(source_b, "IMU").unwrap();
    let field_b = ids.add_field(topic_b, "AccX").unwrap();
    (
        Arc::new(StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot")),
        field_a,
        field_b,
    )
}

#[test]
fn adding_a_trace_sends_the_worker_resolved_field_id_and_the_chosen_mode() {
    let (snapshot, field) = one_source_snapshot();
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host_and_snapshot(
        recorder.clone(),
        snapshot,
        "delog.plots()[0].traces.add('IMU.AccX', mode='step', color='#4C9AFF')",
    )
    .unwrap();
    let seen = recorder.seen.lock().unwrap();
    assert!(
        seen.iter().any(|request| matches!(
            request,
            ControlRequest::Traces(TraceRequest::Add { field_id, mode, .. })
                if *field_id == field && *mode == TraceMode::Step
        )),
        "{seen:?}"
    );
}

#[test]
fn an_unknown_trace_mode_is_rejected_before_any_round_trip() {
    let (snapshot, _field) = one_source_snapshot();
    let recorder = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host_and_snapshot(
        recorder.clone(),
        snapshot,
        "delog.plots()[0].traces.add('IMU.AccX', mode='squiggle')",
    )
    .unwrap_err();
    assert!(error.contains("ValueError"), "{error}");
    assert!(
        !recorder
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, ControlRequest::Traces(TraceRequest::Add { .. }))),
        "a bad mode must not reach the app"
    );
}

#[test]
fn adding_a_field_ref_from_a_specific_source_targets_that_source() {
    let (snapshot, field_a, _field_b) = two_source_snapshot();
    let recorder = Arc::new(Recorder::default());
    delog_script::control::testing::eval_with_host_and_snapshot(
        recorder.clone(),
        snapshot,
        "delog.plots()[0].traces.add(delog.find('IMU', 'AccX', source='flight_a'))",
    )
    .unwrap();
    let seen = recorder.seen.lock().unwrap();
    assert!(
        seen.iter().any(|request| matches!(
            request,
            ControlRequest::Traces(TraceRequest::Add { field_id, .. })
                if *field_id == field_a
        )),
        "{seen:?}"
    );
}

#[test]
fn a_bare_field_string_ambiguous_across_two_sources_is_rejected_before_any_round_trip() {
    let (snapshot, _field_a, _field_b) = two_source_snapshot();
    let recorder = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host_and_snapshot(
        recorder.clone(),
        snapshot,
        "delog.plots()[0].traces.add('IMU.AccX')",
    )
    .unwrap_err();
    assert!(error.contains("ValueError"), "{error}");
    assert!(error.contains("ambiguous"), "{error}");
    assert!(
        !recorder
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, ControlRequest::Traces(TraceRequest::Add { .. }))),
        "an ambiguous field must not reach the app"
    );
}

#[test]
fn remove_without_a_position_or_a_field_is_rejected_before_any_round_trip() {
    let (snapshot, _field) = one_source_snapshot();
    let recorder = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host_and_snapshot(
        recorder.clone(),
        snapshot,
        "delog.plots()[0].traces.remove()",
    )
    .unwrap_err();
    assert!(error.contains("ValueError"), "{error}");
    assert!(
        !recorder
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, ControlRequest::Traces(TraceRequest::Remove { .. }))),
        "a target-less remove must not reach the app"
    );
}

#[test]
fn remove_with_both_a_position_and_a_field_is_rejected_before_any_round_trip() {
    let (snapshot, _field) = one_source_snapshot();
    let recorder = Arc::new(Recorder::default());
    let error = delog_script::control::testing::eval_with_host_and_snapshot(
        recorder.clone(),
        snapshot,
        "delog.plots()[0].traces.remove(0, field='IMU.AccX')",
    )
    .unwrap_err();
    assert!(error.contains("ValueError"), "{error}");
    assert!(
        !recorder
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, ControlRequest::Traces(TraceRequest::Remove { .. }))),
        "an over-specified remove must not reach the app"
    );
}
