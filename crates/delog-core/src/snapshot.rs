//! Immutable store snapshots and wait-free publication (PLAN.md §4.4).

use std::error::Error;
use std::fmt;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;

use crate::identity::{
    FieldEntry, FieldId, IdentityRegistry, SourceEntry, SourceId, TopicEntry, TopicId,
};
use crate::store::TopicStore;
use crate::time::TimeRange;

/// One source row inside a deeply immutable snapshot.
#[derive(Debug, Clone)]
pub struct SourceSnapshot {
    pub entry: SourceEntry,
    pub topics: Arc<[TopicId]>,
}

/// One topic row inside a deeply immutable snapshot.
#[derive(Debug, Clone)]
pub struct TopicSnapshot {
    pub entry: TopicEntry,
    pub store: Option<Arc<TopicStore>>,
}

/// Coherent, deeply immutable view of the current store.
#[derive(Debug, Clone)]
pub struct StoreSnapshot {
    pub sources: Arc<[SourceSnapshot]>,
    pub topics: Arc<[TopicSnapshot]>,
    pub fields: Arc<[FieldEntry]>,
    pub epoch: u64,
    /// Offset-applied union of every topic's time range, computed once when the
    /// snapshot is built. `global_time_range()` reads this in O(1); computing it
    /// lazily was O(total chunks across all topics) and the UI calls it several
    /// times per frame, so it dominated frame time as live chunks accumulated.
    global_range: Option<TimeRange>,
}

/// A push callback invoked with the new epoch on every publish.
pub type EpochListener = Arc<dyn Fn(u64) + Send + Sync>;

/// Published data store. Readers call [`DataStore::load`] once per frame/job
/// and hold the returned `Arc<StoreSnapshot>` without blocking the writer.
pub struct DataStore {
    current: ArcSwap<StoreSnapshot>,
    /// Pull subscribers (poll a channel — CORE-06).
    subscribers: Mutex<Vec<Sender<u64>>>,
    /// Push listeners (epoch callbacks — ING-06): UI repaint, cache GC/append.
    listeners: Mutex<Vec<EpochListener>>,
}

impl fmt::Debug for DataStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DataStore")
            .field("epoch", &self.current_epoch())
            .finish_non_exhaustive()
    }
}

/// Snapshot construction failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    InvalidTopicId(TopicId),
    DuplicateTopicStore(TopicId),
    TopicStoreSchemaMismatch {
        topic: TopicId,
        expected: String,
        actual: String,
    },
}

/// Snapshot publication failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataStoreError {
    EpochOverflow,
}

impl StoreSnapshot {
    pub fn empty() -> Self {
        Self::from_registry(&IdentityRegistry::new(), [], 0).expect("empty registry is valid")
    }

    pub fn from_registry(
        identity: &IdentityRegistry,
        topic_stores: impl IntoIterator<Item = (TopicId, Arc<TopicStore>)>,
        epoch: u64,
    ) -> Result<Self, SnapshotError> {
        let mut stores = vec![None; identity.topics().len()];
        for (topic_id, store) in topic_stores {
            let topic = identity
                .topic(topic_id)
                .ok_or(SnapshotError::InvalidTopicId(topic_id))?;
            if store.schema.name() != topic.name {
                return Err(SnapshotError::TopicStoreSchemaMismatch {
                    topic: topic_id,
                    expected: topic.name.clone(),
                    actual: store.schema.name().to_owned(),
                });
            }

            let slot = stores
                .get_mut(topic_id.index())
                .ok_or(SnapshotError::InvalidTopicId(topic_id))?;
            if slot.is_some() {
                return Err(SnapshotError::DuplicateTopicStore(topic_id));
            }
            *slot = Some(store);
        }

        let sources: Vec<_> = identity
            .sources()
            .iter()
            .cloned()
            .map(|entry| {
                let topics = identity
                    .topics()
                    .iter()
                    .filter_map(|topic| (topic.source == entry.id).then_some(topic.id))
                    .collect::<Vec<_>>();
                SourceSnapshot {
                    entry,
                    topics: Arc::from(topics),
                }
            })
            .collect();

        let topics: Vec<_> = identity
            .topics()
            .iter()
            .cloned()
            .zip(stores)
            .map(|(entry, store)| TopicSnapshot { entry, store })
            .collect();

        let mut snapshot = Self {
            sources: Arc::from(sources),
            topics: Arc::from(topics),
            fields: Arc::from(identity.fields().to_vec()),
            epoch,
            global_range: None,
        };
        snapshot.global_range = snapshot.compute_global_range();
        Ok(snapshot)
    }

