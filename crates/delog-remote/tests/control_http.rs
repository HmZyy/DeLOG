use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::http::{Request, StatusCode, header};
use delog_api::control::{
    AuthorizedControlHost, ControlPrincipal, ControlRequest, ControlResponse, MarkerInfo,
    MarkerOrigin, MarkerRequest, PlaybackInfo, PlaybackRequest, PlotInfo, WorkspaceInfo,
    WorkspaceRequest,
};
use delog_api::{Error, MutationCompletion};
use delog_core::snapshot::DataStore;
use delog_remote::routes::{RouterState, router};
use delog_remote::{ControlLimits, RemoteConfig, RemoteServices};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Default)]
struct Gate {
    closed: Mutex<bool>,
    changed: Condvar,
}

#[derive(Default)]
struct ScriptedHost {
    calls: Mutex<Vec<ControlRequest>>,
    principals: Mutex<Vec<ControlPrincipal>>,
    failure: Mutex<Option<Error>>,
    delay: Mutex<Duration>,
    gate: Gate,
}

impl ScriptedHost {
    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn requests(&self) -> Vec<ControlRequest> {
        self.calls.lock().unwrap().clone()
    }

    fn fail_with(&self, error: Error) {
        *self.failure.lock().unwrap() = Some(error);
    }

    fn close_gate(&self) {
        *self.gate.closed.lock().unwrap() = true;
    }

    fn open_gate(&self) {
        *self.gate.closed.lock().unwrap() = false;
        self.gate.changed.notify_all();
    }
}

impl AuthorizedControlHost for ScriptedHost {
    fn call_as(
        &self,
        principal: ControlPrincipal,
        request: ControlRequest,
    ) -> delog_api::Result<ControlResponse> {
        self.calls.lock().unwrap().push(request.clone());
        self.principals.lock().unwrap().push(principal);
        let mut closed = self.gate.closed.lock().unwrap();
        while *closed {
            closed = self.gate.changed.wait(closed).unwrap();
        }
        drop(closed);
        let delay = *self.delay.lock().unwrap();
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        if let Some(error) = self.failure.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(match request {
            ControlRequest::Markers(MarkerRequest::AppendReturning { marker, owner, .. }) => {
                ControlResponse::Marker(MarkerInfo {
                    id: 41,
                    index: 0,
                    t_us: marker.time_us,
                    label: marker.label,
                    color: [1.0; 4],
                    note: String::new(),
                    origin: MarkerOrigin::Script,
                    owner: Some(owner),
                })
            }
            ControlRequest::Workspace(WorkspaceRequest::AddPlot { .. }) => {
                ControlResponse::Plots(vec![PlotInfo {
                    window: 0,
                    tile: 3,
                    instance_id: 9,
                    index: 0,
                    label: "plot".into(),
                    owner: None,
                }])
            }
            ControlRequest::Workspace(WorkspaceRequest::ListWindows) => {
                ControlResponse::Windows(vec![])
            }
            ControlRequest::Workspace(WorkspaceRequest::GetState) => {
                ControlResponse::Workspace(WorkspaceInfo {
                    scene_visible: true,
                })
            }
            ControlRequest::Playback(PlaybackRequest::Get) => {
                ControlResponse::Playback(PlaybackInfo {
                    speed: 1.5,
                    follow_live: false,
                })
            }
            ControlRequest::Plots(_) => ControlResponse::Plots(vec![]),
            ControlRequest::Annotations(_) => ControlResponse::Annotations(vec![]),
            ControlRequest::Markers(MarkerRequest::List) => ControlResponse::Markers(vec![]),
            ControlRequest::Vehicles(_) => ControlResponse::Vehicles(vec![]),
            ControlRequest::Layouts(delog_api::control::LayoutRequest::List) => {
                ControlResponse::Names(vec!["default".into()])
            }
            ControlRequest::Layouts(delog_api::control::LayoutRequest::Current) => {
                ControlResponse::Layout("{}".into())
            }
            _ => ControlResponse::Unit,
        })
    }
}

struct TestResponse {
    status: StatusCode,
    request_id: Option<String>,
    body: Bytes,
}

impl TestResponse {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

struct ControlHttpFixture {
    state: Arc<RouterState>,
    host: Arc<ScriptedHost>,
    token: String,
}

impl ControlHttpFixture {
    fn new() -> Self {
        Self::with_limits(ControlLimits::default())
    }

