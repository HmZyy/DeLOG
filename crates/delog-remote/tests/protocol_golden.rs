use delog_remote::{
    ApiError, ClientRegistry, ErrorEnvelope, InstanceDto, OpaqueId, RegisterClientRequest,
    RegisterClientResponse, SecretToken, WireCompletion,
};
use serde_json::json;

#[test]
fn unknown_registration_fields_are_rejected() {
    let json = r#"{"name":"flight-diagnosis","takeover":false,"typo":1}"#;
    assert!(serde_json::from_str::<RegisterClientRequest>(json).is_err());
}

#[test]
fn registration_request_defaults_takeover_to_false() {
    let request: RegisterClientRequest =
        serde_json::from_str(r#"{"name":"flight-diagnosis"}"#).unwrap();
    assert_eq!(request.name, "flight-diagnosis");
    assert!(!request.takeover);
}

#[test]
fn instance_dto_serializes_to_the_documented_shape() {
    let dto = InstanceDto::current(
        OpaqueId::new("inst-test-0001"),
        "bench session",
        Some("arducopter-2026-09-01.ulg".to_string()),
    );

    let value = serde_json::to_value(&dto).unwrap();

    assert_eq!(
        value,
        json!({
            "instance_id": "inst-test-0001",
            "label": "bench session",
            "session_description": "arducopter-2026-09-01.ulg",
            "api_major": 1,
            "api_min_minor": 0,
            "api_max_minor": 0,
        })
    );
}

#[test]
fn instance_dto_omits_session_description_when_absent() {
    let dto = InstanceDto::current(OpaqueId::new("inst-test-0002"), "bench session", None);

    let value = serde_json::to_value(&dto).unwrap();

    assert_eq!(
        value,
        json!({
            "instance_id": "inst-test-0002",
            "label": "bench session",
            "api_major": 1,
            "api_min_minor": 0,
            "api_max_minor": 0,
        })
    );
}

#[test]
fn error_envelope_serializes_to_the_documented_shape() {
    let error = ApiError::snapshot_expired("lease abc123 expired");
    let envelope = ErrorEnvelope::from_error(OpaqueId::new("req-test-0001"), &error);

    let value = serde_json::to_value(&envelope).unwrap();

    assert_eq!(
        value,
        json!({
            "request_id": "req-test-0001",
            "code": "snapshot_expired",
            "message": "lease abc123 expired",
            "retryable": false,
        })
    );
}

#[test]
fn error_envelope_includes_details_and_completion_when_present() {
    let error = ApiError::unavailable("control queue is full")
        .with_completion(WireCompletion::Unknown)
        .with_details(json!({ "queued": 64 }));
    let envelope = ErrorEnvelope::from_error(OpaqueId::new("req-test-0002"), &error);

    let value = serde_json::to_value(&envelope).unwrap();

    assert_eq!(
        value,
        json!({
            "request_id": "req-test-0002",
            "code": "unavailable",
            "message": "control queue is full",
            "retryable": true,
            "details": { "queued": 64 },
            "completion": "unknown",
        })
    );
}

#[test]
fn every_wire_code_round_trips_through_api_error() {
    let cases = [
        (ApiError::invalid_input("x"), "invalid_input", false),
        (ApiError::not_found("x"), "not_found", false),
        (ApiError::ambiguous("x"), "ambiguous", false),
        (ApiError::stale_handle("x"), "stale_handle", false),
        (ApiError::forbidden("x"), "forbidden", false),
        (ApiError::snapshot_expired("x"), "snapshot_expired", false),
        (ApiError::conflict("x"), "conflict", false),
        (ApiError::unavailable("x"), "unavailable", true),
        (ApiError::internal("x"), "internal", false),
    ];

    for (error, code, retryable) in cases {
        assert_eq!(error.code(), code);
        assert_eq!(error.retryable(), retryable);
    }
}

#[test]
fn native_errors_map_onto_the_approved_wire_codes() {
    let cases = [
        (delog_api::Error::invalid_input("x"), "invalid_input"),
        (delog_api::Error::not_found("x"), "not_found"),
        (delog_api::Error::ambiguous("x"), "ambiguous"),
        (delog_api::Error::stale_handle("x"), "stale_handle"),
        (delog_api::Error::forbidden("x"), "forbidden"),
        (delog_api::Error::conflict("x"), "conflict"),
        (delog_api::Error::unavailable("x"), "unavailable"),
        (delog_api::Error::protocol("x"), "internal"),
        (delog_api::Error::execution("x"), "internal"),
        (delog_api::Error::internal("x"), "internal"),
    ];

    for (native, code) in cases {
        let wire = ApiError::from(native);
        assert_eq!(wire.code(), code);
        assert_eq!(wire.message(), "x");
    }
}

#[test]
fn successful_registration_response_serializes_to_the_documented_shape() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let request: RegisterClientRequest =
        serde_json::from_str(r#"{"name":"flight-diagnosis","takeover":false}"#).unwrap();

    let registered = registry.register(&bootstrap, &bootstrap, request).unwrap();
    let response = RegisterClientResponse::from(&registered);

    let value = serde_json::to_value(&response).unwrap();

    assert_eq!(
        value.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["client_id", "owner_id", "owner_name", "token"]
    );
    assert_eq!(value["owner_name"], "flight-diagnosis");
    assert_eq!(value["client_id"], registered.client_id.as_str());
    assert_eq!(value["owner_id"], registered.owner_id.as_str());
    assert_eq!(value["token"], registered.token.expose());
}
