use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use delog_core::snapshot::DataStore;
use delog_remote::{
    RemoteConfig, SecretToken,
    routes::{RouterState, router},
};
use serde_json::{Value, json};
use tower::ServiceExt;
mod support;

fn fixture() -> Arc<RouterState> {
    RouterState::new(
        RemoteConfig {
            label: "test".into(),
            loaded_file: Some("flight.bin".into()),
            lease_idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_millis(50),
            max_concurrent_downloads: 1,
            discovery_root: Some(tempfile::tempdir().unwrap().path().to_owned()),
            control: delog_remote::ControlLimits::default(),
            uploads: delog_remote::UploadConfig::default(),
        },
        support::services(Arc::new(DataStore::new())),
        "127.0.0.1:12345".parse().unwrap(),
    )
    .unwrap()
}

async fn call(
    state: &Arc<RouterState>,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:12345")
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let response = router(state.clone())
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["content-type"], "application/json");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    if status.is_client_error() || status.is_server_error() {
        assert!(
            value["request_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
    }
    (status, value)
}

async fn register(state: &Arc<RouterState>, name: &str, takeover: bool) -> (StatusCode, Value) {
    call(
        state,
        "POST",
        "/v1/clients",
        Some(&state.bootstrap_token().expose()),
        json!({"name":name,"takeover":takeover}),
    )
    .await
}

#[tokio::test]
async fn origin_and_host_fail_closed_before_authentication() {
    let state = fixture();
    for (host, origin) in [
        (Some("127.0.0.1:12345"), Some("https://example.test")),
        (Some("localhost:12345"), None),
        (Some("127.0.0.1:9"), None),
        (None, None),
    ] {
        let mut req = Request::builder().uri("/v1/instance");
        if let Some(host) = host {
            req = req.header("host", host);
        }
        if let Some(origin) = origin {
            req = req.header("origin", origin);
        }
        let response = router(state.clone())
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(value["request_id"].is_string());
    }
}

#[tokio::test]
async fn instance_allows_bootstrap_or_live_client_and_reports_version_without_secrets() {
    let state = fixture();
    for token in [
        None,
        Some("malformed".to_owned()),
        Some(SecretToken::generate().unwrap().expose()),
    ] {
        assert_eq!(
            call(&state, "GET", "/v1/instance", token.as_deref(), json!(null))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let bootstrap = state.bootstrap_token().expose();
    let (status, instance) =
        call(&state, "GET", "/v1/instance", Some(&bootstrap), json!(null)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(instance["api_major"], 1);
    assert_eq!(instance["api_min_minor"], 0);
    assert_eq!(instance["api_max_minor"], 0);
    assert!(!instance.to_string().contains(&bootstrap));
    let (_, client) = register(&state, "reader", false).await;
    let token = client["token"].as_str().unwrap();
    assert_eq!(
        call(&state, "GET", "/v1/instance", Some(token), json!(null))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            "POST",
            "/v1/snapshots",
            Some(&bootstrap),
            json!(null)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &state,
            "POST",
            "/v1/clients",
            Some(token),
            json!({"name":"new"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&state, "GET", "/v2/instance", Some(token), json!(null))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn takeover_revokes_the_old_client_and_its_leases() {
    let state = fixture();
    let (_, first) = register(&state, "reader", false).await;
    let token = first["token"].as_str().unwrap();
    assert_eq!(
        register(&state, "reader", false).await.0,
        StatusCode::CONFLICT
    );
    let (_, lease) = call(&state, "POST", "/v1/snapshots", Some(token), json!(null)).await;
    let (_, second) = register(&state, "reader", true).await;
    assert_eq!(first["owner_id"], second["owner_id"]);
    assert_ne!(first["client_id"], second["client_id"]);
    assert_eq!(
        call(&state, "GET", "/v1/instance", Some(token), json!(null))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert!(state.status().leases.is_empty());
    assert!(lease["lease_id"].is_string());
}

#[tokio::test]
async fn registration_conflicts_do_not_echo_token_shaped_names() {
    let state = fixture();
    let secret = state.bootstrap_token().expose();
    assert_eq!(register(&state, &secret, false).await.0, StatusCode::OK);
    let (status, error) = register(&state, &secret, false).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "conflict");
    assert!(error["request_id"].is_string());
    assert!(!error.to_string().contains(&secret));
}

#[tokio::test]
async fn client_and_lease_ownership_is_enforced_and_expiry_is_reported() {
    let state = fixture();
    let (_, one) = register(&state, "one", false).await;
    let (_, two) = register(&state, "two", false).await;
    let a = one["token"].as_str().unwrap();
    let b = two["token"].as_str().unwrap();
    let (_, lease) = call(&state, "POST", "/v1/snapshots", Some(a), json!(null)).await;
    let id = lease["lease_id"].as_str().unwrap();
    assert_eq!(
        call(
            &state,
            "DELETE",
            &format!("/v1/clients/{}", one["client_id"].as_str().unwrap()),
            Some(b),
            json!(null)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for (method, path) in [
        ("GET", format!("/v1/snapshots/{id}/catalog")),
        ("DELETE", format!("/v1/snapshots/{id}")),
    ] {
        assert_eq!(
            call(&state, method, &path, Some(b), json!(null)).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(
            &state,
            "GET",
            &format!("/v1/snapshots/{id}/topics/missing/data"),
            Some(a),
            json!(null)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    state.reap(Instant::now() + Duration::from_secs(61));
    assert!(state.status().clients.is_empty());
    assert!(state.status().leases.is_empty());
}

#[tokio::test]
async fn malformed_and_oversized_json_and_wrong_methods_have_error_envelopes() {
    let state = fixture();
    let token = state.bootstrap_token().expose();
    for body in [
        "{broken".to_owned(),
        json!({"name":"x".repeat(65536)}).to_string(),
    ] {
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/clients")
                    .header("host", "127.0.0.1:12345")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(value["code"], "invalid_input");
        assert!(value["request_id"].is_string());
        assert!(!value.to_string().contains(&token));
    }
    assert_eq!(
        call(&state, "PATCH", "/v1/instance", Some(&token), json!(null))
            .await
            .0,
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn projected_arrow_filters_signed_times_and_holds_the_download_slot_until_body_drop() {
    let state = RouterState::new(
        RemoteConfig {
            label: "data".into(),
            loaded_file: None,
            lease_idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_millis(50),
            max_concurrent_downloads: 1,
            discovery_root: Some(tempfile::tempdir().unwrap().path().to_owned()),
            control: delog_remote::ControlLimits::default(),
            uploads: delog_remote::UploadConfig::default(),
        },
        support::services(support::store(2, 4)),
        "127.0.0.1:12345".parse().unwrap(),
    )
    .unwrap();
    let (_, client) = register(&state, "reader", false).await;
    let token = client["token"].as_str().unwrap();
    let (_, lease) = call(&state, "POST", "/v1/snapshots", Some(token), json!(null)).await;
    let id = lease["lease_id"].as_str().unwrap();
    let (_, catalog) = call(
        &state,
        "GET",
        &format!("/v1/snapshots/{id}/catalog"),
        Some(token),
        json!(null),
    )
    .await;
    let topic = &catalog["sources"][0]["topics"][0];
    let path = format!(
        "/v1/snapshots/{id}/topics/{}/data",
        topic["handle"].as_str().unwrap()
    );
    let uri = format!(
        "{path}?fields={}&start_ns=-9000&end_ns=-7000",
        topic["fields"][1]["handle"].as_str().unwrap()
    );
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header("host", "127.0.0.1:12345")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/vnd.apache.arrow.stream"
    );
    let (status, error) = call(&state, "GET", &uri, Some(token), json!(null)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["code"], "unavailable");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let reader = arrow_ipc::reader::StreamReader::try_new(bytes.as_ref(), None).unwrap();
    assert_eq!(reader.schema().fields().len(), 3);
    assert_eq!(reader.schema().field(2).name(), "y");
    let batches = reader.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 3);
    let times = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow::array::Int64Array>()
        .unwrap();
    assert_eq!(times.values().as_ref(), &[-9000, -8000, -7000]);
    for suffix in [
        "?start_ns=oops",
        "?end_ns=9223372036854775808",
        "?fields=missing",
        "?start_ns=2&end_ns=1",
    ] {
        let (status, _) = call(
            &state,
            "GET",
            &format!("{path}{suffix}"),
            Some(token),
            json!(null),
        )
        .await;
        assert!(matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND
        ));
    }
    let (_, error) = call(
        &state,
        "GET",
        &format!("{path}?fields={token}"),
        Some(token),
        json!(null),
    )
    .await;
    assert!(!error.to_string().contains(token));
}

#[tokio::test]
async fn expired_lease_returns_gone_without_a_reaper_tick() {
    let state = RouterState::new(
        RemoteConfig {
            label: "expiry".into(),
            loaded_file: None,
            lease_idle_timeout: Duration::from_millis(80),
            request_timeout: Duration::from_secs(1),
            max_concurrent_downloads: 1,
            discovery_root: Some(tempfile::tempdir().unwrap().path().to_owned()),
            control: delog_remote::ControlLimits::default(),
            uploads: delog_remote::UploadConfig::default(),
        },
        support::services(Arc::new(DataStore::new())),
        "127.0.0.1:12345".parse().unwrap(),
    )
    .unwrap();
    let (_, client) = register(&state, "reader", false).await;
    let token = client["token"].as_str().unwrap();
    let (_, lease) = call(&state, "POST", "/v1/snapshots", Some(token), json!(null)).await;
    tokio::time::sleep(Duration::from_millis(90)).await;
    call(&state, "GET", "/v1/instance", Some(token), json!(null)).await;
    let (status, error) = call(
        &state,
        "GET",
        &format!(
            "/v1/snapshots/{}/catalog",
            lease["lease_id"].as_str().unwrap()
        ),
        Some(token),
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(error["code"], "snapshot_expired");
}

#[tokio::test]
async fn snapshot_requests_enforce_body_limit_and_errors_do_not_echo_url_handles() {
    let state = fixture();
    let (_, client) = register(&state, "reader", false).await;
    let token = client["token"].as_str().unwrap();
    assert_eq!(
        call(
            &state,
            "POST",
            "/v1/snapshots",
            Some(token),
            json!({"extra":"x".repeat(65536)})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_, error) = call(
        &state,
        "GET",
        &format!("/v1/snapshots/{token}/catalog"),
        Some(token),
        json!(null),
    )
    .await;
    assert!(!error.to_string().contains(token));
}

#[tokio::test]
async fn revoking_an_unpolled_body_releases_its_download_slot() {
    let state = RouterState::new(
        RemoteConfig {
            label: "data".into(),
            loaded_file: None,
            lease_idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_millis(50),
            max_concurrent_downloads: 1,
            discovery_root: Some(tempfile::tempdir().unwrap().path().to_owned()),
            control: delog_remote::ControlLimits::default(),
            uploads: delog_remote::UploadConfig::default(),
        },
        support::services(support::store(64, 32)),
        "127.0.0.1:12345".parse().unwrap(),
    )
    .unwrap();
    let (_, client) = register(&state, "reader", false).await;
    let token = client["token"].as_str().unwrap();
    let mut held_body = None;
    for _ in 0..2 {
        let (_, lease) = call(&state, "POST", "/v1/snapshots", Some(token), json!(null)).await;
        let id = lease["lease_id"].as_str().unwrap();
        let (_, catalog) = call(
            &state,
            "GET",
            &format!("/v1/snapshots/{id}/catalog"),
            Some(token),
            json!(null),
        )
        .await;
        let topic = catalog["sources"][0]["topics"][0]["handle"]
            .as_str()
            .unwrap();
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/snapshots/{id}/topics/{topic}/data"))
                    .header("host", "127.0.0.1:12345")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(state.revoke_lease(&serde_json::from_value(lease["lease_id"].clone()).unwrap()));
        if held_body.is_none() {
            held_body = Some(response);
        }
    }
    drop(held_body);
}

#[tokio::test]
async fn closed_and_revoked_leases_read_as_expired_for_their_owner() {
    let state = fixture();
    let (_, one) = register(&state, "one", false).await;
    let (_, two) = register(&state, "two", false).await;
    let a = one["token"].as_str().unwrap();
    let b = two["token"].as_str().unwrap();
    let (_, closed) = call(&state, "POST", "/v1/snapshots", Some(a), json!(null)).await;
    let closed = closed["lease_id"].as_str().unwrap();
    let (_, revoked) = call(&state, "POST", "/v1/snapshots", Some(a), json!(null)).await;
    let revoked_id: delog_remote::LeaseId =
        serde_json::from_value(revoked["lease_id"].clone()).unwrap();
    let revoked = revoked["lease_id"].as_str().unwrap();
    assert_eq!(
        call(
            &state,
            "DELETE",
            &format!("/v1/snapshots/{closed}"),
            Some(a),
            json!(null)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(state.revoke_lease(&revoked_id));
    assert!(!state.revoke_lease(&revoked_id));
    for id in [closed, revoked] {
        let (status, error) = call(
            &state,
            "GET",
            &format!("/v1/snapshots/{id}/catalog"),
            Some(a),
            json!(null),
        )
        .await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(error["code"], "snapshot_expired");
        assert_eq!(
            call(
                &state,
                "GET",
                &format!("/v1/snapshots/{id}/catalog"),
                Some(b),
                json!(null)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &state,
                "DELETE",
                &format!("/v1/snapshots/{id}"),
                Some(a),
                json!(null)
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert!(state.status().leases.is_empty());
}

fn state_with(store: Arc<DataStore>, request_timeout: Duration) -> Arc<RouterState> {
    RouterState::new(
        RemoteConfig {
            label: "limits".into(),
            loaded_file: None,
            lease_idle_timeout: Duration::from_secs(60),
            request_timeout,
            max_concurrent_downloads: 1,
            discovery_root: Some(tempfile::tempdir().unwrap().path().to_owned()),
            control: delog_remote::ControlLimits::default(),
            uploads: delog_remote::UploadConfig::default(),
        },
        support::services(store),
        "127.0.0.1:12345".parse().unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn a_client_is_limited_to_sixteen_active_leases() {
    let state = fixture();
    let name = "lease-hoarder-name";
    let (_, client) = register(&state, name, false).await;
    let token = client["token"].as_str().unwrap();
    let mut leases = Vec::new();
    for _ in 0..16 {
        let (status, lease) = call(&state, "POST", "/v1/snapshots", Some(token), json!(null)).await;
        assert_eq!(status, StatusCode::OK);
        leases.push(lease["lease_id"].as_str().unwrap().to_owned());
    }
    let (status, error) = call(&state, "POST", "/v1/snapshots", Some(token), json!(null)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["code"], "unavailable");
    assert_eq!(error["retryable"], true);
    assert!(!error.to_string().contains(name));
    let (_, other) = register(&state, "other", false).await;
    assert_eq!(
        call(
            &state,
            "POST",
            "/v1/snapshots",
            Some(other["token"].as_str().unwrap()),
            json!(null)
        )
        .await
        .0,
        StatusCode::OK
    );
    call(
        &state,
        "DELETE",
        &format!("/v1/snapshots/{}", leases[0]),
        Some(token),
        json!(null),
    )
    .await;
    assert_eq!(
        call(&state, "POST", "/v1/snapshots", Some(token), json!(null))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_snapshot_preparation_is_bounded() {
    let state = state_with(support::wide_store(10_000), Duration::from_secs(30));
    let name = "preparation-flood-name";
    let (_, client) = register(&state, name, false).await;
    let token = client["token"].as_str().unwrap().to_owned();
    let requests: Vec<_> = (0..8)
        .map(|_| {
            let state = state.clone();
            let token = token.clone();
            tokio::spawn(async move {
                call(&state, "POST", "/v1/snapshots", Some(&token), json!(null)).await
            })
        })
        .collect();
    let mut accepted = 0;
    let mut saturated = 0;
    for request in requests {
        let (status, body) = request.await.unwrap();
        match status {
            StatusCode::OK => accepted += 1,
            StatusCode::SERVICE_UNAVAILABLE => {
                saturated += 1;
                assert_eq!(body["code"], "unavailable");
                assert_eq!(body["retryable"], true);
                assert!(!body.to_string().contains(name));
            }
            other => panic!("unexpected status {other}"),
        }
    }
    assert!(accepted >= 2, "only {accepted} preparations were accepted");
    assert!(saturated >= 1, "no preparation was refused");
    assert_eq!(
        call(&state, "POST", "/v1/snapshots", Some(&token), json!(null))
            .await
            .0,
        StatusCode::OK
    );
}
