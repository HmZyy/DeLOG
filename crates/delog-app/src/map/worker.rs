use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
    io::{self, Read},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheActionKind {
    SetLimit,
    Clear,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum CacheActionStatus {
    #[default]
    Idle,
    Pending {
        id: u64,
        kind: CacheActionKind,
    },
    Complete {
        id: u64,
        kind: CacheActionKind,
        generation: Option<u64>,
    },
    Error {
        id: u64,
        kind: CacheActionKind,
        message: String,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TileManagerStatus {
    pub queued: usize,
    pub in_flight: usize,
    pub ready: usize,
    pub failed: usize,
    pub cache_bytes: u64,
    pub cache_action: CacheActionStatus,
}

struct ReadyEnvelope {
    epoch: u64,
    tile: ReadyTile,
}

#[derive(Clone)]
struct Work {
    request: TileRequest,
    attempts: u32,
    url: String,
    epoch: u64,
    sequence: u64,
}

struct Completion {
    work: Work,
    result: Result<(Vec<u8>, Vec<u8>), String>,
    worker: usize,
}

enum RequestState {
    Queued,
    InFlight,
    Ready,
    Failed { retry_at: Instant, attempts: u32 },
}

type Key = (MapProviderId, TileId, u64);

struct Pending(Work);

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.0.request.priority == other.0.request.priority && self.0.sequence == other.0.sequence
    }
}
impl Eq for Pending {}
impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Pending {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .request
            .priority
            .cmp(&self.0.request.priority)
            .then_with(|| other.0.sequence.cmp(&self.0.sequence))
    }
}

enum Command {
    Request(TileRequest, Option<String>),
    SetLimit { id: u64, bytes: u64 },
    Clear { id: u64, epoch: u64 },
}

pub struct TileManager {
    command_tx: Sender<Command>,
    ready_rx: Receiver<ReadyEnvelope>,
    status: Arc<Mutex<TileManagerStatus>>,
    epoch: Arc<AtomicU64>,
    next_action: u64,
    shutdown_tx: Sender<()>,
    controller: Option<thread::JoinHandle<()>>,
}

impl TileManager {
    pub fn new(
        cache_dir: PathBuf,
        limit: u64,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let cache = TileDiskCache::open(cache_dir, limit)?;
        let (command_tx, command_rx) = unbounded::<Command>();
        let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
        let (ready_tx, ready_rx) = unbounded();
        let status = Arc::new(Mutex::new(TileManagerStatus {
            cache_bytes: cache.usage_bytes(),
            ..Default::default()
        }));
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(repaint);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("DeLOG/0.2 map tiles")
            .build()
            .map_err(io::Error::other)?;
        let controller_status = Arc::clone(&status);
        let epoch = Arc::new(AtomicU64::new(0));
        let controller = thread::spawn(move || {
            controller_loop(
                command_rx,
                shutdown_rx,
                ready_tx,
                controller_status,
                cache,
                repaint,
                client,
            )
        });
        Ok(Self {
            command_tx,
            ready_rx,
            status,
            epoch,
            next_action: 0,
            shutdown_tx,
            controller: Some(controller),
        })
    }

    pub fn request(&mut self, request: TileRequest) {
        self.request_with_url(
            request.clone(),
            provider(request.provider).map(|p| p.url(request.id)),
        );
    }

    fn request_with_url(&mut self, request: TileRequest, url: Option<String>) {
        let _ = self.command_tx.try_send(Command::Request(request, url));
    }

    pub fn poll(&mut self) -> Vec<ReadyTile> {
        let epoch = self.epoch.load(AtomicOrdering::Acquire);
        self.ready_rx
            .try_iter()
            .filter(|ready| ready.epoch == epoch)
            .map(|ready| ready.tile)
            .collect()
    }

    pub fn status(&self) -> TileManagerStatus {
        self.status.lock().unwrap().clone()
    }

