use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use arrow_ipc::writer::StreamWriter;
use axum::body::{Body, Bytes, to_bytes};
use axum::http::{Request, StatusCode, header};
use delog_core::ingest::ingest_channel;
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::snapshot::DataStore;
use delog_remote::routes::{RouterState, router};
use delog_remote::{
    ClientId, ControlLimits, ErrorEnvelope, IdempotencyAction, IdempotencyRegistry, OpaqueId,
    RemoteConfig, RemoteServices, RequestFingerprint, RequestStateDto, content_digest,
};
use serde_json::{Value, json};
use tower::ServiceExt;
mod support;

fn client(name: &str) -> ClientId {
    serde_json::from_value(json!(name)).unwrap()
}

fn fingerprint(body: &str) -> RequestFingerprint {
    RequestFingerprint::new("POST", "/v1/control", content_digest(body.as_bytes()))
}

fn registry() -> IdempotencyRegistry {
    IdempotencyRegistry::new(Duration::from_secs(60), 2)
}

fn execute(action: IdempotencyAction) -> delog_remote::RequestId {
    match action {
        IdempotencyAction::Execute(request) => request,
        other => panic!("expected execute, got {}", describe(&other)),
    }
}

fn describe(action: &IdempotencyAction) -> &'static str {
    match action {
        IdempotencyAction::Execute(_) => "execute",
        IdempotencyAction::Replay { .. } => "replay",
        IdempotencyAction::Wait(_) => "wait",
        IdempotencyAction::Unknown { .. } => "unknown",
    }
}

#[test]
fn keys_must_be_one_to_one_hundred_twenty_eight_visible_ascii_bytes() {
    let registry = registry();
    let now = Instant::now();
    let alpha = client("alpha");
    for bad in ["", "has space", "caf\u{e9}"] {
        let error = registry
            .begin(&alpha, bad, fingerprint("a"), now)
            .err()
            .unwrap();
        assert_eq!(error.code(), "invalid_input");
    }
    let long = "k".repeat(129);
    assert_eq!(
        registry
            .begin(&alpha, &long, fingerprint("a"), now)
            .err()
            .unwrap()
            .code(),
        "invalid_input"
    );
    execute(
        registry
            .begin(&alpha, &"k".repeat(128), fingerprint("a"), now)
            .unwrap(),
    );
}

#[test]
fn in_flight_requests_wait_and_completed_requests_replay_exactly() {
    let registry = registry();
    let now = Instant::now();
    let alpha = client("alpha");
    let request = execute(registry.begin(&alpha, "k", fingerprint("a"), now).unwrap());
    assert!(matches!(
        registry.begin(&alpha, "k", fingerprint("a"), now).unwrap(),
        IdempotencyAction::Wait(_)
    ));
    assert_eq!(
        registry
            .begin(&alpha, "k", fingerprint("b"), now)
            .err()
            .unwrap()
            .code(),
        "conflict"
    );
    let status = registry.get(&alpha, &request).unwrap();
    assert_eq!(status.state, RequestStateDto::InFlight);

    registry.complete(
        &request,
        StatusCode::CREATED,
        Bytes::from_static(b"{\"x\":1}"),
    );
    match registry.begin(&alpha, "k", fingerprint("a"), now).unwrap() {
        IdempotencyAction::Replay {
            request: replayed,
            status,
            body,
        } => {
            assert_eq!(replayed, request);
            assert_eq!(status, StatusCode::CREATED);
            assert_eq!(&body[..], b"{\"x\":1}");
        }
        other => panic!("expected replay, got {}", describe(&other)),
    }
    assert_eq!(
        registry
            .begin(&alpha, "k", fingerprint("b"), now)
            .err()
            .unwrap()
            .code(),
        "conflict"
    );
    let status = registry.get_by_key(&alpha, "k").unwrap();
    assert_eq!(status.state, RequestStateDto::Completed);
    assert_eq!(status.status, Some(201));
    assert_eq!(status.response, Some(json!({"x": 1})));
}

