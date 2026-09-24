use delog_api::control::{
    ControlRequest, MarkerFilter, MarkerRequest, SplitDirection, WorkspaceRequest,
    request_is_batchable,
};
use delog_api::operations::OperationMode;
use delog_api::params::{ParamValue, shared_empty};
use delog_api::timestamps::TimestampMode;

#[test]
fn native_consumers_can_reach_each_capability_without_delog_script() {
    let request = ControlRequest::Workspace(WorkspaceRequest::AddPlot {
        direction: SplitDirection::Horizontal,
    });
    assert!(!request_is_batchable(&request));
    assert_eq!(OperationMode::parse(None).unwrap(), OperationMode::Both);
    assert_eq!(TimestampMode::default(), TimestampMode::Effective);
    assert!(shared_empty().lock().unwrap().scripts.is_empty());
    assert_eq!(ParamValue::Bool(true), ParamValue::Bool(true));
    let marker = ControlRequest::Markers(MarkerRequest::Remove(MarkerFilter::ScriptAll));
    assert!(request_is_batchable(&marker));
}