    /// Queues a cache-limit update and returns its operation ID immediately.
    /// Completion or failure is published through [`Self::status`].
    pub fn set_limit(&mut self, bytes: u64) -> io::Result<u64> {
        let id = self.next_action.wrapping_add(1);
        self.next_action = id;
        self.status.lock().unwrap().cache_action = CacheActionStatus::Pending {
            id,
            kind: CacheActionKind::SetLimit,
        };
        if let Err(error) = self.command_tx.send(Command::SetLimit { id, bytes }) {
            finish_cache_action(
                &self.status,
                id,
                CacheActionKind::SetLimit,
                Err(io::Error::other(error)),
                None,
            );
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "tile controller stopped",
            ));
        }
        Ok(id)
    }

    /// Invalidates ready results and queues a disk clear, returning its
    /// operation ID immediately. Completion, generation, or failure is
    /// published through [`Self::status`].
    pub fn clear_cache(&mut self) -> io::Result<u64> {
        let id = self.next_action.wrapping_add(1);
        self.next_action = id;
        let epoch = self
            .epoch
            .fetch_add(1, AtomicOrdering::AcqRel)
            .wrapping_add(1);
        self.status.lock().unwrap().cache_action = CacheActionStatus::Pending {
            id,
            kind: CacheActionKind::Clear,
        };
        if let Err(error) = self.command_tx.send(Command::Clear { id, epoch }) {
            finish_cache_action(
                &self.status,
                id,
                CacheActionKind::Clear,
                Err(io::Error::other(error)),
                None,
            );
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "tile controller stopped",
            ));
        }
        Ok(id)
    }
}

impl Drop for TileManager {
    fn drop(&mut self) {
        // Shutdown bypasses queued commands. Joining is bounded by the 10-second
        // HTTP timeout of the at-most-four requests already running.
        let _ = self.shutdown_tx.try_send(());
        if let Some(controller) = self.controller.take() {
            let _ = controller.join();
        }
    }
}

fn network_load(
    client: &reqwest::blocking::Client,
    work: &Work,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let bytes = download(client, &work.url)?;
    let rgba = decode_tile(&bytes)?;
    Ok((bytes, rgba))
}

