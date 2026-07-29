use super::*;
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::{
    io::Cursor,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

fn jpeg() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(256, 256, image::Rgb([12, 34, 56]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .unwrap();
    bytes
}

fn request(generation: u64) -> TileRequest {
    TileRequest {
        scope: MapScopeId(1),
        provider: MapProviderId::BingSatellite,
        id: TileId {
            zoom: 1,
            x: 0,
            y: 0,
        },
        corners: [[0.0; 3]; 4],
        priority: 0,
        generation,
    }
}

fn server(body: Vec<u8>, content_type: &'static str, hits: Arc<AtomicUsize>) -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let address = format!("http://{}", server.server_addr());
    thread::spawn(move || {
        for req in server.incoming_requests() {
            hits.fetch_add(1, Ordering::SeqCst);
            let response = tiny_http::Response::from_data(body.clone()).with_header(
                tiny_http::Header::from_bytes("Content-Type", content_type).unwrap(),
            );
            let _ = req.respond(response);
        }
    });
    address
}

fn concurrency_server(
    body: Vec<u8>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
) -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let address = format!("http://{}", server.server_addr());
    thread::spawn(move || {
        for req in server.incoming_requests() {
            let body = body.clone();
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            thread::spawn(move || {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(50));
                let response = tiny_http::Response::from_data(body).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "image/jpeg").unwrap(),
                );
                let _ = req.respond(response);
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    address
}

fn gated_server(body: Vec<u8>) -> (String, Receiver<String>, Sender<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let address = format!("http://{}", server.server_addr());
    let (observed_tx, observed_rx) = unbounded();
    let (release_tx, release_rx) = unbounded();
    thread::spawn(move || {
        for req in server.incoming_requests() {
            let body = body.clone();
            let observed_tx = observed_tx.clone();
            let release_rx = release_rx.clone();
            thread::spawn(move || {
                let _ = observed_tx.send(req.url().to_owned());
                let _ = release_rx.recv();
                let response = tiny_http::Response::from_data(body).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "image/jpeg").unwrap(),
                );
                let _ = req.respond(response);
            });
        }
    });
    (address, observed_rx, release_tx)
}

fn await_poll(manager: &mut TileManager) -> Vec<ReadyTile> {
    for _ in 0..100 {
        let ready = manager.poll(MapScopeId(1));
        if !ready.is_empty() || manager.status().failed != 0 {
            return ready;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("tile did not complete")
}

fn await_cache_action(manager: &TileManager) -> CacheActionStatus {
    for _ in 0..200 {
        let action = manager.status().cache_action;
        if !matches!(action, CacheActionStatus::Pending { .. }) {
            return action;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("cache action did not complete")
}

fn synthetic_tile() -> Vec<u8> {
    let image = image::RgbImage::from_fn(256, 256, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 96])
    });
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .unwrap();
    bytes
}

fn request_at_scope_42() -> TileRequest {
    TileRequest {
        scope: MapScopeId(42),
        provider: MapProviderId::BingSatellite,
        id: TileId {
            zoom: 14,
            x: 8_192,
            y: 5_461,
        },
        corners: [
            [-12.5, 18.0, 0.0],
            [12.5, 18.0, 0.0],
            [12.5, -18.0, 0.0],
            [-12.5, -18.0, 0.0],
        ],
        priority: 0,
        generation: 1,
    }
}

fn test_server_with_hits(body: Vec<u8>, hits: Arc<AtomicUsize>) -> (String, Receiver<()>, Sender<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/tile.jpeg", server.server_addr());
    let (observed_tx, observed_rx) = unbounded();
    let (release_tx, release_rx) = unbounded();
    thread::spawn(move || {
        for request in server.incoming_requests() {
            hits.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            let observed_tx = observed_tx.clone();
            let release_rx = release_rx.clone();
            thread::spawn(move || {
                observed_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                let response = tiny_http::Response::from_data(body).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "image/jpeg").unwrap(),
                );
                let _ = request.respond(response);
            });
        }
    });
    (url, observed_rx, release_tx)
}

