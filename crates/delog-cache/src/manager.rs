use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use delog_core::identity::FieldId;
use delog_core::mem::MemBreakdown;
use delog_core::metrics::MetricsRegistry;
use delog_core::snapshot::StoreSnapshot;

use crate::trace::TraceCache;

pub const DEFAULT_BUDGET_BYTES: u64 = 1 << 30;

enum Slot {
    Building,
    Ready(TraceCache),
}

pub struct CacheManager {
    caches: HashMap<FieldId, Slot>,
    /// Plotted fields — pinned against eviction.
    pinned: HashSet<FieldId>,
    budget_bytes: u64,
    frame: u64,
    origin_us: i64,
    built_tx: Sender<(FieldId, Option<TraceCache>)>,
    built_rx: Receiver<(FieldId, Option<TraceCache>)>,
    metrics: Arc<MetricsRegistry>,
}

impl CacheManager {
    pub fn new() -> Self {
        Self::with_budget(DEFAULT_BUDGET_BYTES)
    }

    pub fn with_budget(budget_bytes: u64) -> Self {
        let (built_tx, built_rx) = channel();
        Self {
            caches: HashMap::new(),
            pinned: HashSet::new(),
            budget_bytes,
            frame: 0,
            origin_us: 0,
            built_tx,
            built_rx,
            metrics: Arc::new(MetricsRegistry::new()),
        }
    }

    pub fn with_metrics(mut self, metrics: Arc<MetricsRegistry>) -> Self {
        self.metrics = metrics;
        self
    }

    pub fn begin_frame(&mut self, frame: u64) {
        self.frame = frame;
    }

    /// A changed origin invalidates every cache: x values were rebased against
    /// the old origin, forcing a rebuild.
    pub fn set_origin(&mut self, origin_us: i64) {
        if origin_us != self.origin_us {
            self.origin_us = origin_us;
            self.caches.clear();
        }
    }

    pub fn request(&mut self, field: FieldId, snapshot: &Arc<StoreSnapshot>) {
        self.pinned.insert(field);
        if self.caches.contains_key(&field) {
            return;
        }
        self.caches.insert(field, Slot::Building);
        let tx = self.built_tx.clone();
        let snap = Arc::clone(snapshot);
        let origin = self.origin_us;
        let frame = self.frame;
        let metrics = Arc::clone(&self.metrics);
        std::thread::Builder::new()
            .name("delog-cache-build".into())
            .spawn(move || {
                let cache = {
                    let _t = metrics.scope("cache_build");
                    TraceCache::build(&snap, field, origin, frame, &metrics)
                };
                let _ = tx.send((field, cache));
            })
            .expect("spawn cache build thread");
    }

    pub fn unpin(&mut self, field: FieldId) {
        self.pinned.remove(&field);
    }

    /// Drain finished builds into ready slots; a build that found no data
    /// removes its slot so a later request retries. Returns fields whose build
    /// produced no cache. Call once per frame.
    pub fn poll_builds(&mut self) -> Vec<FieldId> {
        let mut empty = Vec::new();
        while let Ok((field, result)) = self.built_rx.try_recv() {
            match result {
                Some(cache) => {
                    self.caches.insert(field, Slot::Ready(cache));
                }
                None => {
                    self.caches.remove(&field);
                    empty.push(field);
                }
            }
        }
        empty
    }

    pub fn on_epoch(&mut self, snapshot: &StoreSnapshot) {
        // A changed source offset makes the cache's x values stale: drop the
        // slot (per-frame `request` rebuilds it). Appending would mix offsets.
        self.caches.retain(|&field, slot| match slot {
            Slot::Ready(cache) => !cache.offset_changed(snapshot, field),
            Slot::Building => true,
        });
        let metrics = Arc::clone(&self.metrics);
        for (&field, slot) in self.caches.iter_mut() {
            if let Slot::Ready(cache) = slot {
                let _t = metrics.scope("cache_append");
                cache.append(snapshot, field, &metrics);
            }
        }
        self.gc(snapshot);
    }

    fn gc(&mut self, snapshot: &StoreSnapshot) {
        self.caches
            .retain(|&field, _| snapshot.is_field_live(field));
        self.pinned.retain(|&field| snapshot.is_field_live(field));
    }

    pub fn get(&mut self, field: FieldId) -> Option<&TraceCache> {
        match self.caches.get_mut(&field)? {
            Slot::Ready(cache) => {
                cache.touch(self.frame);
                Some(cache)
            }
            Slot::Building => None,
        }
    }

    pub fn is_building(&self, field: FieldId) -> bool {
        matches!(self.caches.get(&field), Some(Slot::Building))
    }

    pub fn is_ready(&self, field: FieldId) -> bool {
        matches!(self.caches.get(&field), Some(Slot::Ready(_)))
    }

    /// Evict least-recently-used unpinned ready caches until within budget;
    /// pinned caches are never evicted.
    pub fn evict_over_budget(&mut self) {
        while self.total_cache_bytes() > self.budget_bytes {
            let victim = self
                .caches
                .iter()
                .filter(|(field, slot)| {
                    !self.pinned.contains(field) && matches!(slot, Slot::Ready(_))
                })
                .min_by_key(|(_, slot)| match slot {
                    Slot::Ready(c) => c.last_used_frame,
                    Slot::Building => u64::MAX,
                })
                .map(|(&field, _)| field);
            match victim {
                Some(field) => {
                    self.caches.remove(&field);
                }
                None => break,
            }
        }
    }