fn controller_loop(
    command_rx: Receiver<Command>,
    shutdown_rx: Receiver<()>,
    ready_tx: Sender<ReadyEnvelope>,
    status_snapshot: Arc<Mutex<TileManagerStatus>>,
    mut cache: TileDiskCache,
    repaint: Arc<dyn Fn() + Send + Sync>,
    client: reqwest::blocking::Client,
) {
    let (completion_tx, completion_rx) = unbounded::<Completion>();
    let mut worker_txs = Vec::new();
    let mut workers = Vec::new();
    for worker in 0..WORKERS {
        let (tx, rx) = bounded::<Work>(1);
        let completion_tx = completion_tx.clone();
        let client = client.clone();
        workers.push(thread::spawn(move || {
            while let Ok(work) = rx.recv() {
                let result = network_load(&client, &work);
                if completion_tx
                    .send(Completion {
                        work,
                        result,
                        worker,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        worker_txs.push(tx);
    }
    drop(completion_tx);
    let mut states: HashMap<Key, (RequestState, u64)> = HashMap::new();
    let mut pending = BinaryHeap::new();
    let mut idle: BinaryHeap<std::cmp::Reverse<usize>> =
        (0..WORKERS).map(std::cmp::Reverse).collect();
    let mut latest_generation = 0;
    let mut epoch = 0_u64;
    let mut sequence = 0_u64;
    let mut shutdown = false;
    while !shutdown {
        if pending.len() < QUEUE_CAPACITY {
            select! {
            recv(shutdown_rx) -> _ => shutdown = true,
            recv(command_rx) -> command => match command {
                Ok(command) => handle_command(command, &mut cache, &mut states, &mut pending, &mut latest_generation, &mut epoch, &mut sequence, &status_snapshot),
                Err(_) => shutdown = true,
            },
            recv(completion_rx) -> completion => if let Ok(completion) = completion {
                process_completion(completion, &mut idle, &mut states, &mut cache, &ready_tx, &repaint, epoch, latest_generation);
            }
            }
        } else {
            select! {
                recv(shutdown_rx) -> _ => shutdown = true,
                recv(completion_rx) -> completion => if let Ok(completion) = completion {
                    process_completion(completion, &mut idle, &mut states, &mut cache, &ready_tx, &repaint, epoch, latest_generation);
                }
            }
        }
        while pending.len() < QUEUE_CAPACITY {
            let Ok(command) = command_rx.try_recv() else {
                break;
            };
            handle_command(
                command,
                &mut cache,
                &mut states,
                &mut pending,
                &mut latest_generation,
                &mut epoch,
                &mut sequence,
                &status_snapshot,
            );
        }
        while !shutdown {
            if pending.is_empty() || idle.is_empty() {
                break;
            }
            let Pending(work) = pending.pop().unwrap();
            let std::cmp::Reverse(worker) = idle.pop().unwrap();
            let key = (
                work.request.provider,
                work.request.id,
                work.request.generation,
            );
            if work.epoch != epoch
                || work.request.generation != latest_generation
                || !states
                    .get(&key)
                    .is_some_and(|(_, token)| *token == work.sequence)
            {
                idle.push(std::cmp::Reverse(worker));
                continue;
            }
            match cache.read(work.request.provider, work.request.id) {
                Ok(Some(bytes)) => match decode_tile(&bytes) {
                    Ok(rgba) => {
                        states.insert(key, (RequestState::Ready, work.sequence));
                        let _ = ready_tx.send(ReadyEnvelope {
                            epoch: work.epoch,
                            tile: ReadyTile {
                                provider: work.request.provider,
                                id: work.request.id,
                                generation: work.request.generation,
                                rgba,
                                corners: work.request.corners,
                            },
                        });
                        idle.push(std::cmp::Reverse(worker));
                        repaint();
                    }
                    Err(_) => {
                        states.insert(key, (RequestState::InFlight, work.sequence));
                        let _ = worker_txs[worker].send(work);
                    }
                },
                _ => {
                    states.insert(key, (RequestState::InFlight, work.sequence));
                    let _ = worker_txs[worker].send(work);
                }
            }
        }
        publish_status(&status_snapshot, &states, cache.usage_bytes());
    }
    drop(worker_txs);
    for worker in workers {
        let _ = worker.join();
    }
}

fn handle_command(
    command: Command,
    cache: &mut TileDiskCache,
    states: &mut HashMap<Key, (RequestState, u64)>,
    pending: &mut BinaryHeap<Pending>,
    latest_generation: &mut u64,
    epoch: &mut u64,
    sequence: &mut u64,
    status_snapshot: &Mutex<TileManagerStatus>,
) {
    match command {
        Command::Request(request, Some(url)) => {
            if request.generation > *latest_generation {
                *latest_generation = request.generation;
                states.retain(|key, _| key.2 == request.generation);
            }
            if request.generation != *latest_generation {
                return;
            }
            let key = (request.provider, request.id, request.generation);
            let attempts = match states.get(&key).map(|x| &x.0) {
                Some(RequestState::Queued | RequestState::InFlight | RequestState::Ready) => {
                    return;
                }
                Some(RequestState::Failed { retry_at, .. }) if Instant::now() < *retry_at => {
                    return;
                }
                Some(RequestState::Failed { attempts, .. }) => *attempts,
                None => 0,
            };
            *sequence = sequence.wrapping_add(1);
            let work = Work {
                request,
                attempts,
                url,
                epoch: *epoch,
                sequence: *sequence,
            };
            states.insert(key, (RequestState::Queued, *sequence));
            pending.push(Pending(work));
        }
        Command::Request(_, None) => {}
        Command::SetLimit { id, bytes } => {
            let result = cache.set_limit(bytes);
            finish_cache_action(status_snapshot, id, CacheActionKind::SetLimit, result, None);
        }
        Command::Clear {
            id,
            epoch: next_epoch,
        } => {
            *epoch = next_epoch;
            states.clear();
            pending.clear();
            let result = cache.clear().map(|generation| {
                *latest_generation = latest_generation.wrapping_add(1).max(generation.0);
                *latest_generation
            });
            match result {
                Ok(generation) => finish_cache_action(
                    status_snapshot,
                    id,
                    CacheActionKind::Clear,
                    Ok(()),
                    Some(generation),
                ),
                Err(error) => finish_cache_action(
                    status_snapshot,
                    id,
                    CacheActionKind::Clear,
                    Err(error),
                    None,
                ),
            }
        }
    }
}

fn finish_cache_action(
    snapshot: &Mutex<TileManagerStatus>,
    id: u64,
    kind: CacheActionKind,
    result: io::Result<()>,
    generation: Option<u64>,
) {
    let mut status = snapshot.lock().unwrap();
    if matches!(status.cache_action, CacheActionStatus::Pending { id: pending, .. } if pending == id)
    {
        status.cache_action = match result {
            Ok(()) => CacheActionStatus::Complete {
                id,
                kind,
                generation,
            },
            Err(error) => CacheActionStatus::Error {
                id,
                kind,
                message: error.to_string(),
            },
        };
    }
}

fn process_completion(
    completion: Completion,
    idle: &mut BinaryHeap<std::cmp::Reverse<usize>>,
    states: &mut HashMap<Key, (RequestState, u64)>,
    cache: &mut TileDiskCache,
    ready_tx: &Sender<ReadyEnvelope>,
    repaint: &Arc<dyn Fn() + Send + Sync>,
    epoch: u64,
    latest_generation: u64,
) {
    idle.push(std::cmp::Reverse(completion.worker));
    let work = completion.work;
    let key = (
        work.request.provider,
        work.request.id,
        work.request.generation,
    );
    let current = states
        .get(&key)
        .is_some_and(|(_, token)| *token == work.sequence);
    if current && work.epoch == epoch && work.request.generation == latest_generation {
        match completion.result {
            Ok((bytes, rgba)) => {
                if cache
                    .write(work.request.provider, work.request.id, &bytes)
                    .is_ok()
                {
                    states.insert(key, (RequestState::Ready, work.sequence));
                    let _ = ready_tx.send(ReadyEnvelope {
                        epoch: work.epoch,
                        tile: ReadyTile {
                            provider: work.request.provider,
                            id: work.request.id,
                            generation: work.request.generation,
                            rgba,
                            corners: work.request.corners,
                        },
                    });
                } else {
                    mark_failed(states, key, &work);
                }
            }
            Err(_) => mark_failed(states, key, &work),
        }
        repaint();
    }
}

fn mark_failed(states: &mut HashMap<Key, (RequestState, u64)>, key: Key, work: &Work) {
    let attempts = work.attempts.saturating_add(1);
    states.insert(
        key,
        (
            RequestState::Failed {
                retry_at: Instant::now() + retry_delay(attempts),
                attempts,
            },
            work.sequence,
        ),
    );
}

fn publish_status(
    snapshot: &Mutex<TileManagerStatus>,
    states: &HashMap<Key, (RequestState, u64)>,
    cache_bytes: u64,
) {
    let mut queued = 0;
    let mut in_flight = 0;
    let mut ready = 0;
    let mut failed = 0;
    for (state, _) in states.values() {
        match state {
            RequestState::Queued => queued += 1,
            RequestState::InFlight => in_flight += 1,
            RequestState::Ready => ready += 1,
            RequestState::Failed { .. } => failed += 1,
        }
    }
    let mut status = snapshot.lock().unwrap();
    status.queued = queued;
    status.in_flight = in_flight;
    status.ready = ready;
    status.failed = failed;
    status.cache_bytes = cache_bytes;
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
            let ready = manager.poll();
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
        manager.request_with_url(request(1), Some(url.clone()));
        let mut newer = request(2);
        newer.id.x = 1;
        manager.request_with_url(newer, Some(url));
        thread::sleep(Duration::from_millis(50));
        assert!(manager.poll().iter().all(|tile| tile.generation == 2));
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
        assert!(manager.poll().is_empty());
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
        assert!(manager.poll().is_empty());
    }

    #[test]
    fn request_ingress_accepts_more_than_the_pending_work_bound() {
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
            received += manager.poll().len();
            if received == count {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(received, count);
        assert_eq!(hits.load(Ordering::SeqCst), count);
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
            received += manager.poll().len();
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
    fn retry_is_suppressed_until_eligible() {
        let dir = tempfile::tempdir().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let url = server(b"bad".to_vec(), "image/jpeg", Arc::clone(&hits));
        let mut manager = TileManager::new(dir.path().to_owned(), u64::MAX, || {}).unwrap();
        manager.request_with_url(request(1), Some(url.clone()));
        assert!(await_poll(&mut manager).is_empty());
        manager.request_with_url(request(1), Some(url.clone()));
        thread::sleep(Duration::from_millis(100));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        thread::sleep(Duration::from_millis(2000));
        manager.request_with_url(request(1), Some(url));
        for _ in 0..100 {
            if hits.load(Ordering::SeqCst) == 2 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(hits.load(Ordering::SeqCst), 2);
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
                .contains("HTTP")
        );
        assert!(
            download(&client, &one_response(200, "text/plain", jpeg()))
                .unwrap_err()
                .contains("content type")
        );
        assert!(
            download(
                &client,
                &one_response(200, "image/jpeg", vec![0; MAX_RESPONSE_BYTES as usize + 1])
            )
            .unwrap_err()
            .contains("2 MiB")
        );
    }
}