fn await_ready(manager: &mut TileManager) -> Vec<ReadyTile> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let ready = manager.poll(MapScopeId(42));
        if !ready.is_empty() {
            return ready;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("tile did not become ready: {:?}", manager.status());
}

fn await_clear(manager: &TileManager, expected_id: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match manager.status().cache_action {
            CacheActionStatus::Complete {
                id,
                kind: CacheActionKind::Clear,
                ..
            } if id == expected_id => return,
            CacheActionStatus::Error { id, message, .. } if id == expected_id => {
                panic!("cache clear failed: {message}")
            }
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("cache clear did not complete: {:?}", manager.status());
}

#[test]
fn retry_delay_is_exponential_and_capped() {
    assert_eq!(retry_delay(1), Duration::from_secs(2));
    assert_eq!(retry_delay(5), Duration::from_secs(32));
    assert_eq!(retry_delay(30), Duration::from_secs(60));
}

#[test]
fn validates_jpeg_dimensions_and_rejects_corruption() {
    assert_eq!(decode_tile(&jpeg()).unwrap().len(), 256 * 256 * 4);
    assert!(decode_tile(b"not an image").is_err());
    let small = image::RgbImage::new(1, 1);
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(small)
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .unwrap();
    assert!(decode_tile(&bytes).is_err());
}

#[test]
fn permanent_failures_do_not_become_retryable() {
    for status in [400, 403, 404] {
        let failure = classify_http_failure(reqwest::StatusCode::from_u16(status).unwrap());
        assert!(!failure.retryable);
        assert_eq!(failure.class, TileFailureClass::Permanent);
    }
    assert!(classify_http_failure(reqwest::StatusCode::INTERNAL_SERVER_ERROR).retryable);
}

#[test]
fn request_timeout_is_transient_and_retryable() {
    let failure = classify_http_failure(reqwest::StatusCode::REQUEST_TIMEOUT);
    assert!(failure.retryable);
    assert_eq!(failure.class, TileFailureClass::NetworkTransient);
}

#[test]
fn jpeg_content_type_accepts_parameters() {
    assert!(is_jpeg_content_type("image/jpeg; charset=binary"));
    assert!(is_jpeg_content_type("image/jpeg"));
    assert!(!is_jpeg_content_type("text/html; charset=utf-8"));
}

#[test]
fn deduplicates_and_uses_disk_before_network() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let url = server(jpeg(), "image/jpeg", Arc::clone(&hits));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request(1), Some(url.clone()));
    manager.request_with_url(request(1), Some(url.clone()));
    assert_eq!(await_poll(&mut manager).len(), 1);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    drop(manager);
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request(1), Some(url));
    assert_eq!(await_poll(&mut manager).len(), 1);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn continuously_requested_delivered_tile_stays_deduplicated() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let url = server(jpeg(), "image/jpeg", Arc::clone(&hits));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let tile_a = request(1);
    manager.request_with_url(tile_a.clone(), Some(format!("{url}/a")));
    assert_eq!(await_poll(&mut manager)[0].id, tile_a.id);

    for _ in 0..100 {
        manager.request_with_url(tile_a.clone(), Some(format!("{url}/a")));
        assert!(manager.poll(tile_a.scope).is_empty());
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(manager.status().ready, 1);
}

#[test]
fn tile_excluded_by_next_desired_snapshot_can_be_requested_again_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let url = server(jpeg(), "image/jpeg", Arc::clone(&hits));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let tile_a = request(1);
    manager.set_desired(
        tile_a.scope,
        tile_a.provider,
        tile_a.generation,
        [tile_a.id],
    );
    manager.request_with_url(tile_a.clone(), Some(format!("{url}/a")));
    assert_eq!(await_poll(&mut manager)[0].id, tile_a.id);

    manager.set_desired(tile_a.scope, tile_a.provider, tile_a.generation, []);
    manager.set_desired(
        tile_a.scope,
        tile_a.provider,
        tile_a.generation,
        [tile_a.id],
    );
    manager.request_with_url(tile_a.clone(), Some(format!("{url}/a")));
    assert_eq!(
        await_poll(&mut manager)[0].id,
        tile_a.id,
        "an explicitly released tile must be disk-loaded again"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "reload must come from disk");
}

