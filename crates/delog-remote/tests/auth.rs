use std::time::Instant;

use delog_remote::{ClientRegistry, RegisterClientRequest, RegisterClientResponse, SecretToken};

#[test]
fn secrets_are_random_redacted_and_constant_time_comparable() {
    let first = SecretToken::generate().unwrap();
    let second = SecretToken::generate().unwrap();
    assert_ne!(first.expose(), second.expose());
    assert_eq!(format!("{first:?}"), "SecretToken([REDACTED])");
    assert!(first.matches(&SecretToken::parse(first.expose()).unwrap()));
    assert!(!first.matches(&second));
}

#[test]
fn parse_round_trips_a_generated_token() {
    let token = SecretToken::generate().unwrap();
    let parsed = SecretToken::parse(token.expose()).unwrap();
    assert!(token.matches(&parsed));
}

#[test]
fn parse_rejects_wrong_length_and_invalid_encoding() {
    assert!(SecretToken::parse("not-base64-!!!").is_err());
    assert!(SecretToken::parse("YQ").is_err());
}

fn register_request(name: &str, takeover: bool) -> RegisterClientRequest {
    serde_json::from_value(serde_json::json!({ "name": name, "takeover": takeover })).unwrap()
}

#[test]
fn registration_response_debug_output_redacts_the_token() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();

    let registered = registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("flight-diagnosis", false),
        )
        .unwrap();
    let response = RegisterClientResponse::from(&registered);

    let debug_output = format!("{response:?}");

    assert!(!debug_output.contains(&response.token));
    assert!(debug_output.contains("[REDACTED]"));
}

#[test]
fn register_rejects_a_presented_token_that_does_not_match_the_bootstrap() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let wrong = SecretToken::generate().unwrap();

    let error = registry
        .register(
            &bootstrap,
            &wrong,
            register_request("flight-diagnosis", false),
        )
        .unwrap_err();

    assert_eq!(error.code(), "forbidden");
}

#[test]
fn register_trims_the_name_and_returns_a_fresh_token() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();

    let registered = registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("  flight-diagnosis  ", false),
        )
        .unwrap();

    assert_eq!(registered.owner_name, "flight-diagnosis");
}

#[test]
fn register_rejects_empty_and_overlong_and_invalid_names() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();

    for name in ["", "   ", &"a".repeat(65), "bad name", "bad/name"] {
        let error = registry
            .register(&bootstrap, &bootstrap, register_request(name, false))
            .unwrap_err();
        assert_eq!(
            error.code(),
            "invalid_input",
            "name {name:?} should be rejected"
        );
    }
}

#[test]
fn register_accepts_the_full_allowed_character_set() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();

    let registered = registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("flight.diagnosis_run-01", false),
        )
        .unwrap();

    assert_eq!(registered.owner_name, "flight.diagnosis_run-01");
}

#[test]
fn second_registration_without_takeover_conflicts() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();

    registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("flight-diagnosis", false),
        )
        .unwrap();

    let error = registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("flight-diagnosis", false),
        )
        .unwrap_err();

    assert_eq!(error.code(), "conflict");
    assert!(!error.message().contains("flight-diagnosis"));
    assert!(error.details().is_none());
}

#[test]
fn takeover_revokes_the_previous_client_and_reuses_the_owner() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();

    let first = registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("flight-diagnosis", false),
        )
        .unwrap();

    let second = registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("flight-diagnosis", true),
        )
        .unwrap();

    assert_eq!(first.owner_id, second.owner_id);
    assert_ne!(first.client_id, second.client_id);

    let error = registry
        .authenticate(&first.token, Instant::now())
        .unwrap_err();
    assert_eq!(error.code(), "forbidden");
}

#[test]
fn reconnecting_by_name_after_full_revocation_reuses_the_owner() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();

    let first = registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("flight-diagnosis", false),
        )
        .unwrap();

    assert!(registry.revoke(&first.client_id));

    let second = registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("flight-diagnosis", false),
        )
        .unwrap();

    assert_eq!(first.owner_id, second.owner_id);
}

#[test]
fn authenticate_finds_the_registered_client_and_returns_its_session() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();

    let registered = registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("flight-diagnosis", false),
        )
        .unwrap();

    let session = registry
        .authenticate(&registered.token, Instant::now())
        .unwrap();

    assert_eq!(session.client_id, registered.client_id);
    assert_eq!(session.owner_id, registered.owner_id);
    assert_eq!(session.owner_name, "flight-diagnosis");
}

#[test]
fn authenticate_rejects_an_unknown_token() {
    let mut registry = ClientRegistry::new();
    let unknown = SecretToken::generate().unwrap();

    let error = registry.authenticate(&unknown, Instant::now()).unwrap_err();

    assert_eq!(error.code(), "forbidden");
}

#[test]
fn revoke_invalidates_a_previously_authenticated_token() {
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();

    let registered = registry
        .register(
            &bootstrap,
            &bootstrap,
            register_request("flight-diagnosis", false),
        )
        .unwrap();

    assert!(registry.revoke(&registered.client_id));
    assert!(!registry.revoke(&registered.client_id));

    let error = registry
        .authenticate(&registered.token, Instant::now())
        .unwrap_err();
    assert_eq!(error.code(), "forbidden");
}