#[test]
fn unknown_and_abandoned_requests_are_recorded_differently() {
    let registry = registry();
    let now = Instant::now();
    let alpha = client("alpha");
    let request = execute(registry.begin(&alpha, "u", fingerprint("a"), now).unwrap());
    let error = ErrorEnvelope {
        request_id: OpaqueId::new(request.as_str()),
        code: "unavailable".into(),
        message: "no reply".into(),
        retryable: true,
        details: None,
        completion: Some(delog_remote::WireCompletion::Unknown),
    };
    registry.unknown(&request, error.clone());
    assert!(matches!(
        registry.begin(&alpha, "u", fingerprint("a"), now).unwrap(),
        IdempotencyAction::Unknown { .. }
    ));
    let status = registry.get(&alpha, &request).unwrap();
    assert_eq!(status.state, RequestStateDto::Unknown);
    assert_eq!(status.error, Some(error));

    let abandoned = execute(registry.begin(&alpha, "n", fingerprint("a"), now).unwrap());
    registry.abandon(&abandoned);
    assert_eq!(
        registry.get(&alpha, &abandoned).err().unwrap().code(),
        "not_found"
    );
    execute(registry.begin(&alpha, "n", fingerprint("b"), now).unwrap());
}

#[test]
fn records_are_scoped_per_client() {
    let registry = registry();
    let now = Instant::now();
    let alpha = client("alpha");
    let bravo = client("bravo");
    let request = execute(registry.begin(&alpha, "k", fingerprint("a"), now).unwrap());
    execute(registry.begin(&bravo, "k", fingerprint("b"), now).unwrap());
    assert_eq!(
        registry.get(&bravo, &request).err().unwrap().code(),
        "not_found"
    );
}

#[test]
fn records_expire_after_their_ttl() {
    let registry = registry();
    let start = Instant::now();
    let alpha = client("alpha");
    let request = execute(
        registry
            .begin(&alpha, "k", fingerprint("a"), start)
            .unwrap(),
    );
    registry.complete(&request, StatusCode::OK, Bytes::from_static(b"{}"));
    assert!(matches!(
        registry
            .begin(
                &alpha,
                "k",
                fingerprint("a"),
                start + Duration::from_secs(59)
            )
            .unwrap(),
        IdempotencyAction::Replay { .. }
    ));
    let later = start + Duration::from_secs(61);
    let fresh = execute(
        registry
            .begin(&alpha, "k", fingerprint("b"), later)
            .unwrap(),
    );
    assert_ne!(fresh, request);
    assert_eq!(
        registry.get(&alpha, &request).err().unwrap().code(),
        "not_found"
    );
}

#[test]
fn per_client_record_count_is_bounded_without_evicting_in_flight_work() {
    let registry = registry();
    let now = Instant::now();
    let alpha = client("alpha");
    let first = execute(registry.begin(&alpha, "1", fingerprint("a"), now).unwrap());
    registry.complete(&first, StatusCode::OK, Bytes::from_static(b"{}"));
    let second = execute(registry.begin(&alpha, "2", fingerprint("a"), now).unwrap());
    let third = execute(registry.begin(&alpha, "3", fingerprint("a"), now).unwrap());
    assert_eq!(
        registry.get(&alpha, &first).err().unwrap().code(),
        "not_found"
    );
    let saturated = registry
        .begin(&alpha, "4", fingerprint("a"), now)
        .err()
        .unwrap();
    assert_eq!(saturated.code(), "unavailable");
    assert_eq!(
        saturated.completion(),
        Some(delog_remote::WireCompletion::NotStarted)
    );
    registry.get(&alpha, &second).unwrap();
    registry.get(&alpha, &third).unwrap();
}

struct HeldWriter {
    ingestor: Ingestor<NullObserver>,
    receiver: delog_core::ingest::IngestReceiver,
}

struct PublicationFixture {
    state: Option<Arc<RouterState>>,
    ingest: Option<thread::JoinHandle<()>>,
    store: Arc<DataStore>,
}

impl PublicationFixture {
    fn new() -> Self {
        let (mut fixture, writer) = Self::with_held_writer(Duration::from_secs(5));
        fixture.start_writer(writer);
        fixture
    }