#[test]
fn desired_union_keeps_fallback_ready_until_next_snapshot_prunes_it() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let url = server(jpeg(), "image/jpeg", Arc::clone(&hits));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let tile_a = request(1);
    let mut tile_b = tile_a.clone();
    tile_b.id.x = 1;

    manager.set_desired(
        tile_a.scope,
        tile_a.provider,
        tile_a.generation,
        [tile_a.id],
    );
    manager.request_with_url(tile_a.clone(), Some(format!("{url}/a")));
    assert_eq!(await_poll(&mut manager)[0].id, tile_a.id);
    manager.set_desired(
        tile_a.scope,
        tile_a.provider,
        tile_a.generation,
        [tile_a.id, tile_b.id],
    );
    manager.request_with_url(tile_b.clone(), Some(format!("{url}/b")));
    assert_eq!(await_poll(&mut manager)[0].id, tile_b.id);

    manager.request_with_url(tile_a.clone(), Some(format!("{url}/a")));
    thread::sleep(Duration::from_millis(20));
    assert!(
        manager.poll(tile_a.scope).is_empty(),
        "fallback A must remain deduplicated"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    manager.set_desired(
        tile_a.scope,
        tile_a.provider,
        tile_a.generation,
        [tile_b.id],
    );
    manager.set_desired(
        tile_a.scope,
        tile_a.provider,
        tile_a.generation,
        [tile_a.id, tile_b.id],
    );
    manager.request_with_url(tile_a.clone(), Some(format!("{url}/a")));
    assert_eq!(await_poll(&mut manager)[0].id, tile_a.id);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "returning A reloads from disk"
    );
}

#[test]
fn changing_desired_snapshots_keep_only_latest_bounded_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let base = request(1);
    for x in 0..10_000 {
        manager.set_desired(
            base.scope,
            base.provider,
            base.generation,
            [TileId { x, ..base.id }],
        );
    }
    assert_eq!(manager.test_desired_counts(), (1, 1));
    assert!(manager.test_is_desired(
        base.scope,
        base.provider,
        base.generation,
        TileId {
            x: 9_999,
            ..base.id
        }
    ));
    assert!(!manager.test_is_desired(
        base.scope,
        base.provider,
        base.generation,
        TileId { x: 0, ..base.id }
    ));
}

#[test]
fn repeated_scope_create_close_leaves_manager_maps_empty() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let base = request(1);
    for scope in 1..=1_000 {
        manager.set_desired(MapScopeId(scope), base.provider, base.generation, [base.id]);
        manager.retain_scopes(&[]);
    }
    assert!(manager.accepted_generations.lock().unwrap().is_empty());
    assert_eq!(manager.test_desired_counts(), (0, 0));
}

#[test]
fn closing_scope_rejects_in_flight_completion() {
    let dir = tempfile::tempdir().unwrap();
    let (url, observed, release) = gated_server(jpeg());
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let tile = request(1);
    manager.set_desired(tile.scope, tile.provider, tile.generation, [tile.id]);
    manager.request_with_url(tile.clone(), Some(format!("{url}/tile")));
    observed.recv_timeout(Duration::from_secs(2)).unwrap();
    manager.retain_scopes(&[]);
    release.send(()).unwrap();
    for _ in 0..100 {
        if manager.test_completion_counts().0 == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(manager.test_completion_counts(), (1, 1));
    assert!(manager.poll(tile.scope).is_empty());
}

#[test]
fn desired_snapshot_rejects_an_in_flight_completion() {
    let dir = tempfile::tempdir().unwrap();
    let (url, observed, release) = gated_server(jpeg());
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let tile = request(1);
    manager.set_desired(tile.scope, tile.provider, tile.generation, [tile.id]);
    manager.request_with_url(tile.clone(), Some(format!("{url}/a")));
    observed.recv_timeout(Duration::from_secs(2)).unwrap();

    manager.set_desired(tile.scope, tile.provider, tile.generation, []);
    let mut replacement = tile.clone();
    replacement.corners[0][0] = 9.0;
    manager.set_desired(tile.scope, tile.provider, tile.generation, [tile.id]);
    manager.request_with_url(replacement.clone(), Some(format!("{url}/replacement")));
    release.send(()).unwrap();
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(2)).unwrap(),
        "/replacement"
    );
    release.send(()).unwrap();
    let ready = await_poll(&mut manager);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].corners, replacement.corners);
    for _ in 0..100 {
        if manager.test_completion_counts().0 == 2 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(manager.test_completion_counts(), (2, 1));
    assert!(manager.poll(tile.scope).is_empty());
}

