use std::{
    collections::HashMap,
    io::{self, Read},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use image::GenericImageView;

use super::{
    cache::TileDiskCache,
    provider::{MapProviderId, TileId, provider},
};

const WORKERS: usize = 4;
const QUEUE_CAPACITY: usize = 256;
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct TileRequest {
    pub provider: MapProviderId,
    pub id: TileId,
    pub corners: [[f32; 3]; 4],
    pub priority: i32,
    pub generation: u64,
}

#[derive(Clone, Debug)]
pub struct ReadyTile {
    pub provider: MapProviderId,
    pub id: TileId,
    pub generation: u64,
    pub rgba: Vec<u8>,
    pub corners: [[f32; 3]; 4],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TileManagerStatus {
    pub queued: usize,
    pub in_flight: usize,
    pub ready: usize,
    pub failed: usize,
    pub cache_bytes: u64,
}

#[derive(Clone)]
struct Work {
    request: TileRequest,
    attempts: u32,
    url: String,
}

struct Completion {
    work: Work,
    result: Result<Vec<u8>, String>,
}

enum RequestState {
    Queued,
    InFlight,
    Ready,
    Failed { retry_at: Instant, attempts: u32 },
}

type Key = (MapProviderId, TileId);

pub struct TileManager {
    cache: Arc<Mutex<TileDiskCache>>,
    work_tx: Option<Sender<Work>>,
    work_rx: Receiver<Work>,
    completion_rx: Receiver<Completion>,
    states: HashMap<Key, RequestState>,
    latest_generation: u64,
    workers: Vec<thread::JoinHandle<()>>,
}

impl TileManager {
    pub fn new(
        cache_dir: PathBuf,
        limit: u64,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let cache = Arc::new(Mutex::new(TileDiskCache::open(cache_dir, limit)?));
        let (work_tx, work_rx) = bounded::<Work>(QUEUE_CAPACITY);
        let (completion_tx, completion_rx) = bounded::<Completion>(QUEUE_CAPACITY);
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(repaint);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("DeLOG/0.2 map tiles")
            .build()
            .map_err(io::Error::other)?;
        let mut workers = Vec::with_capacity(WORKERS);
        for _ in 0..WORKERS {
            let (rx, tx, cache, repaint, client) = (
                work_rx.clone(),
                completion_tx.clone(),
                Arc::clone(&cache),
                Arc::clone(&repaint),
                client.clone(),
            );
            workers.push(thread::spawn(move || {
                worker_loop(rx, tx, cache, repaint, client)
            }));
        }
        Ok(Self {
            cache,
            work_tx: Some(work_tx),
            work_rx,
            completion_rx,
            states: HashMap::new(),
            latest_generation: 0,
            workers,
        })
    }

    pub fn request(&mut self, request: TileRequest) {
        self.request_with_url(
            request.clone(),
            provider(request.provider).map(|p| p.url(request.id)),
        );
    }

    fn request_with_url(&mut self, request: TileRequest, url: Option<String>) {
        self.latest_generation = self.latest_generation.max(request.generation);
        let Some(url) = url else { return };
        let key = (request.provider, request.id);
        let attempts = match self.states.get(&key) {
            Some(RequestState::Queued | RequestState::InFlight | RequestState::Ready) => return,
            Some(RequestState::Failed { retry_at, .. }) if Instant::now() < *retry_at => return,
            Some(RequestState::Failed { attempts, .. }) => *attempts,
            None => 0,
        };
        let work = Work {
            request,
            attempts,
            url,
        };
        match self.work_tx.as_ref().unwrap().try_send(work) {
            Ok(()) => {
                self.states.insert(key, RequestState::Queued);
            }
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => unreachable!("workers live with manager"),
        }
    }

    pub fn poll(&mut self) -> Vec<ReadyTile> {
        let mut ready = Vec::new();
        while let Ok(completion) = self.completion_rx.try_recv() {
            let request = completion.work.request;
            let key = (request.provider, request.id);
            if request.generation != self.latest_generation {
                self.states.remove(&key);
                continue;
            }
            match completion.result {
                Ok(rgba) => {
                    self.states.insert(key, RequestState::Ready);
                    ready.push(ReadyTile {
                        provider: request.provider,
                        id: request.id,
                        generation: request.generation,
                        rgba,
                        corners: request.corners,
                    });
                }
                Err(_) => {
                    let attempts = completion.work.attempts.saturating_add(1);
                    self.states.insert(
                        key,
                        RequestState::Failed {
                            retry_at: Instant::now() + retry_delay(attempts),
                            attempts,
                        },
                    );
                }
            }
        }
        ready
    }

    pub fn status(&self) -> TileManagerStatus {
        let mut status = TileManagerStatus::default();
        for state in self.states.values() {
            match state {
                RequestState::Queued => status.queued += 1,
                RequestState::InFlight => status.in_flight += 1,
                RequestState::Ready => status.ready += 1,
                RequestState::Failed { .. } => status.failed += 1,
            }
        }
        // A bounded channel exposes the exact queued portion; accepted work no longer in
        // the channel is being handled by one of the four workers (or awaits polling).
        let accepted = status.queued + status.in_flight;
        status.queued = self.work_rx.len().min(accepted);
        status.in_flight = accepted - status.queued;
        status.cache_bytes = self.cache.lock().unwrap().usage_bytes();
        status
    }

    pub fn set_limit(&mut self, bytes: u64) -> io::Result<()> {
        self.cache.lock().unwrap().set_limit(bytes)
    }

    pub fn clear_cache(&mut self) -> io::Result<u64> {
        let cache_generation = self.cache.lock().unwrap().clear()?.0;
        self.latest_generation = self.latest_generation.wrapping_add(1).max(cache_generation);
        self.states.clear();
        Ok(self.latest_generation)
    }
}

impl Drop for TileManager {
    fn drop(&mut self) {
        self.work_tx.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn worker_loop(
    rx: Receiver<Work>,
    tx: Sender<Completion>,
    cache: Arc<Mutex<TileDiskCache>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    client: reqwest::blocking::Client,
) {
    while let Ok(work) = rx.recv() {
        let result = load_tile(&client, &cache, &work);
        if tx.send(Completion { work, result }).is_err() {
            break;
        }
        repaint();
    }
}

fn load_tile(
    client: &reqwest::blocking::Client,
    cache: &Mutex<TileDiskCache>,
    work: &Work,
) -> Result<Vec<u8>, String> {
    if let Some(bytes) = cache
        .lock()
        .unwrap()
        .read(work.request.provider, work.request.id)
        .map_err(|e| e.to_string())?
    {
        return decode_tile(&bytes);
    }
    let bytes = download(client, &work.url)?;
    let rgba = decode_tile(&bytes)?;
    cache
        .lock()
        .unwrap()
        .write(work.request.provider, work.request.id, &bytes)
        .map_err(|e| e.to_string())?;
    Ok(rgba)
}

fn download(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = client.get(url).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if content_type != "image/jpeg" {
        return Err(format!("unexpected content type {content_type}"));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES)
    {
        return Err("tile exceeds 2 MiB".into());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("tile exceeds 2 MiB".into());
    }
    Ok(bytes)
}

fn decode_tile(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?;
    if image.dimensions() != (256, 256) {
        return Err("tile must be exactly 256x256".into());
    }
    Ok(image.to_rgba8().into_raw())
}

fn retry_delay(attempts: u32) -> Duration {
    Duration::from_secs(
        1_u64
            .checked_shl(attempts.min(63))
            .unwrap_or(u64::MAX)
            .min(60),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Cursor,
        sync::atomic::{AtomicUsize, Ordering},
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

    fn await_poll(manager: &mut TileManager) -> Vec<ReadyTile> {
        for _ in 0..100 {
            let ready = manager.poll();
            if !ready.is_empty() || manager.status().failed != 0 {
                return ready;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("tile did not complete")
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
    fn corrupt_http_response_becomes_retryable_failure() {
        let dir = tempfile::tempdir().unwrap();
        let url = server(b"bad".to_vec(), "image/jpeg", Arc::new(AtomicUsize::new(0)));
        let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
        manager.request_with_url(request(1), Some(url));
        assert!(await_poll(&mut manager).is_empty());
        assert_eq!(manager.status().failed, 1);
    }

    #[test]
    fn stale_generation_results_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let url = server(jpeg(), "image/jpeg", Arc::new(AtomicUsize::new(0)));
        let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
        manager.request_with_url(request(1), Some(url));
        manager.latest_generation = 2;
        thread::sleep(Duration::from_millis(50));
        assert!(manager.poll().is_empty());
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
        thread::sleep(Duration::from_millis(80));
        assert!(manager.poll().is_empty());
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
        for _ in 0..200 {
            received += manager.poll().len();
            if received == 8 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(received, 8);
        assert_eq!(peak.load(Ordering::SeqCst), 4);
    }
}
