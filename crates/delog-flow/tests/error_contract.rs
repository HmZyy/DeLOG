use std::error::Error;

use delog_flow::command::ApplyError;
use delog_flow::graph::ConnectError;

#[test]
fn connection_error_has_actionable_display_text() {
    assert_eq!(
        ConnectError::TypeMismatch.to_string(),
        "incompatible port types"
    );
    assert_eq!(
        ConnectError::InputOccupied.to_string(),
        "input already connected"
    );
}

#[test]
fn command_error_retains_the_connection_cause() {
    let error = ApplyError::Connect(ConnectError::Cycle);
    assert_eq!(error.to_string(), "connection would create a cycle");
    assert_eq!(
        error.source().map(ToString::to_string),
        Some("connection would create a cycle".to_owned())
    );
}