#[test]
fn corrupt_http_response_becomes_permanent_failure() {
    let dir = tempfile::tempdir().unwrap();
    let url = server(b"bad".to_vec(), "image/jpeg", Arc::new(AtomicUsize::new(0)));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request(1), Some(url));
    assert!(await_poll(&mut manager).is_empty());
    assert_eq!(manager.status().failed, 1);
    assert_eq!(
        manager.status().failure.unwrap().class,
        TileFailureClass::Permanent
    );
}

#[test]
fn repeated_unique_failures_keep_failure_state_bounded_and_evicted_keys_retryable() {
    let mut states = HashMap::new();
    let mut failed_order = VecDeque::new();
    for x in 0..=FAILURE_CAPACITY {
        let mut req = request(1);
        req.id = TileId {
            zoom: 20,
            x: x as u32,
            y: 0,
        };
        let work = Work {
            request: req,
            attempts: 0,
            url: String::new(),
            epoch: 0,
            sequence: x as u64 + 1,
        };
        let key = (
            work.request.scope,
            work.request.provider,
            work.request.id,
            work.request.generation,
        );
        mark_failed(
            &mut states,
            &mut failed_order,
            key,
            &work,
            TileFailure {
                class: TileFailureClass::NetworkTransient,
                retryable: true,
                message: "test".into(),
            },
        );
    }
    assert_eq!(states.len(), FAILURE_CAPACITY);
    assert_eq!(failed_order.len(), FAILURE_CAPACITY);
    assert_eq!(
        states
            .values()
            .filter(|(state, _)| matches!(state, RequestState::Failed { .. }))
            .count(),
        FAILURE_CAPACITY
    );

    let mut pending = BinaryHeap::new();
    let mut latest_generation = HashMap::from([(MapScopeId(1), 1)]);
    let first = request(1);
    let first_key = (first.scope, first.provider, first.id, first.generation);
    assert!(!states.contains_key(&first_key));
    let ingress = Mutex::new(vec![IngressRequest {
        request: first,
        url: "http://retry".into(),
        sequence: 10_000,
    }]);
    drain_ingress(
        &ingress,
        &mut states,
        &mut pending,
        &mut latest_generation,
        0,
    );
    assert!(
        states
            .get(&first_key)
            .is_some_and(|(state, _)| matches!(state, RequestState::Queued))
    );
}

#[test]
fn stalled_controller_control_submissions_coalesce_to_constant_space() {
    let mut controls = PendingControls::default();
    for id in 1..=10_000 {
        controls.submit(Command::SetLimit { id, bytes: id });
        controls.submit(Command::Clear { id, epoch: id });
    }
    assert_eq!(controls.len(), 2);
    assert!(matches!(
        controls.limit,
        Some(Command::SetLimit { id: 10_000, .. })
    ));
    assert!(matches!(
        controls.clear,
        Some(Command::Clear { id: 10_000, .. })
    ));
}