    fn with_held_writer(request_timeout: Duration) -> (Self, HeldWriter) {
        let ingestor = Ingestor::new(NullObserver);
        let store = ingestor.store();
        let (sender, receiver) = ingest_channel();
        let state = RouterState::new(
            RemoteConfig {
                label: "idempotency-test".into(),
                loaded_file: None,
                lease_idle_timeout: Duration::from_secs(60),
                request_timeout,
                max_concurrent_downloads: 2,
                discovery_root: None,
                control: ControlLimits::default(),
                uploads: delog_remote::UploadConfig::default(),
            },
            RemoteServices {
                store: Arc::clone(&store),
                ingest: sender,
                control: Arc::new(support::RecordingControlHost::default()),
                owners: Arc::new(delog_remote::OwnerRegistry::new()),
            },
            "127.0.0.1:12345".parse().unwrap(),
        )
        .unwrap();
        (
            Self {
                state: Some(state),
                ingest: None,
                store,
            },
            HeldWriter { ingestor, receiver },
        )
    }

    fn start_writer(&mut self, writer: HeldWriter) {
        let HeldWriter { ingestor, receiver } = writer;
        self.ingest = Some(thread::spawn(move || ingestor.run(receiver)));
    }

    fn state(&self) -> Arc<RouterState> {
        Arc::clone(self.state.as_ref().unwrap())
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        token: &str,
        key: Option<&str>,
        body: Body,
    ) -> (StatusCode, Bytes) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:12345")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/vnd.apache.arrow.stream");
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let response = router(self.state())
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        (
            status,
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
        )
    }

    async fn register(&self, name: &str) -> String {
        let token = self.state().bootstrap_token().expose();
        let (_, body) = self
            .call(
                "POST",
                "/v1/clients",
                &token,
                None,
                Body::from(json!({ "name": name }).to_string()),
            )
            .await;
        let value: Value = serde_json::from_slice(&body).unwrap();
        value["token"].as_str().unwrap().to_owned()
    }

    fn derived_sources(&self) -> usize {
        self.store
            .load()
            .sources
            .iter()
            .filter(|source| !source.entry.removed && source.entry.derived_provenance.is_some())
            .count()
    }
}

impl Drop for PublicationFixture {
    fn drop(&mut self) {
        self.state.take();
        if let Some(ingest) = self.ingest.take() {
            ingest.join().unwrap();
        }
    }
}

fn arrow(values: Vec<f64>) -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("__delog_time_ns", DataType::Int64, false),
        Field::new("error", DataType::Float64, false),
    ]));
    let times: Vec<i64> = (0..values.len() as i64).map(|i| i * 1_000).collect();
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(times)),
        Arc::new(Float64Array::from(values)),
    ];
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).unwrap();
    let mut bytes = Vec::new();
    {
        let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
        writer.write(&batch).unwrap();
        writer.finish().unwrap();
    }
    bytes
}

