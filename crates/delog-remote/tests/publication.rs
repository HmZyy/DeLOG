use std::sync::Arc;
use std::thread;
use std::time::Duration;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use arrow_ipc::writer::StreamWriter;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use delog_core::ingest::ingest_channel;
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::snapshot::DataStore;
use delog_remote::routes::{RouterState, router};
use delog_remote::{RemoteConfig, RemoteServices, SecretToken};
use serde_json::{Value, json};
use tower::ServiceExt;
mod support;

static NEXT_KEY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct Fixture {
    state: Option<Arc<RouterState>>,
    store: Arc<DataStore>,
    ingest: Option<thread::JoinHandle<()>>,
}

impl Fixture {
    fn new() -> Self {
        let ingestor = Ingestor::new(NullObserver);
        let store = ingestor.store();
        let (sender, receiver) = ingest_channel();
        let ingest = thread::spawn(move || ingestor.run(receiver));
        let state = RouterState::new(
            RemoteConfig {
                label: "publication-test".into(),
                loaded_file: None,
                lease_idle_timeout: Duration::from_secs(60),
                request_timeout: Duration::from_secs(2),
                max_concurrent_downloads: 2,
                discovery_root: None,
                control: delog_remote::ControlLimits::default(),
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
        Self {
            state: Some(state),
            store,
            ingest: Some(ingest),
        }
    }

    fn state(&self) -> Arc<RouterState> {
        Arc::clone(self.state.as_ref().unwrap())
    }

    async fn register(&self, name: &str) -> String {
        let token = self.state().bootstrap_token().expose();
        let (_, value) = self
            .call(
                "POST",
                "/v1/clients",
                &token,
                "application/json",
                Body::from(json!({"name": name}).to_string()),
            )
            .await;
        value["token"].as_str().unwrap().to_owned()
    }

    async fn put(
        &self,
        token: &str,
        topic: &str,
        bytes: Vec<u8>,
        replace: bool,
    ) -> (StatusCode, Value) {
        self.call(
            "PUT",
            &format!("/v1/publications/{topic}?replace={replace}"),
            token,
            "application/vnd.apache.arrow.stream",
            Body::from(bytes),
        )
        .await
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        token: &str,
        content_type: &str,
        body: Body,
    ) -> (StatusCode, Value) {
        let response = router(self.state())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::HOST, "127.0.0.1:12345")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, content_type)
                    .header(
                        "idempotency-key",
                        format!(
                            "key-{}",
                            NEXT_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        ),
                    )
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
        (status, value)
    }

    fn values_for(&self, owner: &str, logical_topic: &str) -> Vec<f64> {
        let snapshot = self.store.load();
        let source = snapshot
            .sources
            .iter()
            .find(|source| {
                !source.entry.removed
                    && source
                        .entry
                        .derived_provenance
                        .as_ref()
                        .is_some_and(|provenance| {
                            provenance.owner == owner && provenance.logical_topic == logical_topic
                        })
            })
            .unwrap();
        let topic = source
            .topics
            .iter()
            .find_map(|topic| snapshot.topic_store(*topic))
            .unwrap();
        topic.chunks[0].cols[0]
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap()
            .values()
            .to_vec()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.take();
        if let Some(ingest) = self.ingest.take() {
            ingest.join().unwrap();
        }
    }
}

fn arrow(times: Vec<Option<i64>>, values: Vec<f64>) -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("__delog_time_ns", DataType::Int64, true),
        Field::new("error", DataType::Float64, false),
    ]));
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
async fn failed_replace_leaves_old_publication_visible() {
    let fixture = Fixture::new();
    let token = fixture.register("diagnosis").await;
    let (status, _) = fixture
        .put(
            &token,
            "attitude_error",
            arrow(vec![Some(1_000)], vec![1.0]),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, error) = fixture
        .put(&token, "attitude_error", arrow(vec![None], vec![2.0]), true)
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["code"], "invalid_input");
    assert_eq!(
        fixture.values_for("external/diagnosis", "attitude_error"),
        vec![1.0]
    );
}