    pub fn total_cache_bytes(&self) -> u64 {
        self.caches
            .values()
            .filter_map(|s| match s {
                Slot::Ready(c) => Some(c.bytes()),
                Slot::Building => None,
            })
            .sum()
    }

    pub fn field_mem(&self, field: FieldId) -> MemBreakdown {
        let bytes = match self.caches.get(&field) {
            Some(Slot::Ready(c)) => c.bytes(),
            _ => 0,
        };
        MemBreakdown {
            cache_cpu: bytes,
            ..MemBreakdown::ZERO
        }
    }

    pub fn field_samples(&self, field: FieldId) -> Option<usize> {
        match self.caches.get(&field) {
            Some(Slot::Ready(c)) => Some(c.samples()),
            _ => None,
        }
    }

    pub fn field_visible_samples(&self, field: FieldId, x0: f32, x1: f32) -> Option<usize> {
        match self.caches.get(&field) {
            Some(Slot::Ready(c)) => {
                let (a, b) = c.index_range(x0, x1);
                Some(b.saturating_sub(a))
            }
            _ => None,
        }
    }

    pub fn ready_count(&self) -> usize {
        self.caches
            .values()
            .filter(|s| matches!(s, Slot::Ready(_)))
            .count()
    }
}

impl Default for CacheManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Int32Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::{FieldId, IdentityRegistry, SourceId, TopicId};
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;

    use super::*;

    fn schema() -> Arc<TopicSchema> {
        Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Int32, Some("cm"), 0.01).unwrap()],
            )
            .unwrap(),
        )
    }

    fn snapshot(
        rows: i64,
    ) -> (
        IdentityRegistry,
        Arc<StoreSnapshot>,
        SourceId,
        TopicId,
        FieldId,
    ) {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "BARO").unwrap();
        let field = identity.add_field(topic, "Alt").unwrap();
        let cols: Vec<ArrayRef> = vec![Arc::new(Int32Array::from(
            (0..rows as i32).collect::<Vec<_>>(),
        ))];
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from((0..rows).collect::<Vec<_>>()),
                cols,
                &schema(),
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema(), [chunk]).unwrap());
        let snap = Arc::new(StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap());
        (identity, snap, source, topic, field)
    }

    fn await_ready(mgr: &mut CacheManager, field: FieldId) {
        for _ in 0..2_000 {
            mgr.poll_builds();
            if mgr.is_ready(field) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("cache never became ready");
    }

    #[test]
    fn async_build_becomes_ready_and_get_returns_the_cache() {
        let (_id, snap, _src, _topic, field) = snapshot(500);
        let mut mgr = CacheManager::new();

        mgr.request(field, &snap);
        assert!(mgr.is_building(field));
        await_ready(&mut mgr, field);

        let cache = mgr.get(field).expect("ready");
        assert_eq!(cache.samples(), 500);
        assert!(mgr.total_cache_bytes() > 0);
        assert!(mgr.field_mem(field).cache_cpu > 0);
    }

    #[test]
    fn removing_a_source_gcs_its_cache() {
        let (mut identity, snap, source, _topic, field) = snapshot(64);
        let mut mgr = CacheManager::new();
        mgr.request(field, &snap);
        await_ready(&mut mgr, field);
        assert!(mgr.is_ready(field));

        identity.remove_source(source);
        let after = StoreSnapshot::from_registry(&identity, [], 0).unwrap();
        mgr.on_epoch(&after);

        assert!(!mgr.is_ready(field));
        assert_eq!(mgr.ready_count(), 0);
    }

    #[test]
    fn changing_a_source_offset_invalidates_the_cache() {
        let (mut identity, snap, source, topic, field) = snapshot(64);
        let mut mgr = CacheManager::new();
        mgr.request(field, &snap);
        await_ready(&mut mgr, field);
        assert!(mgr.is_ready(field));

        // Same data, new offset: the cache baked the old offset into its x
        // values, so it must be dropped for a rebuild.
        identity.set_source_offset_us(source, 5_000);
        let store = Arc::clone(snap.topic_store(topic).unwrap());
        let after = StoreSnapshot::from_registry(&identity, [(topic, store)], 1).unwrap();
        mgr.on_epoch(&after);

        assert!(!mgr.is_ready(field), "stale-offset cache must be dropped");
    }

    #[test]
    fn lru_evicts_unpinned_caches_over_budget_but_keeps_pinned() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "BARO").unwrap();
        let a = identity.add_field(topic, "A").unwrap();
        let b = identity.add_field(topic, "B").unwrap();
        let s = Arc::new(
            TopicSchema::new(
                "BARO",
                [
                    FieldSchema::new("A", DataType::Int32, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("B", DataType::Int32, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Int32Array::from((0..1000).collect::<Vec<_>>())),
            Arc::new(Int32Array::from((0..1000).collect::<Vec<_>>())),
        ];
        let chunk = Arc::new(
            Chunk::try_new(Int64Array::from((0..1000).collect::<Vec<_>>()), cols, &s).unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&s), [chunk]).unwrap());
        let snap = Arc::new(StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap());

        let mut mgr = CacheManager::with_budget(4 * 1024);
        mgr.begin_frame(1);
        mgr.request(a, &snap);
        await_ready(&mut mgr, a);
        mgr.get(a);
        mgr.begin_frame(2);
        mgr.request(b, &snap);
        await_ready(&mut mgr, b);
        mgr.get(b); // touched more recently than A

        mgr.unpin(a);
        mgr.evict_over_budget();
        assert!(!mgr.is_ready(a), "unpinned LRU cache should be evicted");
        assert!(mgr.is_ready(b), "pinned cache must survive");
    }
}
