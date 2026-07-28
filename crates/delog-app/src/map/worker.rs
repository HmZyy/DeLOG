use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap, HashSet, VecDeque},
    io::{self, Read},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering},
    },
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, bounded, select};
use image::GenericImageView;

use super::{
    cache::TileDiskCache,
    provider::{MapProviderId, TileId, provider},
};

const WORKERS: usize = 4;
const QUEUE_CAPACITY: usize = 256;
const READY_CAPACITY: usize = 256;
const FAILURE_CAPACITY: usize = 256;
const DESIRED_CAPACITY_PER_SCOPE: usize = 512;
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct TileRequest {
    pub scope: MapScopeId,
    pub provider: MapProviderId,
    pub id: TileId,
    pub corners: [[f32; 3]; 4],
    pub priority: i32,
    pub generation: u64,
}

#[derive(Clone, Debug)]
pub struct ReadyTile {
    pub scope: MapScopeId,
    pub epoch: u64,
    pub provider: MapProviderId,
    pub id: TileId,
    pub generation: u64,
    pub rgba: Vec<u8>,
    pub corners: [[f32; 3]; 4],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileFailureClass {
    NetworkTransient,
    Cache,
    Permanent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TileFailure {
    pub class: TileFailureClass,
    pub retryable: bool,
    pub message: String,
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
    pub epoch: u64,
    pub queued: usize,
    pub in_flight: usize,
    pub ready: usize,
    pub failed: usize,
    pub failure: Option<TileFailure>,
    pub completions_processed: u64,
    pub stale_completions_discarded: u64,
    pub cache_bytes: u64,
    pub cache_action: CacheActionStatus,
}

struct ReadyEnvelope {
    epoch: u64,
    sequence: u64,
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
    result: Result<(Vec<u8>, Vec<u8>), TileFailure>,
    worker: usize,
}

enum RequestState {
    Queued,
    InFlight,
    Ready,
    Failed {
        retry_at: Option<Instant>,
        attempts: u32,
        failure: TileFailure,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct MapScopeId(pub u64);

type Key = (MapScopeId, MapProviderId, TileId, u64);

#[derive(Clone, Debug)]
struct DesiredSnapshot {
    provider: MapProviderId,
    generation: u64,
    ids: HashMap<TileId, u64>,
    revision: u64,
}

impl DesiredSnapshot {
    fn contains(&self, key: &Key) -> bool {
        key.1 == self.provider && key.3 == self.generation && self.ids.contains_key(&key.2)
    }

    fn accepts(&self, key: &Key, sequence: u64) -> bool {
        key.1 == self.provider
            && key.3 == self.generation
            && self
                .ids
                .get(&key.2)
                .is_some_and(|minimum| sequence >= *minimum)
    }
}

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
    SetLimit { id: u64, bytes: u64 },
    Clear { id: u64, epoch: u64 },
}

#[derive(Default)]
struct PendingControls {
    limit: Option<Command>,
    clear: Option<Command>,
}

impl PendingControls {
    fn submit(&mut self, command: Command) {
        match command {
            command @ Command::SetLimit { .. } => self.limit = Some(command),
            command @ Command::Clear { .. } => self.clear = Some(command),
        }
    }

    fn take(&mut self) -> Vec<Command> {
        let mut commands: Vec<_> = [self.limit.take(), self.clear.take()]
            .into_iter()
            .flatten()
            .collect();
        commands.sort_by_key(|command| match command {
            Command::SetLimit { id, .. } | Command::Clear { id, .. } => *id,
        });
        commands
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        usize::from(self.limit.is_some()) + usize::from(self.clear.is_some())
    }
}

pub struct TileManager {
    controls: Arc<Mutex<PendingControls>>,
    wake_tx: Sender<()>,
    wake_pending: Arc<AtomicBool>,
    ingress: Arc<Mutex<Vec<IngressRequest>>>,
    ready_rx: Receiver<ReadyEnvelope>,
    ready_backlog: VecDeque<ReadyEnvelope>,
    status: Arc<Mutex<TileManagerStatus>>,
    epoch: Arc<AtomicU64>,
    accepted_generations: Mutex<HashMap<MapScopeId, u64>>,
    desired: Arc<Mutex<HashMap<MapScopeId, DesiredSnapshot>>>,
    desired_revision: AtomicU64,
    request_sequence: AtomicU64,
    next_action: u64,
    shutdown_tx: Sender<()>,
    controller: Option<thread::JoinHandle<()>>,
}

struct IngressRequest {
    request: TileRequest,
    url: String,
    sequence: u64,
}

impl TileManager {
    pub fn new(
        cache_dir: PathBuf,
        limit: u64,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let cache = TileDiskCache::open(cache_dir, limit)?;
        let controls = Arc::new(Mutex::new(PendingControls::default()));
        let (wake_tx, wake_rx) = bounded::<()>(1);
        let wake_pending = Arc::new(AtomicBool::new(false));
        let ingress = Arc::new(Mutex::new(Vec::with_capacity(QUEUE_CAPACITY)));
        let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
        let (ready_tx, ready_rx) = bounded(READY_CAPACITY);
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
        let controller_ingress = Arc::clone(&ingress);
        let controller_controls = Arc::clone(&controls);
        let controller_wake_pending = Arc::clone(&wake_pending);
        let desired = Arc::new(Mutex::new(HashMap::new()));
        let controller_desired = Arc::clone(&desired);
        let ready_evict_rx = ready_rx.clone();
        let controller = thread::spawn(move || {
            controller_loop(
                controller_controls,
                wake_rx,
                controller_wake_pending,
                shutdown_rx,
                ready_tx,
                ready_evict_rx,
                controller_status,
                cache,
                repaint,
                client,
                controller_ingress,
                controller_desired,
            )
        });
        Ok(Self {
            controls,
            wake_tx,
            wake_pending,
            ingress,
            ready_rx,
            ready_backlog: VecDeque::new(),
            status,
            epoch,
            accepted_generations: Mutex::new(HashMap::new()),
            desired,
            desired_revision: AtomicU64::new(0),
            request_sequence: AtomicU64::new(0),
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
        let key = (
            request.scope,
            request.provider,
            request.id,
            request.generation,
        );
        if self
            .desired
            .lock()
            .unwrap()
            .get(&request.scope)
            .is_some_and(|snapshot| !snapshot.contains(&key))
        {
            return;
        }
        let mut ingress = self.ingress.lock().unwrap();
        let mut accepted = self.accepted_generations.lock().unwrap();
        let generation = accepted.entry(request.scope).or_default();
        if request.generation < *generation {
            return;
        }
        if request.generation > *generation {
            *generation = request.generation;
            ingress.retain(|item| item.request.scope != request.scope);
        }
        let Some(url) = url else { return };
        let sequence = self
            .request_sequence
            .fetch_add(1, AtomicOrdering::Relaxed)
            .wrapping_add(1);
        let incoming = IngressRequest {
            request,
            url,
            sequence,
        };
        if ingress.len() < QUEUE_CAPACITY {
            ingress.push(incoming);
        } else if let Some((worst, _)) = ingress
            .iter()
            .enumerate()
            .max_by_key(|(_, item)| (item.request.priority, item.sequence))
            && (incoming.request.priority, incoming.sequence)
                < (ingress[worst].request.priority, ingress[worst].sequence)
        {
            ingress[worst] = incoming;
        }
        drop(ingress);
        if !self.wake_pending.swap(true, AtomicOrdering::AcqRel) {
            let _ = self.wake_tx.try_send(());
        }
    }

    pub fn poll(&mut self, scope: MapScopeId) -> Vec<ReadyTile> {
        let epoch = self.epoch.load(AtomicOrdering::Acquire);
        let generations = self.accepted_generations.lock().unwrap().clone();
        let generation = generations.get(&scope).copied().unwrap_or(0);
        let desired = self.desired.lock().unwrap().clone();
        self.ready_backlog.extend(self.ready_rx.try_iter());
        let mut selected = Vec::new();
        self.ready_backlog.retain(|ready| {
            if ready.epoch != epoch {
                return false;
            }
            let key = (
                ready.tile.scope,
                ready.tile.provider,
                ready.tile.id,
                ready.tile.generation,
            );
            if desired
                .get(&ready.tile.scope)
                .is_some_and(|snapshot| !snapshot.accepts(&key, ready.sequence))
            {
                return false;
            }
            if generations.get(&ready.tile.scope).copied() != Some(ready.tile.generation) {
                return false;
            }
            if ready.tile.scope == scope && ready.tile.generation == generation {
                selected.push(ready.tile.clone());
                false
            } else {
                true
            }
        });
        while self.ready_backlog.len() > READY_CAPACITY {
            self.ready_backlog.pop_front();
        }
        selected
    }

    /// Replaces the complete current/fallback ownership set for one pane.
    pub fn set_desired(
        &mut self,
        scope: MapScopeId,
        provider: MapProviderId,
        generation: u64,
        ids: impl IntoIterator<Item = TileId>,
    ) {
        let previous = self.desired.lock().unwrap().get(&scope).cloned();
        let next_sequence = self
            .request_sequence
            .load(AtomicOrdering::Acquire)
            .wrapping_add(1);
        let ids: HashMap<_, _> = ids
            .into_iter()
            .take(DESIRED_CAPACITY_PER_SCOPE)
            .map(|id| {
                let minimum = previous
                    .as_ref()
                    .filter(|old| old.provider == provider && old.generation == generation)
                    .and_then(|old| old.ids.get(&id).copied())
                    .unwrap_or(next_sequence);
                (id, minimum)
            })
            .collect();
        let revision = self
            .desired_revision
            .fetch_add(1, AtomicOrdering::AcqRel)
            .wrapping_add(1);
        let snapshot = DesiredSnapshot {
            provider,
            generation,
            ids,
            revision,
        };
        self.accepted_generations
            .lock()
            .unwrap()
            .insert(scope, generation);
        self.desired.lock().unwrap().insert(scope, snapshot.clone());
        self.ingress.lock().unwrap().retain(|item| {
            item.request.scope != scope
                || snapshot.accepts(
                    &(
                        scope,
                        item.request.provider,
                        item.request.id,
                        item.request.generation,
                    ),
                    item.sequence,
                )
        });
        self.ready_backlog.retain(|ready| {
            ready.tile.scope != scope
                || snapshot.accepts(
                    &(
                        scope,
                        ready.tile.provider,
                        ready.tile.id,
                        ready.tile.generation,
                    ),
                    ready.sequence,
                )
        });
        self.wake_controller();
    }

    pub fn retain_scopes(&mut self, live: &[MapScopeId]) {
        let live: HashSet<_> = live.iter().copied().collect();
        self.accepted_generations
            .lock()
            .unwrap()
            .retain(|scope, _| live.contains(scope));
        self.desired
            .lock()
            .unwrap()
            .retain(|scope, _| live.contains(scope));
        self.ingress
            .lock()
            .unwrap()
            .retain(|item| live.contains(&item.request.scope));
        self.ready_backlog
            .retain(|ready| live.contains(&ready.tile.scope));
        self.wake_controller();
    }

    #[cfg(test)]
    fn test_desired_counts(&self) -> (usize, usize) {
        let desired = self.desired.lock().unwrap();
        (
            desired.len(),
            desired.values().map(|snapshot| snapshot.ids.len()).sum(),
        )
    }

    #[cfg(test)]
    fn test_is_desired(
        &self,
        scope: MapScopeId,
        provider: MapProviderId,
        generation: u64,
        id: TileId,
    ) -> bool {
        self.desired
            .lock()
            .unwrap()
            .get(&scope)
            .is_some_and(|snapshot| snapshot.contains(&(scope, provider, id, generation)))
    }

    pub fn status(&self) -> TileManagerStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn test_completion_counts(&self) -> (u64, u64) {
        let status = self.status.lock().unwrap();
        (
            status.completions_processed,
            status.stale_completions_discarded,
        )
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
        self.controls
            .lock()
            .unwrap()
            .submit(Command::SetLimit { id, bytes });
        self.wake_controller();
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
        self.ingress.lock().unwrap().clear();
        self.status.lock().unwrap().cache_action = CacheActionStatus::Pending {
            id,
            kind: CacheActionKind::Clear,
        };
        self.status.lock().unwrap().epoch = epoch;
        self.controls
            .lock()
            .unwrap()
            .submit(Command::Clear { id, epoch });
        self.wake_controller();
        Ok(id)
    }

    fn wake_controller(&self) {
        if !self.wake_pending.swap(true, AtomicOrdering::AcqRel) {
            let _ = self.wake_tx.try_send(());
        }
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
) -> Result<(Vec<u8>, Vec<u8>), TileFailure> {
    let bytes = download(client, &work.url)?;
    let rgba = decode_tile(&bytes).map_err(|message| TileFailure {
        class: TileFailureClass::Permanent,
        retryable: false,
        message,
    })?;
    Ok((bytes, rgba))
}

fn controller_loop(
    controls: Arc<Mutex<PendingControls>>,
    wake_rx: Receiver<()>,
    wake_pending: Arc<AtomicBool>,
    shutdown_rx: Receiver<()>,
    ready_tx: Sender<ReadyEnvelope>,
    ready_evict_rx: Receiver<ReadyEnvelope>,
    status_snapshot: Arc<Mutex<TileManagerStatus>>,
    mut cache: TileDiskCache,
    repaint: Arc<dyn Fn() + Send + Sync>,
    client: reqwest::blocking::Client,
    ingress: Arc<Mutex<Vec<IngressRequest>>>,
    desired: Arc<Mutex<HashMap<MapScopeId, DesiredSnapshot>>>,
) {
    let (completion_tx, completion_rx) = bounded::<Completion>(WORKERS);
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
    let mut latest_generations = HashMap::new();
    let mut applied_desired_revisions = HashMap::new();
    let mut controller_desired = HashMap::new();
    let mut ready_order = VecDeque::with_capacity(READY_CAPACITY);
    let mut failed_order = VecDeque::with_capacity(FAILURE_CAPACITY);
    let mut epoch = 0_u64;
    let mut shutdown = false;
    while !shutdown {
        select! {
            recv(shutdown_rx) -> _ => shutdown = true,
            recv(wake_rx) -> _ => { wake_pending.store(false, AtomicOrdering::Release); },
            recv(completion_rx) -> completion => if let Ok(completion) = completion {
                process_completion(completion, &mut idle, &mut states, &mut ready_order, &mut failed_order, &mut cache, &ready_tx, &ready_evict_rx, &repaint, epoch, &latest_generations, &controller_desired, &desired, &status_snapshot);
            }
        }
        for command in controls.lock().unwrap().take() {
            handle_command(
                command,
                &mut cache,
                &mut states,
                &mut pending,
                &mut epoch,
                &status_snapshot,
            );
        }
        apply_desired_snapshots(
            &desired,
            &ingress,
            &mut states,
            &mut pending,
            &mut ready_order,
            &mut failed_order,
            &mut latest_generations,
            &mut applied_desired_revisions,
            &mut controller_desired,
        );
        drain_ingress(
            &ingress,
            &mut states,
            &mut pending,
            &mut latest_generations,
            epoch,
        );
        while !shutdown {
            if pending.is_empty() || idle.is_empty() {
                break;
            }
            let Pending(work) = pending.pop().unwrap();
            let std::cmp::Reverse(worker) = idle.pop().unwrap();
            let key = (
                work.request.scope,
                work.request.provider,
                work.request.id,
                work.request.generation,
            );
            if work.epoch != epoch
                || latest_generations.get(&work.request.scope).copied()
                    != Some(work.request.generation)
                || !controller_desired
                    .get(&work.request.scope)
                    .is_none_or(|snapshot| snapshot.accepts(&key, work.sequence))
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
                        retain_ready(&mut states, &mut ready_order, key, work.sequence);
                        send_ready(
                            &ready_tx,
                            &ready_evict_rx,
                            ReadyEnvelope {
                                epoch: work.epoch,
                                sequence: work.sequence,
                                tile: ReadyTile {
                                    scope: work.request.scope,
                                    epoch: work.epoch,
                                    provider: work.request.provider,
                                    id: work.request.id,
                                    generation: work.request.generation,
                                    rgba,
                                    corners: work.request.corners,
                                },
                            },
                        );
                        idle.push(std::cmp::Reverse(worker));
                        repaint();
                    }
                    Err(_) => {
                        states.insert(key, (RequestState::InFlight, work.sequence));
                        let _ = worker_txs[worker].send(work);
                    }
                },
                Err(error) => {
                    status_snapshot.lock().unwrap().failure = Some(TileFailure {
                        class: TileFailureClass::Cache,
                        retryable: false,
                        message: error.to_string(),
                    });
                    states.insert(key, (RequestState::InFlight, work.sequence));
                    let _ = worker_txs[worker].send(work);
                }
                Ok(None) => {
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

fn apply_desired_snapshots(
    desired: &Mutex<HashMap<MapScopeId, DesiredSnapshot>>,
    ingress: &Mutex<Vec<IngressRequest>>,
    states: &mut HashMap<Key, (RequestState, u64)>,
    pending: &mut BinaryHeap<Pending>,
    ready_order: &mut VecDeque<Key>,
    failed_order: &mut VecDeque<(Key, u64)>,
    latest_generations: &mut HashMap<MapScopeId, u64>,
    applied_revisions: &mut HashMap<MapScopeId, u64>,
    controller_desired: &mut HashMap<MapScopeId, DesiredSnapshot>,
) {
    let snapshots = desired.lock().unwrap().clone();
    if snapshots.keys().all(|scope| {
        applied_revisions.get(scope) == snapshots.get(scope).map(|snapshot| &snapshot.revision)
    }) && applied_revisions.len() == snapshots.len()
    {
        return;
    }
    *controller_desired = snapshots;
    *applied_revisions = controller_desired
        .iter()
        .map(|(scope, snapshot)| (*scope, snapshot.revision))
        .collect();
    latest_generations.retain(|scope, _| controller_desired.contains_key(scope));
    for (scope, snapshot) in controller_desired.iter() {
        latest_generations.insert(*scope, snapshot.generation);
    }
    states.retain(|key, (_, sequence)| {
        controller_desired
            .get(&key.0)
            .is_some_and(|snapshot| snapshot.accepts(key, *sequence))
    });
    pending.retain(|item| {
        controller_desired
            .get(&item.0.request.scope)
            .is_some_and(|snapshot| {
                snapshot.accepts(
                    &(
                        item.0.request.scope,
                        item.0.request.provider,
                        item.0.request.id,
                        item.0.request.generation,
                    ),
                    item.0.sequence,
                )
            })
    });
    ready_order.retain(|key| {
        controller_desired
            .get(&key.0)
            .is_some_and(|snapshot| snapshot.contains(key))
    });
    failed_order.retain(|(key, _)| {
        controller_desired
            .get(&key.0)
            .is_some_and(|snapshot| snapshot.contains(key))
    });
    ingress.lock().unwrap().retain(|item| {
        controller_desired
            .get(&item.request.scope)
            .is_some_and(|snapshot| {
                snapshot.accepts(
                    &(
                        item.request.scope,
                        item.request.provider,
                        item.request.id,
                        item.request.generation,
                    ),
                    item.sequence,
                )
            })
    });
}

fn handle_command(
    command: Command,
    cache: &mut TileDiskCache,
    states: &mut HashMap<Key, (RequestState, u64)>,
    pending: &mut BinaryHeap<Pending>,
    epoch: &mut u64,
    status_snapshot: &Mutex<TileManagerStatus>,
) {
    match command {
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
            let result = cache.clear().map(|generation| generation.0);
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

fn drain_ingress(
    ingress: &Mutex<Vec<IngressRequest>>,
    states: &mut HashMap<Key, (RequestState, u64)>,
    pending: &mut BinaryHeap<Pending>,
    latest_generations: &mut HashMap<MapScopeId, u64>,
    epoch: u64,
) {
    let requests = std::mem::take(&mut *ingress.lock().unwrap());
    for item in requests {
        let request = item.request;
        let latest = latest_generations.entry(request.scope).or_default();
        if request.generation > *latest {
            *latest = request.generation;
            states.retain(|key, _| key.0 != request.scope || key.3 == request.generation);
            pending.retain(|item| {
                item.0.request.scope != request.scope
                    || item.0.request.generation == request.generation
            });
        }
        if request.generation != *latest {
            continue;
        }
        let key = (
            request.scope,
            request.provider,
            request.id,
            request.generation,
        );
        let attempts = match states.get(&key).map(|x| &x.0) {
            Some(RequestState::Queued | RequestState::InFlight | RequestState::Ready) => continue,
            Some(RequestState::Failed {
                retry_at: Some(retry_at),
                ..
            }) if Instant::now() < *retry_at => continue,
            Some(RequestState::Failed { retry_at: None, .. }) => continue,
            Some(RequestState::Failed { attempts, .. }) => *attempts,
            None => 0,
        };
        let work = Work {
            request,
            attempts,
            url: item.url,
            epoch,
            sequence: item.sequence,
        };
        if pending.len() == QUEUE_CAPACITY {
            let mut entries = std::mem::take(pending).into_vec();
            let worst = entries
                .iter()
                .enumerate()
                .max_by_key(|(_, item)| (item.0.request.priority, item.0.sequence))
                .map(|(index, _)| index)
                .unwrap();
            let worst_rank = (entries[worst].0.request.priority, entries[worst].0.sequence);
            if (work.request.priority, work.sequence) >= worst_rank {
                *pending = BinaryHeap::from(entries);
                continue;
            }
            let evicted = entries.swap_remove(worst).0;
            let evicted_key = (
                evicted.request.scope,
                evicted.request.provider,
                evicted.request.id,
                evicted.request.generation,
            );
            if states.get(&evicted_key).is_some_and(|(state, token)| {
                matches!(state, RequestState::Queued) && *token == evicted.sequence
            }) {
                states.remove(&evicted_key);
            }
            *pending = BinaryHeap::from(entries);
        }
        states.insert(key, (RequestState::Queued, work.sequence));
        pending.push(Pending(work));
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
    ready_order: &mut VecDeque<Key>,
    failed_order: &mut VecDeque<(Key, u64)>,
    cache: &mut TileDiskCache,
    ready_tx: &Sender<ReadyEnvelope>,
    ready_evict_rx: &Receiver<ReadyEnvelope>,
    repaint: &Arc<dyn Fn() + Send + Sync>,
    epoch: u64,
    latest_generations: &HashMap<MapScopeId, u64>,
    desired: &HashMap<MapScopeId, DesiredSnapshot>,
    authoritative_desired: &Mutex<HashMap<MapScopeId, DesiredSnapshot>>,
    _status_snapshot: &Mutex<TileManagerStatus>,
) {
    idle.push(std::cmp::Reverse(completion.worker));
    let work = completion.work;
    let key = (
        work.request.scope,
        work.request.provider,
        work.request.id,
        work.request.generation,
    );
    let current = states
        .get(&key)
        .is_some_and(|(_, token)| *token == work.sequence);
    let authoritative_accepts = authoritative_desired
        .lock()
        .unwrap()
        .get(&work.request.scope)
        .map_or_else(
            || !desired.contains_key(&work.request.scope),
            |snapshot| snapshot.accepts(&key, work.sequence),
        );
    let accepted = current
        && work.epoch == epoch
        && latest_generations.get(&work.request.scope).copied() == Some(work.request.generation)
        && desired
            .get(&work.request.scope)
            .is_none_or(|snapshot| snapshot.accepts(&key, work.sequence))
        && authoritative_accepts;
    {
        let mut status = _status_snapshot.lock().unwrap();
        status.completions_processed = status.completions_processed.saturating_add(1);
        if !accepted {
            status.stale_completions_discarded =
                status.stale_completions_discarded.saturating_add(1);
        }
    }
    if accepted {
        match completion.result {
            Ok((bytes, rgba)) => {
                if let Err(error) = cache.write(work.request.provider, work.request.id, &bytes) {
                    _status_snapshot.lock().unwrap().failure = Some(TileFailure {
                        class: TileFailureClass::Cache,
                        retryable: false,
                        message: error.to_string(),
                    });
                } else {
                    let mut status = _status_snapshot.lock().unwrap();
                    if matches!(
                        status.failure.as_ref().map(|failure| failure.class),
                        Some(TileFailureClass::Cache)
                    ) {
                        status.failure = None;
                    }
                }
                retain_ready(states, ready_order, key, work.sequence);
                send_ready(
                    ready_tx,
                    ready_evict_rx,
                    ReadyEnvelope {
                        epoch: work.epoch,
                        sequence: work.sequence,
                        tile: ReadyTile {
                            scope: work.request.scope,
                            epoch: work.epoch,
                            provider: work.request.provider,
                            id: work.request.id,
                            generation: work.request.generation,
                            rgba,
                            corners: work.request.corners,
                        },
                    },
                );
            }
            Err(failure) => mark_failed(states, failed_order, key, &work, failure),
        }
        repaint();
    }
}

fn retain_ready(
    states: &mut HashMap<Key, (RequestState, u64)>,
    ready_order: &mut VecDeque<Key>,
    key: Key,
    sequence: u64,
) {
    states.insert(key, (RequestState::Ready, sequence));
    ready_order.push_back(key);
    while ready_order.len() > READY_CAPACITY {
        if let Some(evicted) = ready_order.pop_front()
            && states
                .get(&evicted)
                .is_some_and(|(state, _)| matches!(state, RequestState::Ready))
        {
            states.remove(&evicted);
        }
    }
}

fn send_ready(
    tx: &Sender<ReadyEnvelope>,
    evict_rx: &Receiver<ReadyEnvelope>,
    ready: ReadyEnvelope,
) {
    if let Err(crossbeam_channel::TrySendError::Full(ready)) = tx.try_send(ready) {
        let _ = evict_rx.try_recv();
        let _ = tx.try_send(ready);
    }
}

fn mark_failed(
    states: &mut HashMap<Key, (RequestState, u64)>,
    failed_order: &mut VecDeque<(Key, u64)>,
    key: Key,
    work: &Work,
    failure: TileFailure,
) {
    let attempts = work.attempts.saturating_add(1);
    states.insert(
        key,
        (
            RequestState::Failed {
                retry_at: failure
                    .retryable
                    .then(|| Instant::now() + retry_delay(attempts)),
                attempts,
                failure,
            },
            work.sequence,
        ),
    );
    failed_order.push_back((key, work.sequence));
    while failed_order.len() > FAILURE_CAPACITY {
        if let Some((evicted, token)) = failed_order.pop_front()
            && states.get(&evicted).is_some_and(|(state, current)| {
                matches!(state, RequestState::Failed { .. }) && *current == token
            })
        {
            states.remove(&evicted);
        }
    }
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
    let state_failure = states
        .values()
        .filter_map(|(state, _)| match state {
            RequestState::Failed { failure, .. } => Some(failure.clone()),
            _ => None,
        })
        .min_by_key(|failure| match failure.class {
            TileFailureClass::Cache => 0,
            TileFailureClass::NetworkTransient => 1,
            TileFailureClass::Permanent => 2,
        });
    if !matches!(
        status.failure.as_ref().map(|f| f.class),
        Some(TileFailureClass::Cache)
    ) {
        status.failure = state_failure;
    }
}

fn classify_http_failure(status: reqwest::StatusCode) -> TileFailure {
    let retryable = status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
    TileFailure {
        class: if retryable {
            TileFailureClass::NetworkTransient
        } else {
            TileFailureClass::Permanent
        },
        retryable,
        message: format!("HTTP {status}"),
    }
}

fn is_jpeg_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("image/jpeg"))
}

fn permanent(message: impl Into<String>) -> TileFailure {
    TileFailure {
        class: TileFailureClass::Permanent,
        retryable: false,
        message: message.into(),
    }
}

fn download(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>, TileFailure> {
    let response = client.get(url).send().map_err(|e| TileFailure {
        class: TileFailureClass::NetworkTransient,
        retryable: true,
        message: e.to_string(),
    })?;
    if !response.status().is_success() {
        return Err(classify_http_failure(response.status()));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !is_jpeg_content_type(content_type) {
        return Err(permanent(format!("unexpected content type {content_type}")));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES)
    {
        return Err(permanent("tile exceeds 2 MiB"));
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| TileFailure {
            class: TileFailureClass::NetworkTransient,
            retryable: true,
            message: e.to_string(),
        })?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(permanent("tile exceeds 2 MiB"));
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

pub fn request_from_test(manager: &mut TileManager, request: TileRequest, url: String) {
    manager.request_with_url(request, Some(url));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
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
}
