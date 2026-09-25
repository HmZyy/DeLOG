use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::http::{Request, StatusCode, header};
use delog_core::snapshot::DataStore;
use delog_remote::routes::{RouterState, router};
use delog_remote::{ClientId, RemoteConfig, RemoteServices, SecretToken};
use futures_util::stream;
use serde_json::{Value, json};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

mod support;

const ENDPOINT: &str = "127.0.0.1:43123";

struct Fixture {
    state: Arc<RouterState>,
}

impl Fixture {
    fn new() -> Self {
        let state = RouterState::new(
            RemoteConfig {
                label: "security-stress".into(),
                loaded_file: None,
                lease_idle_timeout: Duration::from_secs(60),
                request_timeout: Duration::from_secs(2),
                max_concurrent_downloads: 2,
                discovery_root: None,
                control: delog_remote::ControlLimits::default(),
                uploads: delog_remote::UploadConfig::default(),
            },
            RemoteServices {
                store: Arc::new(DataStore::new()),
                ingest: delog_core::ingest::ingest_channel().0,
                control: Arc::new(support::RecordingControlHost::default()),
                owners: Arc::new(delog_remote::OwnerRegistry::new()),
            },
            ENDPOINT.parse().unwrap(),
        )
        .unwrap();
        Self { state }
    }

    fn bootstrap(&self) -> String {
        self.state.bootstrap_token().expose()
    }

    async fn send(&self, mut request: Request<Body>) -> (StatusCode, Value) {
        if !request.headers().contains_key(header::HOST) {
            request
                .headers_mut()
                .insert(header::HOST, ENDPOINT.parse().unwrap());
        }
        let response = router(Arc::clone(&self.state))
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
        (status, value)
    }

    async fn register(&self, name: &str) -> (String, ClientId) {
        let (status, value) = self
            .send(
                Request::builder()
                    .method("POST")
                    .uri("/v1/clients")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", self.bootstrap()),
                    )
                    .body(Body::from(json!({"name": name}).to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        (
            value["token"].as_str().unwrap().to_owned(),
            serde_json::from_value(value["client_id"].clone()).unwrap(),
        )
    }
}

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogCapture {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(Arc::clone(&self.0))
    }
}

impl LogCapture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn request(method: &str, uri: &str, token: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(body.into())
        .unwrap()
}

#[tokio::test]
async fn secrets_never_appear_in_errors_traces_or_debug_output() {
    let fixture = Fixture::new();
    let secret = fixture.bootstrap();
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (status, body) = fixture
        .send(request(
            "POST",
            "/v1/clients",
            &secret,
            format!(r#"{{"name":"broken","secret":"{secret}""#),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!body.to_string().contains(&secret));
    assert!(!capture.text().contains(&secret));
    assert!(!format!("{:?}", SecretToken::parse(&secret).unwrap()).contains(&secret));
}

#[tokio::test]
async fn authentication_and_boundary_inputs_fail_closed_without_registry_growth() {
    let fixture = Fixture::new();
    let before = fixture.state.status().clients.len();
    for index in 0..100 {
        let bogus = SecretToken::generate().unwrap().expose();
        let (status, _) = fixture
            .send(request("GET", "/v1/instance", &bogus, Body::empty()))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "attempt {index}");
    }
    assert_eq!(fixture.state.status().clients.len(), before);

    let huge = "x".repeat(128 * 1024);
    let (status, _) = fixture
        .send(
            Request::builder()
                .uri("/v1/instance")
                .header(header::AUTHORIZATION, huge)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let duplicate = Request::builder()
        .uri("/v1/instance")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", fixture.bootstrap()),
        )
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", fixture.bootstrap()),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(fixture.send(duplicate).await.0, StatusCode::FORBIDDEN);

    for authorization in ["Basic abc", "bearer abc", "Bearer", "Bearer "] {
        let invalid = Request::builder()
            .uri("/v1/instance")
            .header(header::AUTHORIZATION, authorization)
            .body(Body::empty())
            .unwrap();
        assert_eq!(fixture.send(invalid).await.0, StatusCode::FORBIDDEN);
    }

    for origin in ["null", "http://localhost", "https://127.0.0.1"] {
        let with_origin = Request::builder()
            .uri("/v1/instance")
            .header(header::ORIGIN, origin)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", fixture.bootstrap()),
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(fixture.send(with_origin).await.0, StatusCode::FORBIDDEN);
    }

    for host in ["localhost:43123", "127.0.0.1", "[::1]:43123"] {
        let wrong_host = Request::builder()
            .uri("/v1/instance")
            .header(header::HOST, host)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", fixture.bootstrap()),
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(fixture.send(wrong_host).await.0, StatusCode::FORBIDDEN);
    }

    let duplicate_host = Request::builder()
        .uri("/v1/instance")
        .header(header::HOST, ENDPOINT)
        .header(header::HOST, ENDPOINT)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", fixture.bootstrap()),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(fixture.send(duplicate_host).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn malformed_paths_queries_and_bodies_have_unique_sanitized_errors() {
    let fixture = Fixture::new();
    let (token, _) = fixture.register("security").await;
    let oversized = "x".repeat(65 * 1024);
    let (status, first) = fixture
        .send(request("POST", "/v1/snapshots", &token, oversized))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let cases = [
        "/v1/publications/%FF",
        "/v1/publications/%2E%2E",
        "/v1/publications/a%2Fb",
        "/v1/publications/safe?replace=%FF",
    ];
    let mut ids = vec![first["request_id"].as_str().unwrap().to_owned()];
    for uri in cases {
        let (status, error) = fixture
            .send(request("PUT", uri, &token, Body::empty()))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {error}");
        ids.push(error["request_id"].as_str().unwrap().to_owned());
        assert!(!error.to_string().contains(&token));
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), cases.len() + 1);
}

#[tokio::test]
async fn revocation_cancels_an_active_upload_without_publishing() {
    let fixture = Fixture::new();
    let (token, client) = fixture.register("revoked-upload").await;
    let (chunks, receiver) = tokio::sync::mpsc::channel::<Bytes>(1);
    let body = Body::from_stream(stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|chunk| (Ok::<_, std::io::Error>(chunk), receiver))
    }));
    let call = router(Arc::clone(&fixture.state)).oneshot(
        Request::builder()
            .method("PUT")
            .uri("/v1/publications/interrupted")
            .header(header::HOST, ENDPOINT)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/vnd.apache.arrow.stream")
            .header("idempotency-key", "revoked-upload")
            .body(body)
            .unwrap(),
    );
    let task = tokio::spawn(call);
    chunks
        .send(Bytes::from_static(b"partial-arrow-stream"))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while fixture.state.status().active_uploads == 0 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    assert!(fixture.state.revoke_client(&client));
    drop(chunks);

    let response = task.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(fixture.state.status().clients.is_empty());
    assert!(fixture.state.status().leases.is_empty());
}