    pub fn source(&self, id: SourceId) -> Option<&SourceSnapshot> {
        self.sources
            .get(id.index())
            .filter(|source| source.entry.id == id)
    }

    pub fn topic(&self, id: TopicId) -> Option<&TopicSnapshot> {
        self.topics
            .get(id.index())
            .filter(|topic| topic.entry.id == id)
    }

    pub fn topic_store(&self, id: TopicId) -> Option<&Arc<TopicStore>> {
        self.topic(id)?.store.as_ref()
    }

    /// Whether `id` names a source present and not tombstoned (§4.6). Readers
    /// (browser rows) and the cache GC use these to skip removed entities.
    pub fn is_source_live(&self, id: SourceId) -> bool {
        self.source(id).is_some_and(|s| !s.entry.removed)
    }

    pub fn is_topic_live(&self, id: TopicId) -> bool {
        self.topic(id).is_some_and(|t| !t.entry.removed)
    }

    pub fn is_field_live(&self, id: FieldId) -> bool {
        self.fields
            .get(id.index())
            .filter(|entry| entry.id == id)
            .is_some_and(|entry| !entry.removed)
    }

    /// Offset-applied union of every topic's time range. O(1): returns the
    /// value computed once at construction by [`Self::compute_global_range`].
    pub fn global_time_range(&self) -> Option<TimeRange> {
        self.global_range
    }

    /// The O(total chunks) scan behind [`Self::global_time_range`], run once
    /// when the snapshot is built.
    fn compute_global_range(&self) -> Option<TimeRange> {
        let mut out: Option<TimeRange> = None;
        for topic in self.topics.iter() {
            let Some(store) = topic.store.as_ref() else {
                continue;
            };
            let Some(raw_range) = store.time_range() else {
                continue;
            };
            let Some(source) = self.source(topic.entry.source) else {
                continue;
            };
            let effective = raw_range.offset(source.entry.offset_us)?;
            out = Some(match out {
                Some(current) => current.union(effective),
                None => effective,
            });
        }
        out
    }
}

impl DataStore {
    pub fn new() -> Self {
        Self::from_snapshot(StoreSnapshot::empty())
    }

    pub fn from_snapshot(mut snapshot: StoreSnapshot) -> Self {
        snapshot.epoch = 0;
        Self {
            current: ArcSwap::from_pointee(snapshot),
            subscribers: Mutex::new(Vec::new()),
            listeners: Mutex::new(Vec::new()),
        }
    }

    pub fn load(&self) -> Arc<StoreSnapshot> {
        self.current.load_full()
    }

    pub fn current_epoch(&self) -> u64 {
        self.current.load().epoch
    }

    pub fn subscribe(&self) -> Receiver<u64> {
        let (tx, rx) = mpsc::channel();
        self.subscribers
            .lock()
            .expect("subscriber list poisoned")
            .push(tx);
        rx
    }

    /// Register a push callback fired with the new epoch on every publish
    /// (ING-06): the app registers `ctx.request_repaint`, the cache manager its
    /// incremental-append/GC trigger. Callbacks run on the ingest thread and
    /// must be cheap and non-blocking — and must not re-enter the store.
    pub fn on_epoch(&self, listener: impl Fn(u64) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .expect("listener list poisoned")
            .push(Arc::new(listener));
    }

    pub fn publish(
        &self,
        mut snapshot: StoreSnapshot,
    ) -> Result<Arc<StoreSnapshot>, DataStoreError> {
        let next_epoch = self
            .current
            .load()
            .epoch
            .checked_add(1)
            .ok_or(DataStoreError::EpochOverflow)?;
        snapshot.epoch = next_epoch;

        let snapshot = Arc::new(snapshot);
        self.current.store(Arc::clone(&snapshot));
        self.notify(next_epoch);
        Ok(snapshot)
    }