#[tokio::test]
async fn replace_false_conflicts_and_successful_replace_mints_new_handles() {
    let fixture = Fixture::new();
    let token = fixture.register("diagnosis").await;
    let (status, first) = fixture
        .put(
            &token,
            "attitude_error",
            arrow(vec![Some(1_000)], vec![1.0]),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = fixture
        .put(
            &token,
            "attitude_error",
            arrow(vec![Some(2_000)], vec![2.0]),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, second) = fixture
        .put(
            &token,
            "attitude_error",
            arrow(vec![Some(2_000)], vec![2.0]),
            true,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(first["handle"], second["handle"]);
    assert_ne!(first["topic"]["handle"], second["topic"]["handle"]);
    assert_ne!(
        first["topic"]["fields"][0]["handle"],
        second["topic"]["fields"][0]["handle"]
    );
    assert_eq!(
        fixture.values_for("external/diagnosis", "attitude_error"),
        vec![2.0]
    );
}

#[tokio::test]
async fn publications_and_deletion_are_isolated_by_authenticated_owner() {
    let fixture = Fixture::new();
    let alpha = fixture.register("alpha").await;
    let bravo = fixture.register("bravo").await;
    let (_, alpha_publication) = fixture
        .put(&alpha, "error", arrow(vec![Some(1_000)], vec![1.0]), false)
        .await;
    let (_, bravo_publication) = fixture
        .put(&bravo, "error", arrow(vec![Some(1_000)], vec![2.0]), false)
        .await;
    let alpha_handle = alpha_publication["handle"].as_str().unwrap();

    let (status, _) = fixture
        .call(
            "DELETE",
            &format!("/v1/publications/{alpha_handle}"),
            &bravo,
            "application/json",
            Body::empty(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = fixture
        .call(
            "DELETE",
            &format!("/v1/publications/{alpha_handle}"),
            &alpha,
            "application/json",
            Body::empty(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fixture.values_for("external/bravo", "error"), vec![2.0]);
    assert!(bravo_publication["handle"].is_string());
    assert_eq!(
        fixture
            .store
            .load()
            .sources
            .iter()
            .filter(|source| !source.entry.removed && source.entry.derived_provenance.is_some())
            .count(),
        1
    );
}

#[tokio::test]
async fn publication_requires_arrow_mime_and_client_authentication() {
    let fixture = Fixture::new();
    let token = fixture.register("diagnosis").await;
    let bytes = arrow(vec![Some(1_000)], vec![1.0]);
    let (status, _) = fixture
        .call(
            "PUT",
            "/v1/publications/error?replace=false",
            &token,
            "application/json",
            Body::from(bytes.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let unknown = SecretToken::generate().unwrap().expose();
    let (status, _) = fixture
        .call(
            "PUT",
            "/v1/publications/error?replace=false",
            &unknown,
            "application/vnd.apache.arrow.stream",
            Body::from(bytes),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn topic_path_segment_is_percent_decoded() {
    let fixture = Fixture::new();
    let token = fixture.register("diagnosis").await;
    let (status, value) = fixture
        .put(
            &token,
            "attitude%20error%5B0%5D",
            arrow(vec![Some(1_000)], vec![4.0]),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    assert_eq!(value["topic"]["name"], "attitude error[0]");
    assert_eq!(
        fixture.values_for("external/diagnosis", "attitude error[0]"),
        vec![4.0]
    );
    for segment in [
        "a%2Fb", "%2f", ".", "..", "%2E", "%2e%2E", "%FF", "%G1", "abc%4", "%",
    ] {
        let (status, value) = fixture
            .put(&token, segment, arrow(vec![Some(1_000)], vec![1.0]), false)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{segment}: {value}");
        assert_eq!(value["code"], "invalid_input", "{segment}");
    }
}