    fn with_limits(control: ControlLimits) -> Self {
        let host = Arc::new(ScriptedHost::default());
        let state = RouterState::new(
            RemoteConfig {
                label: "control-test".into(),
                loaded_file: None,
                lease_idle_timeout: Duration::from_secs(60),
                request_timeout: Duration::from_secs(5),
                max_concurrent_downloads: 2,
                discovery_root: None,
                control,
                uploads: delog_remote::UploadConfig::default(),
            },
            RemoteServices {
                store: Arc::new(DataStore::new()),
                ingest: delog_core::ingest::ingest_channel().0,
                control: host.clone(),
                owners: Arc::new(delog_remote::OwnerRegistry::new()),
            },
            "127.0.0.1:12345".parse().unwrap(),
        )
        .unwrap();
        let token = futures_util::FutureExt::now_or_never(register(&state, "diagnosis"))
            .expect("registration completes without waiting");
        Self { state, host, token }
    }

    async fn register(&self, name: &str) -> String {
        register(&self.state, name).await
    }

    async fn post_with_key(&self, key: &str, body: Value) -> TestResponse {
        self.send(&self.token, "POST", "/v1/control", Some(key), body)
            .await
    }

    async fn send(
        &self,
        token: &str,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Value,
    ) -> TestResponse {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:12345")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let body = if body.is_null() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        };
        let response = router(self.state.clone())
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let request_id = response
            .headers()
            .get("x-request-id")
            .map(|value| value.to_str().unwrap().to_owned());
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        TestResponse {
            status,
            request_id,
            body,
        }
    }
}

async fn register(state: &Arc<RouterState>, name: &str) -> String {
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/clients")
                .header(header::HOST, "127.0.0.1:12345")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", state.bootstrap_token().expose()),
                )
                .body(Body::from(json!({ "name": name }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    value["token"].as_str().unwrap().to_owned()
}

fn marker_add() -> Value {
    json!({"op": "marker_add", "time_ns": 5_000, "label": "failure"})
}

fn playback(speed: f64) -> Value {
    json!({"op": "playback_set", "speed": speed})
}

#[tokio::test]
async fn concurrent_same_key_executes_one_mutation_and_replays_one_result() {
    let fixture = ControlHttpFixture::new();
    *fixture.host.delay.lock().unwrap() = Duration::from_millis(100);
    let (a, b) = tokio::join!(
        fixture.post_with_key("same-key", marker_add()),
        fixture.post_with_key("same-key", marker_add()),
    );
    assert_eq!(a.status, StatusCode::OK);
    assert_eq!(a.json(), b.json());
    assert_eq!(a.request_id, b.request_id);
    assert_eq!(fixture.host.calls(), 1);
}

#[tokio::test]
async fn replay_after_success_returns_the_exact_stored_response() {
    let fixture = ControlHttpFixture::new();
    let first = fixture.post_with_key("once", marker_add()).await;
    let second = fixture.post_with_key("once", marker_add()).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.json()["kind"], "resource");
    assert_eq!(second.status, first.status);
    assert_eq!(second.body, first.body);
    assert!(first.request_id.is_some());
    assert_eq!(second.request_id, first.request_id);
    assert_eq!(fixture.host.calls(), 1);
    let principals = fixture.host.principals.lock().unwrap();
    assert_eq!(principals[0].owner.name, "external/diagnosis");
    assert_eq!(principals[0].access, delog_api::control::AccessMode::Safe);
}

#[tokio::test]
async fn same_key_with_a_different_body_conflicts_without_executing() {
    let fixture = ControlHttpFixture::new();
    let first = fixture.post_with_key("reused", playback(1.0)).await;
    assert_eq!(first.status, StatusCode::OK);
    let second = fixture.post_with_key("reused", playback(2.0)).await;
    assert_eq!(second.status, StatusCode::CONFLICT);
    assert_eq!(second.json()["code"], "conflict");
    assert_eq!(fixture.host.calls(), 1);
}

#[tokio::test]
async fn replay_after_typed_failure_does_not_repeat_the_call() {
    let fixture = ControlHttpFixture::new();
    fixture
        .host
        .fail_with(Error::forbidden("safe mode forbids this"));
    let first = fixture.post_with_key("denied", playback(3.0)).await;
    assert_eq!(first.status, StatusCode::FORBIDDEN);
    assert_eq!(first.json()["code"], "forbidden");
    assert_eq!(
        first.json()["request_id"].as_str(),
        first.request_id.as_deref()
    );
    *fixture.host.failure.lock().unwrap() = None;
    let second = fixture.post_with_key("denied", playback(3.0)).await;
    assert_eq!(second.status, StatusCode::FORBIDDEN);
    assert_eq!(second.body, first.body);
    assert_eq!(fixture.host.calls(), 1);
}

#[tokio::test]
async fn unknown_completion_is_queryable_by_request_id_and_key() {
    let fixture = ControlHttpFixture::new();
    fixture.host.fail_with(
        Error::unavailable("the DeLOG window did not respond")
            .with_completion(MutationCompletion::Unknown),
    );
    let first = fixture.post_with_key("uncertain", playback(4.0)).await;
    assert_eq!(first.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(first.json()["completion"], "unknown");
    let request_id = first.request_id.clone().unwrap();
    assert_eq!(first.json()["request_id"], request_id.as_str());

    let token = fixture.token.clone();
    let by_id = fixture
        .send(
            &token,
            "GET",
            &format!("/v1/requests/{request_id}"),
            None,
            Value::Null,
        )
        .await;
    assert_eq!(by_id.status, StatusCode::OK);
    assert_eq!(by_id.json()["state"], "unknown");
    assert_eq!(by_id.json()["request_id"], request_id.as_str());
    assert_eq!(by_id.json()["error"]["completion"], "unknown");

    let by_key = fixture
        .send(
            &token,
            "GET",
            "/v1/requests/by-key",
            Some("uncertain"),
            Value::Null,
        )
        .await;
    assert_eq!(by_key.status, StatusCode::OK);
    assert_eq!(by_key.json(), by_id.json());

    *fixture.host.failure.lock().unwrap() = None;
    let retry = fixture.post_with_key("uncertain", playback(4.0)).await;
    assert_eq!(retry.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(retry.json()["completion"], "unknown");
    assert_eq!(fixture.host.calls(), 1);
}

#[tokio::test]
async fn completed_requests_report_their_stored_response() {
    let fixture = ControlHttpFixture::new();
    let first = fixture.post_with_key("done", playback(1.0)).await;
    let token = fixture.token.clone();
    let status = fixture
        .send(
            &token,
            "GET",
            "/v1/requests/by-key",
            Some("done"),
            Value::Null,
        )
        .await;
    assert_eq!(status.status, StatusCode::OK);
    assert_eq!(status.json()["state"], "completed");
    assert_eq!(status.json()["status"], 200);
    assert_eq!(status.json()["response"], first.json());
    let missing = fixture
        .send(
            &token,
            "GET",
            "/v1/requests/by-key",
            Some("never-used"),
            Value::Null,
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn idempotency_keys_and_request_lookups_are_scoped_per_client() {
    let fixture = ControlHttpFixture::new();
    let other = fixture.register("other").await;
    let token = fixture.token.clone();
    let mine = fixture
        .send(&token, "POST", "/v1/control", Some("shared"), playback(1.0))
        .await;
    let theirs = fixture
        .send(&other, "POST", "/v1/control", Some("shared"), playback(2.0))
        .await;
    assert_eq!(mine.status, StatusCode::OK);
    assert_eq!(theirs.status, StatusCode::OK);
    assert_ne!(mine.request_id, theirs.request_id);
    assert_eq!(fixture.host.calls(), 2);

    let foreign = fixture
        .send(
            &other,
            "GET",
            &format!("/v1/requests/{}", mine.request_id.unwrap()),
            None,
            Value::Null,
        )
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mutations_require_a_bounded_idempotency_key() {
    let fixture = ControlHttpFixture::new();
    let token = fixture.token.clone();
    let missing = fixture
        .send(&token, "POST", "/v1/control", None, playback(1.0))
        .await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(missing.json()["code"], "invalid_input");
    let long = "k".repeat(129);
    let too_long = fixture.post_with_key(&long, playback(1.0)).await;
    assert_eq!(too_long.status, StatusCode::BAD_REQUEST);
    let longest = "k".repeat(128);
    assert_eq!(
        fixture.post_with_key(&longest, playback(1.0)).await.status,
        StatusCode::OK
    );
    let batch = fixture
        .send(
            &token,
            "POST",
            "/v1/control/batch",
            None,
            json!({"commands": [playback(1.0)]}),
        )
        .await;
    assert_eq!(batch.status, StatusCode::BAD_REQUEST);
    assert_eq!(fixture.host.calls(), 1);
}

#[tokio::test]
async fn control_state_is_a_query_without_an_idempotency_key() {
    let fixture = ControlHttpFixture::new();
    let token = fixture.token.clone();
    let state = fixture
        .send(&token, "GET", "/v1/control/state", None, Value::Null)
        .await;
    assert_eq!(state.status, StatusCode::OK);
    assert_eq!(state.json()["playback"]["speed"], 1.5);
    assert_eq!(state.json()["layout_names"], json!(["default"]));
}

#[tokio::test]
async fn batch_applies_one_native_batch_request() {
    let fixture = ControlHttpFixture::new();
    let token = fixture.token.clone();
    let response = fixture
        .send(
            &token,
            "POST",
            "/v1/control/batch",
            Some("batch"),
            json!({"commands": [
                playback(2.0),
                {"op": "scene_set_visible", "visible": false},
                {"op": "workspace_equalize"},
            ]}),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json()["kind"], "unit");
    let requests = fixture.host.requests();
    assert_eq!(requests.len(), 1);
    let ControlRequest::Batch(inner) = &requests[0] else {
        panic!("expected a native batch, got {:?}", requests[0]);
    };
    assert_eq!(inner.len(), 3);
    assert!(matches!(
        inner[0],
        ControlRequest::Playback(PlaybackRequest::Set { .. })
    ));
}

#[tokio::test]
async fn non_batchable_commands_are_rejected_before_queueing() {
    let fixture = ControlHttpFixture::new();
    let token = fixture.token.clone();
    let handle_producing = fixture
        .send(
            &token,
            "POST",
            "/v1/control/batch",
            Some("batch-handle"),
            json!({"commands": [playback(2.0), {"op": "window_open"}]}),
        )
        .await;
    assert_eq!(handle_producing.status, StatusCode::BAD_REQUEST);
    assert_eq!(handle_producing.json()["code"], "invalid_input");
    assert_eq!(fixture.host.calls(), 0);

    let plot = fixture
        .post_with_key(
            "plot",
            json!({"op": "workspace_add_plot", "direction": "horizontal"}),
        )
        .await;
    let plot = plot.json()["handle"].as_str().unwrap().to_owned();
    assert_eq!(fixture.host.calls(), 1);
    let guarded = fixture
        .send(
            &token,
            "POST",
            "/v1/control/batch",
            Some("batch-guarded"),
            json!({"commands": [{"op": "trace_clear", "plot": plot}]}),
        )
        .await;
    assert_eq!(guarded.status, StatusCode::BAD_REQUEST);
    assert_eq!(guarded.json()["code"], "invalid_input");
    assert_eq!(fixture.host.calls(), 1);

    let empty = fixture
        .send(
            &token,
            "POST",
            "/v1/control/batch",
            Some("batch-empty"),
            json!({"commands": []}),
        )
        .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    assert_eq!(fixture.host.calls(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_saturation_returns_not_started_and_the_key_stays_usable() {
    let fixture = Arc::new(ControlHttpFixture::with_limits(ControlLimits {
        max_queued: 1,
        ..ControlLimits::default()
    }));
    fixture.host.close_gate();
    let blocked = {
        let fixture = Arc::clone(&fixture);
        tokio::spawn(async move { fixture.post_with_key("first", playback(1.0)).await })
    };
    while fixture.host.calls() == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let saturated = fixture.post_with_key("second", playback(2.0)).await;
    assert_eq!(saturated.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(saturated.json()["code"], "unavailable");
    assert_eq!(saturated.json()["completion"], "not_started");
    assert_eq!(saturated.json()["retryable"], true);
    assert_eq!(fixture.host.calls(), 1);

    fixture.host.open_gate();
    assert_eq!(blocked.await.unwrap().status, StatusCode::OK);
    let retried = fixture.post_with_key("second", playback(2.0)).await;
    assert_eq!(retried.status, StatusCode::OK);
    assert_eq!(fixture.host.calls(), 2);
}

#[tokio::test]
async fn native_not_started_failure_is_retryable_with_the_same_key() {
    let fixture = ControlHttpFixture::new();
    fixture.host.fail_with(
        Error::unavailable("the DeLOG control queue is full")
            .with_completion(MutationCompletion::NotStarted),
    );
    let first = fixture.post_with_key("retry", playback(1.0)).await;
    assert_eq!(first.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(first.json()["completion"], "not_started");
    *fixture.host.failure.lock().unwrap() = None;
    let second = fixture.post_with_key("retry", playback(1.0)).await;
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(fixture.host.calls(), 2);
}

#[tokio::test]
async fn external_principals_are_namespaced_apart_from_script_owner_names() {
    let fixture = ControlHttpFixture::new();
    let script_named = fixture.register("flight-diagnosis").await;
    let response = fixture
        .send(
            &script_named,
            "POST",
            "/v1/control",
            Some("marker"),
            marker_add(),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    let principal = fixture.host.principals.lock().unwrap()[0].clone();
    assert_eq!(principal.owner.name, "external/flight-diagnosis");
    assert_ne!(principal.owner.name, "flight-diagnosis");
    let ControlRequest::Markers(MarkerRequest::AppendReturning { owner, .. }) =
        &fixture.host.requests()[0]
    else {
        panic!("expected a marker append");
    };
    assert_eq!(owner, "external/flight-diagnosis");
}
