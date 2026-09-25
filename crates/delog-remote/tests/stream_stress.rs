use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use arrow_ipc::writer::StreamWriter;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use bytes::Bytes;
use delog_core::snapshot::DataStore;
use delog_remote::routes::{RouterState, router};
use delog_remote::{
    ClientId, LeaseId, LeaseManager, ReadQuery, RemoteConfig, RemoteServer, TopicBatchIter,
    UploadConfig, UploadLimits, arrow_body_with_stats, decode_staged_upload, stage_body,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

mod support;

const ENDPOINT: &str = "127.0.0.1:43124";
const CHUNKS: usize = 80;
const ROWS_PER_CHUNK: usize = 62_500;
const TOTAL_ROWS: usize = CHUNKS * ROWS_PER_CHUNK;
const _: () = assert!(TOTAL_ROWS >= 5_000_000);

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn five_million_row_stream_stays_bounded_and_cancels_cleanly() {
    let store = support::store(CHUNKS, ROWS_PER_CHUNK);
    let leases = LeaseManager::new(Duration::from_secs(60));
    let client: ClientId = serde_json::from_value(json!("stress-client")).unwrap();
    let prepared = LeaseManager::prepare(store.load()).unwrap();
    let lease = leases
        .insert(prepared, client.clone(), Instant::now())
        .unwrap();
    let guard = leases.read(&lease.id, &client, Instant::now()).unwrap();
    let topic_handle = guard.catalog().sources[0].topics[0].handle.clone();
    let topic = guard.topic_id(&topic_handle).unwrap();
    let iter = TopicBatchIter::new(guard, topic, ReadQuery::default()).unwrap();
    let (body, stats) = arrow_body_with_stats(iter);
    let mut stream = body.into_data_stream();

    let first = stream.next().await.unwrap().unwrap();
    assert!(!first.is_empty());
    wait_until("the bounded Arrow queue to fill", || {
        stats.queued_chunks() == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(stats.max_queued_chunks(), 2);
    assert!(stats.max_queued_bytes() > 0);
    assert_eq!(stats.queued_chunks(), 2);

    leases.close(&lease.id, &client).unwrap();
    wait_until("the cancelled Arrow producer to exit", || {
        stats.is_finished()
    })
    .await;
    assert!(!stats.is_complete());
    assert!(leases.list_active().is_empty());
}

async fn json_call(
    state: &Arc<RouterState>,
    method: &str,
    path: &str,
    token: &str,
) -> (StatusCode, Value) {
    let response = router(Arc::clone(state))
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, ENDPOINT)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_http_download_releases_its_slot_and_lease() {
    let state = RouterState::new(
        RemoteConfig {
            label: "stream-stress".into(),
            loaded_file: None,
            lease_idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(3),
            max_concurrent_downloads: 1,
            discovery_root: None,
            control: delog_remote::ControlLimits::default(),
            uploads: delog_remote::UploadConfig::default(),
        },
        support::services(support::store(64, 4096)),
        ENDPOINT.parse().unwrap(),
    )
    .unwrap();
    let bootstrap = state.bootstrap_token().expose();
    let (status, registered) = {
        let response = router(Arc::clone(&state))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/clients")
                    .header(header::HOST, ENDPOINT)
                    .header(header::AUTHORIZATION, format!("Bearer {bootstrap}"))
                    .body(Body::from(json!({"name": "stream-stress"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice::<Value>(&bytes).unwrap())
    };
    assert_eq!(status, StatusCode::OK);
    let token = registered["token"].as_str().unwrap();
    let (_, snapshot) = json_call(&state, "POST", "/v1/snapshots", token).await;
    let lease: LeaseId = serde_json::from_value(snapshot["lease_id"].clone()).unwrap();
    let (_, catalog) = json_call(
        &state,
        "GET",
        &format!("/v1/snapshots/{lease}/catalog"),
        token,
    )
    .await;
    let topic = catalog["sources"][0]["topics"][0]["handle"]
        .as_str()
        .unwrap();
    let response = router(Arc::clone(&state))
        .oneshot(
            Request::builder()
                .uri(format!("/v1/snapshots/{lease}/topics/{topic}/data"))
                .header(header::HOST, ENDPOINT)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    assert!(!stream.next().await.unwrap().unwrap().is_empty());
    wait_until("the download slot to be held", || {
        state.status().active_downloads == 1
    })
    .await;

    assert!(state.revoke_lease(&lease));
    wait_until("the download slot to be released", || {
        state.status().active_downloads == 0
    })
    .await;
    assert!(state.status().leases.is_empty());
}

fn arrow_upload(rows: usize) -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("__delog_time_ns", DataType::Int64, false),
        Field::new("value", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from_iter_values(
                (0..rows).map(|row| row as i64 * 1_000),
            )) as ArrayRef,
            Arc::new(Float64Array::from(vec![1.0; rows])) as ArrayRef,
        ],
    )
    .unwrap();
    let mut bytes = Vec::new();
    {
        let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
        writer.write(&batch).unwrap();
        writer.finish().unwrap();
    }
    bytes
}

fn staged_bytes(root: &std::path::Path) -> u64 {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum()
}

#[tokio::test]
async fn upload_staging_is_bounded_and_every_failure_cleans_the_file() {
    let root = tempfile::tempdir().unwrap();
    let limits = UploadLimits {
        max_bytes: 4 * 1024 * 1024,
        max_rows: 10,
        max_fields: 4,
    };
    let cancellation = CancellationToken::new();
    let (sender, receiver) = tokio::sync::mpsc::channel::<Bytes>(2);
    assert_eq!(sender.max_capacity(), 2);
    let body = Body::from_stream(futures_util::stream::unfold(
        receiver,
        |mut receiver| async move {
            receiver
                .recv()
                .await
                .map(|chunk| (Ok::<_, std::io::Error>(chunk), receiver))
        },
    ));
    let stage_root = root.path().to_owned();
    let stage_cancel = cancellation.clone();
    let staged =
        tokio::spawn(async move { stage_body(body, limits, &stage_root, stage_cancel).await });
    let chunk = Bytes::from(vec![7_u8; 256 * 1024]);
    for _ in 0..4 {
        sender.send(chunk.clone()).await.unwrap();
    }
    wait_until("staged upload bytes", || {
        staged_bytes(root.path()) >= chunk.len() as u64
    })
    .await;
    assert!(sender.capacity() <= sender.max_capacity());
    cancellation.cancel();
    drop(sender);
    assert_eq!(staged.await.unwrap().unwrap_err().code(), "unavailable");
    assert_eq!(staged_bytes(root.path()), 0);

    let error = stage_body(
        Body::from(vec![0_u8; 1025]),
        UploadLimits {
            max_bytes: 1024,
            ..limits
        },
        root.path(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "invalid_input");
    assert_eq!(staged_bytes(root.path()), 0);

    let staged = stage_body(
        Body::from(arrow_upload(3)),
        limits,
        root.path(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let error = decode_staged_upload(
        staged,
        "too-many-rows",
        UploadLimits {
            max_rows: 2,
            ..limits
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "invalid_input");
    assert_eq!(staged_bytes(root.path()), 0);
}

struct SlowUpload {
    remaining: usize,
}

impl Read for SlowUpload {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        std::thread::sleep(Duration::from_millis(5));
        let count = buffer.len().min(16 * 1024).min(self.remaining);
        buffer[..count].fill(3);
        self.remaining -= count;
        Ok(count)
    }
}

#[test]
fn shutdown_cancels_upload_staging_without_publishing() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(DataStore::new());
    let server = RemoteServer::spawn(
        RemoteConfig {
            label: "upload-shutdown".into(),
            loaded_file: None,
            lease_idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(2),
            max_concurrent_downloads: 1,
            discovery_root: Some(root.path().to_owned()),
            control: delog_remote::ControlLimits::default(),
            uploads: UploadConfig {
                limits: UploadLimits {
                    max_bytes: 64 * 1024 * 1024,
                    max_rows: 10_000,
                    max_fields: 8,
                },
                max_concurrent: 1,
            },
        },
        support::services(Arc::clone(&store)),
    )
    .unwrap();
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let registered_response = http
        .post(format!("http://{}/v1/clients", server.endpoint()))
        .bearer_auth(server.bootstrap_token().expose())
        .body(json!({"name": "upload-shutdown"}).to_string())
        .send()
        .unwrap();
    assert_eq!(registered_response.status(), reqwest::StatusCode::OK);
    let registered: Value = serde_json::from_str(&registered_response.text().unwrap()).unwrap();
    let endpoint = server.endpoint();
    let token = registered["token"].as_str().unwrap().to_owned();
    let upload = std::thread::spawn(move || {
        http.put(format!("http://{endpoint}/v1/publications/interrupted"))
            .bearer_auth(token)
            .header("content-type", "application/vnd.apache.arrow.stream")
            .header("idempotency-key", "shutdown-upload")
            .body(reqwest::blocking::Body::new(SlowUpload {
                remaining: 32 * 1024 * 1024,
            }))
            .send()
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while server.status().active_uploads == 0 {
        assert!(Instant::now() < deadline, "upload did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
    server.shutdown().unwrap();
    let _ = upload.join().unwrap();
    assert!(
        store
            .load()
            .sources
            .iter()
            .all(|source| source.entry.derived_provenance.is_none())
    );
}