    fn notify(&self, epoch: u64) {
        {
            let mut subscribers = self.subscribers.lock().expect("subscriber list poisoned");
            subscribers.retain(|tx| tx.send(epoch).is_ok());
        }
        // Clone the listener Arcs out, then invoke them with the lock released,
        // so a callback can never deadlock by touching the listener list.
        let listeners: Vec<EpochListener> = {
            let guard = self.listeners.lock().expect("listener list poisoned");
            guard.clone()
        };
        for listener in listeners {
            listener(epoch);
        }
    }
}

impl Default for DataStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTopicId(id) => write!(f, "invalid topic id {id:?}"),
            Self::DuplicateTopicStore(id) => write!(f, "duplicate topic store for {id:?}"),
            Self::TopicStoreSchemaMismatch {
                topic,
                expected,
                actual,
            } => write!(
                f,
                "topic {topic:?} schema mismatch: expected `{expected}`, got `{actual}`"
            ),
        }
    }
}

impl Error for SnapshotError {}

impl fmt::Display for DataStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EpochOverflow => write!(f, "store epoch overflow"),
        }
    }
}

impl Error for DataStoreError {}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc::TryRecvError;

    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;

    use super::*;
    use crate::chunk::Chunk;
    use crate::identity::FieldId;
    use crate::schema::{FieldSchema, TopicSchema};

    fn schema(name: &str) -> Arc<TopicSchema> {
        Arc::new(
            TopicSchema::new(
                name,
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        )
    }

    fn chunk(times: Vec<i64>, values: Vec<f64>, schema: &TopicSchema) -> Arc<Chunk> {
        let cols: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(values))];
        Arc::new(Chunk::try_new(Int64Array::from(times), cols, schema).unwrap())
    }

    fn identity_with_topic() -> (IdentityRegistry, SourceId, TopicId) {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "BARO").unwrap();
        assert_eq!(identity.add_field(topic, "Alt"), Some(FieldId(0)));
        (identity, source, topic)
    }

    fn store_for(topic_name: &str, times: Vec<i64>) -> Arc<TopicStore> {
        let schema = schema(topic_name);
        let chunk = chunk(times.clone(), vec![1.0; times.len()], &schema);
        Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap())
    }

    #[test]
    fn snapshot_copies_identity_and_attaches_topic_stores() {
        let (identity, source, topic) = identity_with_topic();
        let store = store_for("BARO", vec![100, 200]);

        let snapshot =
            StoreSnapshot::from_registry(&identity, [(topic, Arc::clone(&store))], 7).unwrap();

        assert_eq!(snapshot.epoch, 7);
        assert_eq!(snapshot.source(source).unwrap().entry.label, "flight");
        assert_eq!(snapshot.source(source).unwrap().topics.as_ref(), &[topic]);
        assert!(Arc::ptr_eq(snapshot.topic_store(topic).unwrap(), &store));
        assert_eq!(snapshot.fields[0].name, "Alt");
    }

    #[test]
    fn snapshot_rejects_invalid_duplicate_and_mismatched_topic_stores() {
        let (identity, _source, topic) = identity_with_topic();
        let store = store_for("BARO", vec![100]);

        assert_eq!(
            StoreSnapshot::from_registry(&identity, [(TopicId(99), Arc::clone(&store))], 0)
                .unwrap_err(),
            SnapshotError::InvalidTopicId(TopicId(99))
        );
        assert_eq!(
            StoreSnapshot::from_registry(
                &identity,
                [(topic, Arc::clone(&store)), (topic, Arc::clone(&store))],
                0,
            )
            .unwrap_err(),
            SnapshotError::DuplicateTopicStore(topic)
        );

        let wrong_store = store_for("GPS", vec![100]);
        assert_eq!(
            StoreSnapshot::from_registry(&identity, [(topic, wrong_store)], 0).unwrap_err(),
            SnapshotError::TopicStoreSchemaMismatch {
                topic,
                expected: "BARO".to_owned(),
                actual: "GPS".to_owned(),
            }
        );
    }

    #[test]
    fn snapshot_global_time_range_applies_source_offsets() {
        let (mut identity, source, topic) = identity_with_topic();
        assert_eq!(identity.set_source_offset_us(source, -50), Some(0));
        let store = store_for("BARO", vec![100, 200]);

        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();

        assert_eq!(snapshot.global_time_range(), TimeRange::new(50, 150));
    }

    #[test]
    fn removing_a_source_rebuilds_a_snapshot_without_its_data() {
        // Two sources, each a topic. The writer flow: tombstone in the registry,
        // drop the orphaned stores, rebuild the snapshot from what remains.
        let mut identity = IdentityRegistry::new();
        let keep = identity.add_source("keep");
        let drop = identity.add_source("drop");
        let keep_topic = identity.add_topic(keep, "BARO").unwrap();
        identity.add_field(keep_topic, "Alt").unwrap();
        let drop_topic = identity.add_topic(drop, "GPS").unwrap();
        identity.add_field(drop_topic, "Lat").unwrap();

        let mut stores = vec![
            (keep_topic, store_for("BARO", vec![100, 200])),
            (drop_topic, store_for("GPS", vec![300, 400])),
        ];
        let before = StoreSnapshot::from_registry(&identity, stores.clone(), 0).unwrap();
        assert!(before.is_source_live(drop));
        assert!(before.topic_store(drop_topic).is_some());

        let removed = identity.remove_source(drop).unwrap();
        stores.retain(|(topic, _)| !removed.topics.contains(topic));
        let after = StoreSnapshot::from_registry(&identity, stores, 0).unwrap();

        // The dropped source is tombstoned and carries no data; the survivor is
        // intact and its IDs are unchanged.
        assert!(!after.is_source_live(drop));
        assert!(!after.is_topic_live(drop_topic));
        assert!(after.topic_store(drop_topic).is_none());
        assert!(after.is_source_live(keep));
        assert!(after.topic_store(keep_topic).is_some());
        assert_eq!(after.global_time_range(), TimeRange::new(100, 200));

        // The orphaned field is no longer live and a view onto it fails.
        let drop_field = removed.fields[0];
        assert!(!after.is_field_live(drop_field));
    }

    #[test]
    fn data_store_loads_publish_snapshots_and_notifies_epochs() {
        let (identity, _source, topic) = identity_with_topic();
        let initial =
            StoreSnapshot::from_registry(&identity, [(topic, store_for("BARO", vec![100]))], 99)
                .unwrap();
        let data_store = DataStore::from_snapshot(initial);
        let rx = data_store.subscribe();
        let pinned = data_store.load();

        assert_eq!(pinned.epoch, 0);
        assert_eq!(data_store.current_epoch(), 0);

        let next =
            StoreSnapshot::from_registry(&identity, [(topic, store_for("BARO", vec![200]))], 0)
                .unwrap();
        let published = data_store.publish(next).unwrap();

        assert_eq!(published.epoch, 1);
        assert_eq!(data_store.current_epoch(), 1);
        assert_eq!(rx.try_recv(), Ok(1));
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(pinned.epoch, 0);
    }

    #[test]
    fn epoch_listeners_are_pushed_every_published_epoch() {
        use std::sync::Mutex as StdMutex;

        let (identity, _source, topic) = identity_with_topic();
        let data_store = DataStore::new();

        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        data_store.on_epoch(move |epoch| sink.lock().unwrap().push(epoch));

        for _ in 0..3 {
            let next =
                StoreSnapshot::from_registry(&identity, [(topic, store_for("BARO", vec![1]))], 0)
                    .unwrap();
            data_store.publish(next).unwrap();
        }

        assert_eq!(*seen.lock().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn closed_subscribers_are_dropped() {
        let (identity, _source, topic) = identity_with_topic();
        let data_store = DataStore::new();
        let rx = data_store.subscribe();
        drop(rx);

        let next =
            StoreSnapshot::from_registry(&identity, [(topic, store_for("BARO", vec![200]))], 0)
                .unwrap();
        assert_eq!(data_store.publish(next).unwrap().epoch, 1);

        let live_rx = data_store.subscribe();
        let next =
            StoreSnapshot::from_registry(&identity, [(topic, store_for("BARO", vec![300]))], 0)
                .unwrap();
        assert_eq!(data_store.publish(next).unwrap().epoch, 2);
        assert_eq!(live_rx.try_recv(), Ok(2));
    }
}
