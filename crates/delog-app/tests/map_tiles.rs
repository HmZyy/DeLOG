#[allow(dead_code)]
mod map {
    pub mod cache {
        include!("../src/map/cache.rs");
    }
    pub mod provider {
        include!("../src/map/provider.rs");
    }

    pub mod worker {
        include!("../src/map/worker.rs");

        pub fn request_from_test(manager: &mut TileManager, request: TileRequest, url: String) {
            manager.request_with_url(request, Some(url));
        }
    }
}

use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, unbounded};
use map::provider::{MapProviderId, TileId};
use map::worker::{
    CacheActionKind, CacheActionStatus, MapScopeId, TileFailureClass, TileManager, TileRequest,
};

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

fn request() -> TileRequest {
    TileRequest {
        scope: MapScopeId(42),
        provider: MapProviderId::BingSatellite,
        id: TileId {
            zoom: 14,
            x: 8_192,
            y: 5_461,
        },
        // A small georeferenced ground quad in local ENU coordinates.
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

fn gated_server(body: Vec<u8>, hits: Arc<AtomicUsize>) -> (String, Receiver<()>, Sender<()>) {
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

fn await_ready(manager: &mut TileManager) -> Vec<map::worker::ReadyTile> {
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
fn network_disk_reuse_and_clear_discard_stale_inflight_response() {
    let cache = tempfile::tempdir().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let (url, observed, release) = gated_server(synthetic_tile(), Arc::clone(&hits));

    let mut manager = TileManager::new(cache.path().to_owned(), u64::MAX, || {}).unwrap();
    map::worker::request_from_test(&mut manager, request(), url.clone());
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    let ready = await_ready(&mut manager);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].corners, request().corners);
    drop(manager);

    let mut manager = TileManager::new(cache.path().to_owned(), u64::MAX, || {}).unwrap();
    map::worker::request_from_test(&mut manager, request(), url.clone());
    assert_eq!(await_ready(&mut manager).len(), 1);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "disk hit avoids the server");

    let clear_id = manager.clear_cache().unwrap();
    await_clear(&manager, clear_id);
    map::worker::request_from_test(&mut manager, request(), url);
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
        map::cache::TileDiskCache::open(cache.path().to_owned(), u64::MAX)
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
    let (url, observed, release) = gated_server(synthetic_tile(), hits);
    let mut manager = TileManager::new(cache_path.clone(), u64::MAX, || {}).unwrap();

    std::fs::remove_dir_all(&cache_path).unwrap();
    std::fs::write(&cache_path, b"not a directory").unwrap();
    map::worker::request_from_test(&mut manager, request(), url);
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).unwrap();

    assert_eq!(await_ready(&mut manager).len(), 1);
    assert_eq!(
        manager.status().failure.unwrap().class,
        TileFailureClass::Cache
    );

    std::fs::remove_file(&cache_path).unwrap();
    std::fs::create_dir(&cache_path).unwrap();
    let (url, observed, release) = gated_server(synthetic_tile(), Arc::new(AtomicUsize::new(0)));
    let mut next = request();
    next.id.x += 1;
    map::worker::request_from_test(&mut manager, next, url);
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).unwrap();
    assert_eq!(await_ready(&mut manager).len(), 1);
    assert_eq!(
        manager.status().failure,
        None,
        "recovered cache error clears"
    );
}