#[test]
fn stale_generation_results_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let url = server(jpeg(), "image/jpeg", Arc::new(AtomicUsize::new(0)));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request(1), Some(url.clone()));
    let mut newer = request(2);
    newer.id.x = 1;
    manager.request_with_url(newer, Some(url));
    thread::sleep(Duration::from_millis(50));
    assert!(
        manager
            .poll(MapScopeId(1))
            .iter()
            .all(|tile| tile.generation == 2)
    );
}

#[test]
fn pane_scopes_accept_generations_and_route_ready_results_independently() {
    let dir = tempfile::tempdir().unwrap();
    let url = server(jpeg(), "image/jpeg", Arc::new(AtomicUsize::new(0)));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let mut left = request(7);
    left.scope = MapScopeId(10);
    let mut right = request(1);
    right.scope = MapScopeId(20);
    right.id.x = 1;
    manager.request_with_url(left, Some(url.clone()));
    manager.request_with_url(right, Some(url));

    let mut left_ready = Vec::new();
    let mut right_ready = Vec::new();
    for _ in 0..200 {
        left_ready.extend(manager.poll(MapScopeId(10)));
        right_ready.extend(manager.poll(MapScopeId(20)));
        if !left_ready.is_empty() && !right_ready.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(left_ready[0].generation, 7);
    assert_eq!(right_ready[0].generation, 1);
}

#[test]
fn none_provider_and_no_reference_submit_zero_tile_requests() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let no_reference: Option<TileRequest> = None;
    if let Some(request) = no_reference {
        manager.request(request);
    }
    let mut none = request(1);
    none.provider = MapProviderId::None;
    manager.request(none);
    assert_eq!(manager.request_sequence.load(Ordering::Relaxed), 0);
    assert_eq!(manager.status().queued + manager.status().in_flight, 0);
}

#[test]
fn clear_rejects_results_from_the_previous_generation() {
    let dir = tempfile::tempdir().unwrap();
    let url = concurrency_server(
        jpeg(),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request(1), Some(url));
    manager.clear_cache().unwrap();
    assert!(matches!(
        await_cache_action(&manager),
        CacheActionStatus::Complete {
            kind: CacheActionKind::Clear,
            ..
        }
    ));
    thread::sleep(Duration::from_millis(80));
    assert!(manager.poll(MapScopeId(1)).is_empty());
    drop(manager);
    assert_eq!(
        TileDiskCache::open(dir.path().to_owned(), u64::MAX)
            .unwrap()
            .usage_bytes(),
        0
    );
}

#[test]
fn ready_before_clear_is_not_observable_after_clear_submission() {
    let dir = tempfile::tempdir().unwrap();
    let url = server(jpeg(), "image/jpeg", Arc::new(AtomicUsize::new(0)));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request(1), Some(url));
    for _ in 0..200 {
        if manager.status().ready == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(manager.status().ready, 1);
    manager.clear_cache().unwrap();
    assert_eq!(manager.status().epoch, 1);
    assert!(manager.poll(MapScopeId(1)).is_empty());
}

#[test]
fn download_submitted_after_clear_uses_the_new_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let url = server(jpeg(), "image/jpeg", Arc::new(AtomicUsize::new(0)));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.clear_cache().unwrap();
    manager.request_with_url(request(1), Some(url));
    let ready = await_poll(&mut manager);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].epoch, manager.status().epoch);
    assert_eq!(ready[0].epoch, 1);
}

#[test]
fn ready_before_generation_switch_is_not_observable_after_request_submission() {
    let dir = tempfile::tempdir().unwrap();
    let url = server(jpeg(), "image/jpeg", Arc::new(AtomicUsize::new(0)));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request(1), Some(url.clone()));
    for _ in 0..200 {
        if manager.status().ready == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(manager.status().ready, 1);

    let mut next = request(2);
    next.provider = MapProviderId::None;
    manager.request_with_url(next, Some(url));

    assert!(manager.poll(MapScopeId(1)).is_empty());
}