#[tokio::test]
async fn publication_retry_with_the_same_arrow_body_replays_the_first_commit() {
    let fixture = PublicationFixture::new();
    let token = fixture.register("diagnosis").await;
    let bytes = arrow(vec![1.0, 2.0]);
    let path = "/v1/publications/error?replace=false";
    let first = fixture
        .call(
            "PUT",
            path,
            &token,
            Some("upload"),
            Body::from(bytes.clone()),
        )
        .await;
    assert_eq!(first.0, StatusCode::CREATED);
    let second = fixture
        .call("PUT", path, &token, Some("upload"), Body::from(bytes))
        .await;
    assert_eq!(second, first);
    assert_eq!(fixture.derived_sources(), 1);

    let (status, _) = fixture
        .call("PUT", path, &token, None, Body::from(arrow(vec![3.0])))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn publication_key_reused_with_different_arrow_content_conflicts() {
    let fixture = PublicationFixture::new();
    let token = fixture.register("diagnosis").await;
    let path = "/v1/publications/error?replace=true";
    let (status, _) = fixture
        .call(
            "PUT",
            path,
            &token,
            Some("upload"),
            Body::from(arrow(vec![1.0])),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = fixture
        .call(
            "PUT",
            path,
            &token,
            Some("upload"),
            Body::from(arrow(vec![9.0])),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let error: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["code"], "conflict");
    let snapshot = fixture.store.load();
    let generation = snapshot
        .sources
        .iter()
        .find(|source| !source.entry.removed && source.entry.derived_provenance.is_some())
        .and_then(|source| source.entry.derived_provenance.as_ref())
        .map(|provenance| provenance.generation);
    assert_eq!(generation, Some(1));
}

#[tokio::test]
async fn publication_delete_requires_and_replays_its_key() {
    let fixture = PublicationFixture::new();
    let token = fixture.register("diagnosis").await;
    let (_, body) = fixture
        .call(
            "PUT",
            "/v1/publications/error",
            &token,
            Some("create"),
            Body::from(arrow(vec![1.0])),
        )
        .await;
    let created: Value = serde_json::from_slice(&body).unwrap();
    let path = format!("/v1/publications/{}", created["handle"].as_str().unwrap());
    let (status, _) = fixture
        .call("DELETE", &path, &token, None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let first = fixture
        .call("DELETE", &path, &token, Some("delete"), Body::empty())
        .await;
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(fixture.derived_sources(), 0);
    let second = fixture
        .call("DELETE", &path, &token, Some("delete"), Body::empty())
        .await;
    assert_eq!(second, first);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publication_receipt_timeout_is_unknown_and_a_same_key_retry_does_not_recommit() {
    let (mut fixture, writer) = PublicationFixture::with_held_writer(Duration::from_millis(400));
    let token = fixture.register("diagnosis").await;
    let bytes = arrow(vec![1.0, 2.0]);
    let path = "/v1/publications/error?replace=false";
    let first = fixture
        .call("PUT", path, &token, Some("slow"), Body::from(bytes.clone()))
        .await;
    assert_eq!(first.0, StatusCode::SERVICE_UNAVAILABLE);
    let error: Value = serde_json::from_slice(&first.1).unwrap();
    assert_eq!(error["completion"], "unknown");
    assert_eq!(fixture.derived_sources(), 0);

    fixture.start_writer(writer);
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.derived_sources() == 0 {
        assert!(Instant::now() < deadline, "late commit never landed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let retry = fixture
        .call("PUT", path, &token, Some("slow"), Body::from(bytes.clone()))
        .await;
    assert_eq!(retry, first);
    assert_eq!(fixture.derived_sources(), 1);

    let mut attempt = 0;
    loop {
        attempt += 1;
        let (status, _) = fixture
            .call(
                "PUT",
                path,
                &token,
                Some(&format!("probe-{attempt}")),
                Body::from(bytes.clone()),
            )
            .await;
        if status == StatusCode::CONFLICT {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "registry never adopted the late commit"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(fixture.derived_sources(), 1);

    let (status, body) = fixture
        .call(
            "PUT",
            "/v1/publications/error?replace=true",
            &token,
            Some("replace"),
            Body::from(arrow(vec![3.0])),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let replaced: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(replaced["generation"], 2);
    assert_eq!(fixture.derived_sources(), 1);
}

#[test]
fn unknown_records_are_never_evicted_before_their_ttl() {
    let registry = registry();
    let now = Instant::now();
    let alpha = client("alpha");
    let unknown = execute(registry.begin(&alpha, "u", fingerprint("a"), now).unwrap());
    registry.unknown(
        &unknown,
        ErrorEnvelope {
            request_id: OpaqueId::new(unknown.as_str()),
            code: "unavailable".into(),
            message: "no reply".into(),
            retryable: true,
            details: None,
            completion: Some(delog_remote::WireCompletion::Unknown),
        },
    );
    let completed = execute(registry.begin(&alpha, "c", fingerprint("a"), now).unwrap());
    registry.complete(&completed, StatusCode::OK, Bytes::from_static(b"{}"));

    execute(registry.begin(&alpha, "n", fingerprint("a"), now).unwrap());
    assert_eq!(
        registry.get(&alpha, &completed).err().unwrap().code(),
        "not_found"
    );
    let saturated = registry
        .begin(&alpha, "m", fingerprint("a"), now)
        .err()
        .unwrap();
    assert_eq!(saturated.code(), "unavailable");
    assert_eq!(
        saturated.completion(),
        Some(delog_remote::WireCompletion::NotStarted)
    );
    assert!(matches!(
        registry.begin(&alpha, "u", fingerprint("a"), now).unwrap(),
        IdempotencyAction::Unknown { .. }
    ));
}
