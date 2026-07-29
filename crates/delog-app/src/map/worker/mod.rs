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
    #[cfg(test)]
    completions_processed: u64,
    #[cfg(test)]
    stale_completions_discarded: u64,
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

    #[cfg(test)]
    pub(crate) fn test_completion_counts(&self) -> (u64, u64) {
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
        if !shutdown {
            loop {
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
    #[cfg(test)]
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

#[cfg(test)]
mod tests;