#[test]
fn request_ingress_drops_worst_requests_beyond_the_pending_work_bound() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let url = server(jpeg(), "image/jpeg", Arc::clone(&hits));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let count = QUEUE_CAPACITY + 64;
    for x in 0..count {
        let mut req = request(1);
        req.id = TileId {
            zoom: 10,
            x: x as u32,
            y: 0,
        };
        manager.request_with_url(req, Some(url.clone()));
    }
    let mut received = 0;
    for _ in 0..1000 {
        received += manager.poll(MapScopeId(1)).len();
        if received == count {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(received <= QUEUE_CAPACITY + WORKERS);
    assert_eq!(hits.load(Ordering::SeqCst), received);
}

#[test]
fn downloads_never_exceed_four_concurrent_requests() {
    let dir = tempfile::tempdir().unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let url = concurrency_server(jpeg(), Arc::clone(&active), Arc::clone(&peak));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    for x in 0..8 {
        let mut req = request(1);
        req.id = TileId { zoom: 4, x, y: 0 };
        manager.request_with_url(req, Some(url.clone()));
    }
    let mut received = 0;
    for _ in 0..500 {
        received += manager.poll(MapScopeId(1)).len();
        if received == 8 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(received, 8);
    assert_eq!(peak.load(Ordering::SeqCst), 4);
}

#[test]
fn same_tile_in_a_new_generation_is_not_blocked_by_old_ready_state() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let url = server(jpeg(), "image/jpeg", Arc::clone(&hits));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request(1), Some(url.clone()));
    assert_eq!(await_poll(&mut manager)[0].generation, 1);
    manager.request_with_url(request(2), Some(url));
    assert_eq!(await_poll(&mut manager)[0].generation, 2);
}

#[test]
fn stale_completion_does_not_erase_a_new_generation_request() {
    let dir = tempfile::tempdir().unwrap();
    let url = concurrency_server(
        jpeg(),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request(1), Some(url.clone()));
    manager.request_with_url(request(2), Some(url));
    let ready = await_poll(&mut manager);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].generation, 2);
    assert_eq!(manager.status().ready, 1);
}

#[test]
fn priority_queue_orders_nearest_first_then_submission_order() {
    let make = |priority, sequence| {
        Pending(Work {
            request: TileRequest {
                priority,
                ..request(1)
            },
            attempts: 0,
            url: String::new(),
            epoch: 0,
            sequence,
        })
    };
    let mut queue = BinaryHeap::from([make(20, 1), make(5, 3), make(5, 2)]);
    assert_eq!(queue.pop().unwrap().0.sequence, 2);
    assert_eq!(queue.pop().unwrap().0.sequence, 3);
    assert_eq!(queue.pop().unwrap().0.sequence, 1);
}

#[test]
fn controller_dispatches_higher_priority_when_a_worker_opens() {
    let dir = tempfile::tempdir().unwrap();
    let (url, observed, release) = gated_server(jpeg());
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    for x in 0..WORKERS {
        let mut req = request(1);
        req.id = TileId {
            zoom: 10,
            x: x as u32,
            y: 0,
        };
        manager.request_with_url(req, Some(format!("{url}/blocking-{x}")));
    }
    for _ in 0..WORKERS {
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    let mut low = request(1);
    low.id = TileId {
        zoom: 10,
        x: 10,
        y: 0,
    };
    low.priority = 100;
    manager.request_with_url(low, Some(format!("{url}/low")));
    let mut high = request(1);
    high.id = TileId {
        zoom: 10,
        x: 11,
        y: 0,
    };
    high.priority = 1;
    manager.request_with_url(high, Some(format!("{url}/high")));
    for _ in 0..100 {
        if manager.status().queued == 2 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(manager.status().queued, 2);

    release.send(()).unwrap();
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(2)).unwrap(),
        "/high"
    );
    for _ in 0..WORKERS + 2 {
        let _ = release.send(());
    }
}

#[test]
fn saturated_queue_dispatches_new_high_priority_before_existing_low_priority() {
    let dir = tempfile::tempdir().unwrap();
    let (url, observed, release) = gated_server(jpeg());
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    for x in 0..WORKERS {
        let mut req = request(1);
        req.id = TileId {
            zoom: 20,
            x: x as u32,
            y: 0,
        };
        manager.request_with_url(req, Some(format!("{url}/blocking-{x}")));
    }
    for _ in 0..WORKERS {
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    for x in 0..QUEUE_CAPACITY {
        let mut low = request(1);
        low.id = TileId {
            zoom: 20,
            x: (x + WORKERS) as u32,
            y: 0,
        };
        low.priority = 100;
        manager.request_with_url(low, Some(format!("{url}/low-{x}")));
    }
    for _ in 0..200 {
        if manager.status().queued == QUEUE_CAPACITY {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(manager.status().queued, QUEUE_CAPACITY);

    let mut high = request(1);
    high.id = TileId {
        zoom: 20,
        x: 1000,
        y: 0,
    };
    high.priority = 0;
    manager.request_with_url(high, Some(format!("{url}/high")));
    release.send(()).unwrap();

    assert_eq!(
        observed.recv_timeout(Duration::from_secs(2)).unwrap(),
        "/high"
    );
    assert_eq!(manager.status().queued, QUEUE_CAPACITY - 1);
    for _ in 0..WORKERS + 1 {
        let _ = release.send(());
    }
}

#[test]
fn newer_generation_atomically_purges_saturated_ingress_and_is_admitted() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    let ingress = Arc::clone(&manager.ingress);
    {
        let mut guard = ingress.lock().unwrap();
        for x in 0..QUEUE_CAPACITY {
            let mut old = request(1);
            old.id = TileId {
                zoom: 20,
                x: x as u32,
                y: 0,
            };
            old.priority = 0;
            guard.push(IngressRequest {
                request: old,
                url: "http://old".into(),
                sequence: x as u64,
            });
        }
        manager
            .accepted_generations
            .lock()
            .unwrap()
            .insert(MapScopeId(1), 1);
    }

    let mut newer = request(2);
    newer.id = TileId {
        zoom: 20,
        x: 999,
        y: 0,
    };
    newer.priority = i32::MAX;
    manager.request_with_url(newer, Some("http://new".into()));

    let queued = ingress.lock().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].request.generation, 2);
    assert_eq!(queued[0].url, "http://new");
}

#[test]
fn ready_results_and_state_remain_bounded_without_polling() {
    let dir = tempfile::tempdir().unwrap();
    let url = server(jpeg(), "image/jpeg", Arc::new(AtomicUsize::new(0)));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    for x in 0..(QUEUE_CAPACITY + WORKERS) {
        let mut req = request(1);
        req.id = TileId {
            zoom: 20,
            x: x as u32,
            y: 0,
        };
        manager.request_with_url(req, Some(url.clone()));
    }
    for _ in 0..1000 {
        if manager.status().queued == 0 && manager.status().in_flight == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(manager.status().ready <= READY_CAPACITY);
    assert!(manager.ready_rx.len() <= READY_CAPACITY);
}

#[test]
fn permanent_invalid_response_is_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let url = server(b"bad".to_vec(), "image/jpeg", Arc::clone(&hits));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request(1), Some(url.clone()));
    assert!(await_poll(&mut manager).is_empty());
    manager.request_with_url(request(1), Some(url.clone()));
    thread::sleep(Duration::from_millis(100));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    thread::sleep(Duration::from_millis(2100));
    manager.request_with_url(request(1), Some(url));
    thread::sleep(Duration::from_millis(100));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn shutdown_is_bounded_when_completions_are_not_polled() {
    let dir = tempfile::tempdir().unwrap();
    let url = server(jpeg(), "image/jpeg", Arc::new(AtomicUsize::new(0)));
    let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
    for x in 0..300 {
        let mut req = request(1);
        req.id = TileId { zoom: 10, x, y: 0 };
        manager.request_with_url(req, Some(url.clone()));
    }
    thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    drop(manager);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn rejects_non_success_wrong_type_and_oversized_http_responses() {
    fn one_response(status: u16, content_type: &'static str, body: Vec<u8>) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = format!("http://{}", server.server_addr());
        thread::spawn(move || {
            if let Ok(req) = server.recv() {
                let response = tiny_http::Response::from_data(body)
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", content_type).unwrap(),
                    );
                let _ = req.respond(response);
            }
        });
        address
    }
    let client = reqwest::blocking::Client::new();
    assert!(
        download(&client, &one_response(503, "image/jpeg", jpeg()))
            .unwrap_err()
            .message
            .contains("HTTP")
    );
    assert!(
        download(&client, &one_response(200, "text/plain", jpeg()))
            .unwrap_err()
            .message
            .contains("content type")
    );
    assert!(
        download(
            &client,
            &one_response(200, "image/jpeg", vec![0; MAX_RESPONSE_BYTES as usize + 1])
        )
        .unwrap_err()
        .message
        .contains("2 MiB")
    );
}

#[test]
fn network_disk_reuse_and_clear_discard_stale_inflight_response() {
    let cache = tempfile::tempdir().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let (url, observed, release) = test_server_with_hits(synthetic_tile(), Arc::clone(&hits));

    let mut manager = TileManager::new(cache.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request_at_scope_42(), Some(url.clone()));
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    let ready = await_ready(&mut manager);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].corners, request_at_scope_42().corners);
    drop(manager);

    let mut manager = TileManager::new(cache.path().to_owned(), u64::MAX, || {}).unwrap();
    manager.request_with_url(request_at_scope_42(), Some(url.clone()));
    assert_eq!(await_ready(&mut manager).len(), 1);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "disk hit avoids the server");

    let clear_id = manager.clear_cache().unwrap();
    await_clear(&manager, clear_id);
    manager.request_with_url(request_at_scope_42(), Some(url));
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    let clear_id = manager.clear_cache().unwrap();
    await_clear(&manager, clear_id);
    let stale_before = manager.test_completion_counts().1;
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let status = manager.status();
        if manager.test_completion_counts().1 > stale_before && status.in_flight == 0 {
            break;
        }
        thread::yield_now();
    }
    let status = manager.status();
    assert!(
        manager.test_completion_counts().1 > stale_before,
        "controller did not observe the stale completion: {status:?}"
    );
    assert_eq!(
        status.in_flight, 0,
        "manager did not become idle: {status:?}"
    );

    assert!(manager.poll(MapScopeId(42)).is_empty());
    assert_eq!(manager.status().ready, 0);
    drop(manager);
    assert_eq!(
        crate::map::cache::TileDiskCache::open(cache.path().to_owned(), u64::MAX)
            .unwrap()
            .usage_bytes(),
        0,
        "stale response must not repopulate disk"
    );
}

#[test]
fn cache_write_failure_still_delivers_decoded_tile() {
    let temp = tempfile::tempdir().unwrap();
    let cache_path = temp.path().join("cache");
    let hits = Arc::new(AtomicUsize::new(0));
    let (url, observed, release) = test_server_with_hits(synthetic_tile(), hits);
    let mut manager = TileManager::new(cache_path.clone(), u64::MAX, || {}).unwrap();

    std::fs::remove_dir_all(&cache_path).unwrap();
    std::fs::write(&cache_path, b"not a directory").unwrap();
    manager.request_with_url(request_at_scope_42(), Some(url));
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).unwrap();

    assert_eq!(await_ready(&mut manager).len(), 1);
    assert_eq!(
        manager.status().failure.unwrap().class,
        TileFailureClass::Cache
    );

    std::fs::remove_file(&cache_path).unwrap();
    std::fs::create_dir(&cache_path).unwrap();
    let (url, observed, release) = test_server_with_hits(synthetic_tile(), Arc::new(AtomicUsize::new(0)));
    let mut next = request_at_scope_42();
    next.id.x += 1;
    manager.request_with_url(next, Some(url));
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).unwrap();
    assert_eq!(await_ready(&mut manager).len(), 1);
    assert_eq!(
        manager.status().failure,
        None,
        "recovered cache error clears"
    );
}
