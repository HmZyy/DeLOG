//! The single ingest thread: the only writer of the store, which makes the
//! epoch-snapshot model correct with no locks.
//!
//! Sealing policy: a file source seals at `FILE_CHUNK_ROWS`; a live source seals
//! when its pending rows reach `LIVE_CHUNK_ROWS` or its per-topic pending age
//! reaches `LIVE_MAX_AGE`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow::array::{Array, ArrayRef, Int64Array};
use arrow::compute::{concat, sort_to_indices, take};

use crate::chunk::Chunk;
use crate::derived::{DerivedCommit, DerivedCommitError, DerivedCommitReceipt};
use crate::diagnostics::Diag;
use crate::identity::{IdentityRegistry, SourceId, TopicId};
use crate::ingest::{
    IngestMsg, IngestReceiver, ParseSummary, ParsedBatch, RecvOutcome, SourceKind,
};
use crate::metrics::MetricsRegistry;
use crate::schema::TopicSchema;
use crate::snapshot::{DataStore, StoreSnapshot};
use crate::store::TopicStore;
use crate::time::TimeRange;

pub const FILE_CHUNK_ROWS: usize = 64 * 1024;
pub const LIVE_CHUNK_ROWS: usize = 512;
/// Live source max chunk age before a partial seal.
pub const LIVE_MAX_AGE: Duration = Duration::from_millis(100);

/// Side-channel callbacks; all default to no-ops. Epoch/repaint notification
/// rides the store's own subscriber channel, not here.
pub trait IngestObserver: Send {
    fn on_diagnostic(&mut self, _diag: Diag) {}
    fn on_progress(&mut self, _source: SourceId, _frac: f32) {}
    fn on_close(&mut self, _source: SourceId, _summary: ParseSummary) {}
    fn on_remove(&mut self, _source: SourceId) {}
    /// Runs before the batch is published. `source_label` comes directly from
    /// the ingestor's identity registry and is authoritative at this boundary.
    fn on_batch(&mut self, _kind: SourceKind, _source_label: &str, _batch: &ParsedBatch) {}
}

#[derive(Debug, Default)]
pub struct NullObserver;
impl IngestObserver for NullObserver {}

struct Pending {
    schema: Arc<TopicSchema>,
    timestamps: Vec<Int64Array>,
    columns: Vec<Vec<ArrayRef>>,
    rows: usize,
    first_buffered_at: Instant,
    /// Last timestamp accepted, guards the cross-batch join.
    last_ts: Option<i64>,
}

struct SourceState {
    kind: SourceKind,
    seal_rows: usize,
    topics: HashMap<String, TopicId>,
    pending: HashMap<TopicId, Pending>,
}

pub struct Ingestor<O: IngestObserver> {
    identity: IdentityRegistry,
    store: Arc<DataStore>,
    stores: HashMap<TopicId, Arc<TopicStore>>,
    sources: HashMap<SourceId, SourceState>,
    /// Highest timestamp seen per topic, for the cross-chunk regression check.
    topic_max_ts: HashMap<TopicId, i64>,
    observer: O,
    chunks_sealed: u64,
    rows_ingested: u64,
    metrics: Arc<MetricsRegistry>,
}

impl<O: IngestObserver> Ingestor<O> {
    pub fn new(observer: O) -> Self {
        Self {
            identity: IdentityRegistry::new(),
            store: Arc::new(DataStore::new()),
            stores: HashMap::new(),
            sources: HashMap::new(),
            topic_max_ts: HashMap::new(),
            observer,
            chunks_sealed: 0,
            rows_ingested: 0,
            metrics: Arc::new(MetricsRegistry::new()),
        }
    }

    pub fn with_metrics(mut self, metrics: Arc<MetricsRegistry>) -> Self {
        self.metrics = metrics;
        self
    }

    pub fn store(&self) -> Arc<DataStore> {
        Arc::clone(&self.store)
    }

    pub fn chunks_sealed(&self) -> u64 {
        self.chunks_sealed
    }

    pub fn rows_ingested(&self) -> u64 {
        self.rows_ingested
    }

    /// The idle tick checks each live topic's own pending age, so busy
    /// unrelated topics do not starve seals.
    pub fn run(mut self, rx: IngestReceiver) {
        loop {
            match rx.recv_timeout(LIVE_MAX_AGE) {
                RecvOutcome::Message(msg) => {
                    self.process(msg);
                    self.flush_aged_live();
                }
                RecvOutcome::Idle => self.flush_aged_live(),
                RecvOutcome::Disconnected => break,
            }
        }
        self.flush_all();
    }

    /// Public for step-driven testing.
    pub fn process(&mut self, msg: IngestMsg) {
        match msg {
            IngestMsg::PublicationBarrier { reply } => {
                self.flush_all();
                let _ = reply.send(self.try_publish());
            }

            IngestMsg::OpenSource { key, kind, reply } => {
                let id = self.open_source(&key, kind);
                let _ = reply.send(id);
            }
            IngestMsg::Batch(batch) => self.accept_batch(batch),
            IngestMsg::Diagnostic(diag) => self.observer.on_diagnostic(diag),
            IngestMsg::Progress { source, frac } => self.observer.on_progress(source, frac),
            IngestMsg::CloseSource { source, summary } => {
                self.flush_source(source);
                if !summary.source_meta.is_empty() {
                    self.identity
                        .set_source_metadata(source, summary.source_meta.clone());
                    self.publish();
                }
                self.observer.on_close(source, summary);
            }
            IngestMsg::SetSourceOffset { source, offset_us } => {
                if self
                    .identity
                    .set_source_offset_us(source, offset_us)
                    .is_some_and(|old| old != offset_us)
                {
                    self.publish();
                }
            }
            IngestMsg::SetSourceOffsets { offsets } => {
                if self.set_source_offsets(&offsets) {
                    self.publish();
                }
            }
            IngestMsg::RemoveSource { source } => self.remove_source(source),
            IngestMsg::CommitDerived { commit, reply } => {
                let _ = reply.send(self.commit_derived(commit));
            }
            IngestMsg::RemoveSourceWait { source, reply } => {
                let _ = reply.send(self.remove_source_wait(source));
            }
            IngestMsg::RelabelSource { source, label } => {
                if self.identity.relabel_source(source, label).is_some() {
                    self.publish();
                }
            }
        }
    }

    fn open_source(&mut self, key: &str, kind: SourceKind) -> SourceId {
        let id = self.identity.add_source_with_kind(key, kind);
        let seal_rows = match kind {
            SourceKind::File | SourceKind::Derived => FILE_CHUNK_ROWS,
            SourceKind::Live | SourceKind::LiveDerived => LIVE_CHUNK_ROWS,
        };
        self.sources.insert(
            id,
            SourceState {
                kind,
                seal_rows,
                topics: HashMap::new(),
                pending: HashMap::new(),
            },
        );
        id
    }

    fn set_source_offsets(&mut self, offsets: &[(SourceId, i64)]) -> bool {
        let mut seen = HashSet::with_capacity(offsets.len());
        let valid = offsets.iter().all(|&(id, offset)| {
            seen.insert(id)
                && self.identity.live_source(id).is_some()
                && self.sources.get(&id).is_some_and(|source| {
                    source.pending.values().all(|pending| {
                        pending.timestamps.iter().all(|timestamps| {
                            let values = timestamps.values();
                            let Some((&min_us, &max_us)) = values.first().zip(values.last()) else {
                                return true;
                            };
                            TimeRange::new(min_us, max_us)
                                .is_some_and(|range| range.offset(offset).is_some())
                        })
                    })
                })
                && self
                    .stores
                    .iter()
                    .filter(|(topic, _)| {
                        self.identity
                            .topic(**topic)
                            .is_some_and(|topic| topic.source == id)
                    })
                    .filter_map(|(_, store)| store.time_range())
                    .all(|range| range.offset(offset).is_some())
        });
        if !valid {
            return false;
        }

        let mut changed = false;
        for &(id, offset) in offsets {
            changed |= self
                .identity
                .set_source_offset_us(id, offset)
                .is_some_and(|old| old != offset);
        }
        changed
    }

    fn accept_batch(&mut self, batch: ParsedBatch) {
        if batch.rows() == 0 {
            return;
        }
        let _ingest_timer = self.metrics.scope("ingest_batch");
        let Some(source) = self.sources.get(&batch.source) else {
            self.observer.on_diagnostic(Diag::warning(
                "batch-unknown-source",
                format!("batch for unopened source {:?} dropped", batch.source),
            ));
            return;
        };
        let seal_rows = source.seal_rows;
        let source_kind = source.kind;
        let source_id = batch.source;
        let source_label = self
            .identity
            .source(source_id)
            .expect("opened source has identity")
            .label
            .as_str();
        self.observer.on_batch(source_kind, source_label, &batch);

        let topic_id = match self.ensure_topic(source_id, &batch.schema) {
            Some(id) => id,
            None => return,
        };

        let schema = batch.schema;

        // Defensive within-batch sort: a malformed log may hand us unsorted
        // timestamps. Sorted batches pass through untouched (copy-free).
        let (timestamps, columns) = if is_sorted(&batch.timestamps) {
            (batch.timestamps, batch.columns)
        } else {
            match sort_batch(&batch.timestamps, &batch.columns) {
                Ok(sorted) => {
                    self.observer.on_diagnostic(
                        Diag::warning(
                            "unsorted-batch",
                            format!("topic {topic_id:?}: reordered an unsorted batch"),
                        )
                        .with_source(source_id),
                    );
                    sorted
                }
                Err(err) => {
                    self.observer.on_diagnostic(
                        Diag::error("batch-sort-failed", format!("topic {topic_id:?}: {err}"))
                            .with_source(source_id),
                    );
                    return;
                }
            }
        };

        let batch_first = timestamps.value(0);
        let batch_last = timestamps.value(timestamps.len() - 1);

        // Cross-chunk regression (chunks may overlap): tolerated but reported.
        if let Some(&prev_max) = self.topic_max_ts.get(&topic_id)
            && batch_first < prev_max
        {
            self.observer.on_diagnostic(
                Diag::warning(
                    "timestamp-regression",
                    format!(
                        "topic {topic_id:?}: batch starts at {batch_first} µs, before previous max {prev_max} µs"
                    ),
                )
                .with_source(source_id)
                .at_time(batch_first),
            );
        }
        let max_ts = self.topic_max_ts.entry(topic_id).or_insert(i64::MIN);
        *max_ts = (*max_ts).max(batch_last);

        // Seal first if this batch starts before the accumulator ends, so every
        // sealed chunk stays internally sorted.
        if let Some(pending) = self.pending_mut(source_id, topic_id)
            && pending.last_ts.is_some_and(|last| batch_first < last)
        {
            self.seal_topic(source_id, topic_id);
        }

        let rows = timestamps.len();
        let pending = self.pending_entry(source_id, topic_id, &schema);
        pending.timestamps.push(timestamps);
        pending.columns.push(columns);
        pending.rows += rows;
        pending.last_ts = Some(batch_last);
        let full = pending.rows >= seal_rows;

        self.rows_ingested += rows as u64;
        if full {
            self.seal_topic(source_id, topic_id);
        }
    }

    fn ensure_topic(&mut self, source: SourceId, schema: &Arc<TopicSchema>) -> Option<TopicId> {
        let name = schema.name().to_owned();
        if let Some(state) = self.sources.get(&source)
            && let Some(&id) = state.topics.get(&name)
        {
            return Some(id);
        }

        let topic_id = self.identity.add_topic(source, &name)?;
        for field in schema.fields() {
            self.identity.add_field(topic_id, &field.name);
        }
        self.stores
            .entry(topic_id)
            .or_insert_with(|| Arc::new(TopicStore::new(Arc::clone(schema))));
        self.sources.get_mut(&source)?.topics.insert(name, topic_id);
        Some(topic_id)
    }

    fn pending_mut(&mut self, source: SourceId, topic: TopicId) -> Option<&mut Pending> {
        self.sources.get_mut(&source)?.pending.get_mut(&topic)
    }

    fn pending_entry(
        &mut self,
        source: SourceId,
        topic: TopicId,
        schema: &Arc<TopicSchema>,
    ) -> &mut Pending {
        self.sources
            .get_mut(&source)
            .expect("source registered before batching")
            .pending
            .entry(topic)
            .or_insert_with(|| Pending {
                schema: Arc::clone(schema),
                timestamps: Vec::new(),
                columns: Vec::new(),
                rows: 0,
                first_buffered_at: Instant::now(),
                last_ts: None,
            })
    }

    fn seal_topic(&mut self, source: SourceId, topic: TopicId) {
        let Some(pending) = self
            .sources
            .get_mut(&source)
            .and_then(|state| state.pending.remove(&topic))
        else {
            return;
        };
        if pending.rows == 0 {
            return;
        }

        let schema = Arc::clone(&pending.schema);
        let (timestamps, columns) = match merge_pending(pending) {
            Ok(arrays) => arrays,
            Err(err) => {
                self.observer.on_diagnostic(Diag::error(
                    "chunk-merge-failed",
                    format!("topic {topic:?}: {err}"),
                ));
                return;
            }
        };

        match Chunk::try_new(timestamps, columns, &schema) {
            Ok(chunk) => {
                let current = self
                    .stores
                    .get(&topic)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(TopicStore::new(Arc::clone(&schema))));
                match current.append_chunk(Arc::new(chunk)) {
                    Ok(next) => {
                        self.stores.insert(topic, Arc::new(next));
                        self.chunks_sealed += 1;
                        self.publish();
                    }
                    Err(err) => self.observer.on_diagnostic(Diag::error(
                        "chunk-append-failed",
                        format!("topic {topic:?}: {err}"),
                    )),
                }
            }
            Err(err) => self.observer.on_diagnostic(
                Diag::warning("chunk-seal-failed", format!("topic {topic:?}: {err}"))
                    .with_source(source),
            ),
        }
    }

    fn remove_source(&mut self, source: SourceId) {
        // Flush pending rows first so the seal path does not race the drop.
        self.flush_source(source);
        let Some(removed) = self.identity.remove_source(source) else {
            return;
        };
        for topic in &removed.topics {
            self.stores.remove(topic);
            self.topic_max_ts.remove(topic);
        }
        self.sources.remove(&source);
        self.publish();
        self.observer.on_remove(source);
    }

    fn commit_derived(
        &mut self,
        commit: DerivedCommit,
    ) -> Result<DerivedCommitReceipt, DerivedCommitError> {
        commit.validate()?;

        let mut identity = self.identity.clone();
        let mut stores = self.stores.clone();
        let mut removed_topics = Vec::new();

        if let Some(previous) = commit.previous {
            let previous_entry = identity
                .live_source(previous)
                .ok_or(DerivedCommitError::PreviousSourceNotLive(previous))?;
            let Some(previous_provenance) = previous_entry.derived_provenance.as_ref() else {
                return Err(DerivedCommitError::PreviousSourceNotDerived(previous));
            };
            if previous_entry.kind != SourceKind::Derived {
                return Err(DerivedCommitError::PreviousSourceNotDerived(previous));
            }
            if previous_entry.label != commit.key
                || previous_provenance.owner != commit.provenance.owner
                || previous_provenance.logical_topic != commit.provenance.logical_topic
            {
                return Err(DerivedCommitError::PreviousSourceMismatch(previous));
            }

            let removed = identity
                .remove_source(previous)
                .expect("validated live source remains removable in candidate registry");
            removed_topics = removed.topics;
            for topic in &removed_topics {
                stores.remove(topic);
            }
        } else if identity
            .sources()
            .iter()
            .any(|source| !source.removed && source.label == commit.key)
        {
            return Err(DerivedCommitError::SourceKeyInUse(commit.key));
        }

        let source = identity.add_derived_source(&commit.key, commit.provenance);
        let mut topics = HashMap::with_capacity(commit.source.topics().len());
        let mut topic_max = Vec::with_capacity(commit.source.topics().len());
        for store in commit.source.topics() {
            let topic = identity
                .add_topic(source, store.schema.name())
                .expect("new derived source is live");
            for field in store.schema.fields() {
                identity
                    .add_field(topic, &field.name)
                    .expect("new derived topic is live");
            }
            topics.insert(store.schema.name().to_owned(), topic);
            if let Some(range) = store.time_range() {
                topic_max.push((topic, range.max_us));
            }
            stores.insert(topic, Arc::clone(store));
        }

        let snapshot = StoreSnapshot::from_registry(
            &identity,
            stores.iter().map(|(&id, store)| (id, Arc::clone(store))),
            0,
        )
        .map_err(|error| DerivedCommitError::SnapshotBuild(error.to_string()))?;
        let published = self
            .store
            .publish(snapshot)
            .map_err(|error| DerivedCommitError::StorePublish(error.to_string()))?;

        if let Some(previous) = commit.previous {
            self.sources.remove(&previous);
            for topic in &removed_topics {
                self.topic_max_ts.remove(topic);
            }
        }
        self.identity = identity;
        self.stores = stores;
        self.sources.insert(
            source,
            SourceState {
                kind: SourceKind::Derived,
                seal_rows: FILE_CHUNK_ROWS,
                topics,
                pending: HashMap::new(),
            },
        );
        self.topic_max_ts.extend(topic_max);
        if let Some(previous) = commit.previous {
            self.observer.on_remove(previous);
        }

        Ok(DerivedCommitReceipt {
            source,
            epoch: published.epoch,
        })
    }

    fn remove_source_wait(&mut self, source: SourceId) -> Result<u64, DerivedCommitError> {
        let source_entry = self
            .identity
            .live_source(source)
            .ok_or(DerivedCommitError::SourceNotLive(source))?;
        if source_entry.kind != SourceKind::Derived || source_entry.derived_provenance.is_none() {
            return Err(DerivedCommitError::SourceNotDerived(source));
        }

        let mut identity = self.identity.clone();
        let removed = identity
            .remove_source(source)
            .expect("validated live source remains removable in candidate registry");
        let mut stores = self.stores.clone();
        for topic in &removed.topics {
            stores.remove(topic);
        }

        let snapshot = StoreSnapshot::from_registry(
            &identity,
            stores.iter().map(|(&id, store)| (id, Arc::clone(store))),
            0,
        )
        .map_err(|error| DerivedCommitError::SnapshotBuild(error.to_string()))?;
        let published = self
            .store
            .publish(snapshot)
            .map_err(|error| DerivedCommitError::StorePublish(error.to_string()))?;

        self.identity = identity;
        self.stores = stores;
        self.sources.remove(&source);
        for topic in &removed.topics {
            self.topic_max_ts.remove(topic);
        }
        self.observer.on_remove(source);
        Ok(published.epoch)
    }

    fn flush_source(&mut self, source: SourceId) {
        let topics: Vec<TopicId> = self
            .sources
            .get(&source)
            .map(|state| state.pending.keys().copied().collect())
            .unwrap_or_default();
        for topic in topics {
            self.seal_topic(source, topic);
        }
    }

    fn flush_aged_live(&mut self) {
        let now = Instant::now();
        let stale: Vec<(SourceId, TopicId)> = self
            .sources
            .iter()
            .filter(|(_, state)| matches!(state.kind, SourceKind::Live | SourceKind::LiveDerived))
            .flat_map(|(&source, state)| {
                state.pending.iter().filter_map(move |(&topic, pending)| {
                    (now.duration_since(pending.first_buffered_at) >= LIVE_MAX_AGE)
                        .then_some((source, topic))
                })
            })
            .collect();
        for (source, topic) in stale {
            self.seal_topic(source, topic);
        }
    }

    fn flush_all(&mut self) {
        let sources: Vec<SourceId> = self.sources.keys().copied().collect();
        for source in sources {
            self.flush_source(source);
        }
    }

    fn publish(&self) {
        if let Err(err) = self.try_publish() {
            debug_assert!(false, "snapshot rebuild failed: {err}");
        }
    }

    fn try_publish(&self) -> Result<(), String> {
        let _swap_timer = self.metrics.scope("snapshot_swap");
        let topic_stores = self
            .stores
            .iter()
            .map(|(&id, store)| (id, Arc::clone(store)));
        let snapshot = StoreSnapshot::from_registry(&self.identity, topic_stores, 0)
            .map_err(|e| e.to_string())?;
        self.store.publish(snapshot).map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn is_sorted(timestamps: &Int64Array) -> bool {
    timestamps
        .values()
        .windows(2)
        .all(|pair| pair[0] <= pair[1])
}

fn sort_batch(
    timestamps: &Int64Array,
    columns: &[ArrayRef],
) -> Result<(Int64Array, Vec<ArrayRef>), arrow::error::ArrowError> {
    let indices = sort_to_indices(timestamps, None, None)?;
    let sorted_ts = take(timestamps, &indices, None)?
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("take preserves Int64")
        .clone();
    let sorted_cols = columns
        .iter()
        .map(|col| take(col.as_ref(), &indices, None))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((sorted_ts, sorted_cols))
}

/// A single pending batch is moved through without copying.
fn merge_pending(
    mut pending: Pending,
) -> Result<(Int64Array, Vec<ArrayRef>), arrow::error::ArrowError> {
    if pending.timestamps.len() == 1 {
        let timestamps = pending.timestamps.pop().expect("one timestamp array");
        let columns = pending.columns.pop().expect("one column set");
        return Ok((timestamps, columns));
    }

    let ts_refs: Vec<&dyn Array> = pending.timestamps.iter().map(|a| a as &dyn Array).collect();
    let timestamps = concat(&ts_refs)?
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("concat preserves Int64")
        .clone();

    let field_count = pending.columns.first().map(Vec::len).unwrap_or(0);
    let mut columns = Vec::with_capacity(field_count);
    for col in 0..field_count {
        let refs: Vec<&dyn Array> = pending
            .columns
            .iter()
            .map(|set| set[col].as_ref())
            .collect();
        columns.push(concat(&refs)?);
    }
    Ok((timestamps, columns))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use arrow::array::Float64Array;
    use arrow::datatypes::DataType;

    use super::*;
    use crate::chunk::Chunk;
    use crate::derived::{
        DerivedCommit, DerivedCommitError, DerivedProvenance, PreparedDerivedSource,
    };
    use crate::identity::{AutoMarker, SourceMetadata, SourceParam};
    use crate::ingest::{IngestSink, ingest_channel};
    use crate::schema::FieldSchema;
    use crate::store::TopicStore;
    use crate::time::TimeRange;

    fn schema(name: &str) -> Arc<TopicSchema> {
        Arc::new(
            TopicSchema::new(
                name,
                [FieldSchema::new("V", DataType::Float64, Some("u"), 1.0).unwrap()],
            )
            .unwrap(),
        )
    }

    fn batch(source: SourceId, name: &str, times: &[i64]) -> ParsedBatch {
        let timestamps = Int64Array::from(times.to_vec());
        let columns: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(
            times.iter().map(|t| *t as f64).collect::<Vec<_>>(),
        ))];
        ParsedBatch::new(source, schema(name), timestamps, columns)
    }

    #[derive(Default)]
    struct Recorder {
        diags: Vec<Diag>,
        closes: Vec<(SourceId, ParseSummary)>,
        removes: Vec<SourceId>,
        batches: Vec<(SourceKind, String, String, usize)>,
    }
    impl IngestObserver for &mut Recorder {
        fn on_diagnostic(&mut self, diag: Diag) {
            self.diags.push(diag);
        }
        fn on_close(&mut self, source: SourceId, summary: ParseSummary) {
            self.closes.push((source, summary));
        }
        fn on_remove(&mut self, source: SourceId) {
            self.removes.push(source);
        }
        fn on_batch(&mut self, kind: SourceKind, source_label: &str, batch: &ParsedBatch) {
            self.batches.push((
                kind,
                source_label.to_owned(),
                batch.topic().to_owned(),
                batch.rows(),
            ));
        }
    }

    fn open_with<O: IngestObserver>(
        ing: &mut Ingestor<O>,
        key: &str,
        kind: SourceKind,
    ) -> SourceId {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        ing.process(IngestMsg::OpenSource {
            key: key.to_owned(),
            kind,
            reply: reply_tx,
        });
        reply_rx.recv().unwrap()
    }

    fn open(ing: &mut Ingestor<NullObserver>, key: &str, kind: SourceKind) -> SourceId {
        open_with(ing, key, kind)
    }

    fn prepared_derived_topic(name: &str, values: &[f64]) -> Arc<TopicStore> {
        let schema = schema(name);
        let timestamps = Int64Array::from(
            (0..values.len())
                .map(|index| i64::try_from(index).unwrap() + 1)
                .collect::<Vec<_>>(),
        );
        let columns: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(values.to_vec()))];
        let chunk = Arc::new(Chunk::try_new(timestamps, columns, &schema).unwrap());
        Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap())
    }

    fn derived_commit(
        key: &str,
        owner: &str,
        logical_topic: &str,
        generation: u64,
        previous: Option<SourceId>,
        values: &[f64],
    ) -> DerivedCommit {
        DerivedCommit {
            key: key.to_owned(),
            provenance: DerivedProvenance {
                owner: owner.to_owned(),
                logical_topic: logical_topic.to_owned(),
                generation,
                commit_nonce: [generation as u8; 16],
            },
            previous,
            source: PreparedDerivedSource::try_new([prepared_derived_topic(logical_topic, values)])
                .unwrap(),
        }
    }

    fn process_derived_commit<O: IngestObserver>(
        ing: &mut Ingestor<O>,
        commit: DerivedCommit,
    ) -> Result<crate::derived::DerivedCommitReceipt, DerivedCommitError> {
        let (reply, receipt) = std::sync::mpsc::sync_channel(1);
        ing.process(IngestMsg::CommitDerived { commit, reply });
        receipt.recv().unwrap()
    }

    fn process_derived_remove<O: IngestObserver>(
        ing: &mut Ingestor<O>,
        source: SourceId,
    ) -> Result<u64, DerivedCommitError> {
        let (reply, receipt) = std::sync::mpsc::sync_channel(1);
        ing.process(IngestMsg::RemoveSourceWait { source, reply });
        receipt.recv().unwrap()
    }

    fn first_derived_value(snapshot: &StoreSnapshot, source: SourceId) -> f64 {
        let topic = snapshot
            .source(source)
            .unwrap()
            .topics
            .iter()
            .copied()
            .find(|topic| snapshot.is_topic_live(*topic))
            .unwrap();
        snapshot.topic_store(topic).unwrap().chunks[0].cols[0]
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap()
            .value(0)
    }

    #[test]
    fn derived_replacement_publishes_once_and_old_snapshots_keep_old_data() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let first = process_derived_commit(
            &mut ing,
            derived_commit(
                "external:diagnosis/error",
                "diagnosis",
                "error",
                1,
                None,
                &[1.0],
            ),
        )
        .unwrap();
        let pinned = store.load();
        let epoch_before = pinned.epoch;

        let second = process_derived_commit(
            &mut ing,
            derived_commit(
                "external:diagnosis/error",
                "diagnosis",
                "error",
                2,
                Some(first.source),
                &[2.0],
            ),
        )
        .unwrap();
        let current = store.load();

        assert_eq!(second.epoch, epoch_before + 1);
        assert_eq!(first_derived_value(&pinned, first.source), 1.0);
        assert_eq!(first_derived_value(&current, second.source), 2.0);
        assert!(!current.is_source_live(first.source));
        assert_eq!(
            current
                .source(second.source)
                .unwrap()
                .entry
                .derived_provenance
                .as_ref()
                .unwrap()
                .generation,
            2
        );
    }

    #[test]
    fn derived_prepare_rejects_duplicate_topic_names_before_mutation() {
        let error = PreparedDerivedSource::try_new([
            prepared_derived_topic("error", &[1.0]),
            prepared_derived_topic("error", &[2.0]),
        ])
        .unwrap_err();

        assert_eq!(
            error,
            DerivedCommitError::DuplicateTopicName("error".to_owned())
        );
    }

    #[test]
    fn derived_prepare_requires_at_least_one_non_empty_topic() {
        assert_eq!(
            PreparedDerivedSource::try_new([]).unwrap_err(),
            DerivedCommitError::EmptySource
        );
        assert_eq!(
            PreparedDerivedSource::try_new([Arc::new(TopicStore::new(schema("error")))])
                .unwrap_err(),
            DerivedCommitError::EmptySource
        );
    }

    #[test]
    fn derived_commit_validates_key_owner_topic_and_generation_before_mutation() {
        let cases = [
            (
                derived_commit(" ", "diagnosis", "error", 1, None, &[1.0]),
                DerivedCommitError::EmptyKey,
            ),
            (
                derived_commit("external:diagnosis/error", " ", "error", 1, None, &[1.0]),
                DerivedCommitError::EmptyOwner,
            ),
            (
                derived_commit(
                    "external:diagnosis/error",
                    "diagnosis",
                    " ",
                    1,
                    None,
                    &[1.0],
                ),
                DerivedCommitError::EmptyLogicalTopic,
            ),
            (
                derived_commit(
                    "external:diagnosis/error",
                    "diagnosis",
                    "error",
                    0,
                    None,
                    &[1.0],
                ),
                DerivedCommitError::ZeroGeneration,
            ),
        ];

        for (commit, expected) in cases {
            let mut ing = Ingestor::new(NullObserver);
            let epoch = ing.store().current_epoch();
            assert_eq!(process_derived_commit(&mut ing, commit), Err(expected));
            assert_eq!(ing.store().current_epoch(), epoch);
            assert!(ing.identity.sources().is_empty());
        }
    }

    #[test]
    fn derived_sender_reports_disconnect_before_returning_a_receipt() {
        let (sender, receiver) = ingest_channel();
        drop(receiver);

        assert_eq!(
            sender
                .commit_derived(derived_commit(
                    "external:diagnosis/error",
                    "diagnosis",
                    "error",
                    1,
                    None,
                    &[1.0],
                ))
                .unwrap_err(),
            crate::ingest::IngestDisconnected
        );
        assert_eq!(
            sender.remove_source_wait(SourceId(7)).unwrap_err(),
            crate::ingest::IngestDisconnected
        );
    }

    #[test]
    fn derived_replace_rejects_stale_and_non_derived_previous_sources() {
        let mut ing = Ingestor::new(NullObserver);
        let first = process_derived_commit(
            &mut ing,
            derived_commit(
                "external:diagnosis/error",
                "diagnosis",
                "error",
                1,
                None,
                &[1.0],
            ),
        )
        .unwrap();
        process_derived_remove(&mut ing, first.source).unwrap();

        let stale = process_derived_commit(
            &mut ing,
            derived_commit(
                "external:diagnosis/error",
                "diagnosis",
                "error",
                2,
                Some(first.source),
                &[2.0],
            ),
        )
        .unwrap_err();
        assert_eq!(
            stale,
            DerivedCommitError::PreviousSourceNotLive(first.source)
        );

        let file = open(&mut ing, "flight", SourceKind::File);
        let not_derived = process_derived_commit(
            &mut ing,
            derived_commit(
                "external:diagnosis/error",
                "diagnosis",
                "error",
                2,
                Some(file),
                &[2.0],
            ),
        )
        .unwrap_err();
        assert_eq!(
            not_derived,
            DerivedCommitError::PreviousSourceNotDerived(file)
        );
    }

    #[test]
    fn derived_snapshot_build_failure_preserves_the_old_generation() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let first = process_derived_commit(
            &mut ing,
            derived_commit(
                "external:diagnosis/error",
                "diagnosis",
                "error",
                1,
                None,
                &[1.0],
            ),
        )
        .unwrap();
        let before = store.load();
        ing.stores
            .insert(TopicId(u32::MAX), prepared_derived_topic("invalid", &[9.0]));

        let error = process_derived_commit(
            &mut ing,
            derived_commit(
                "external:diagnosis/error",
                "diagnosis",
                "error",
                2,
                Some(first.source),
                &[2.0],
            ),
        )
        .unwrap_err();

        assert!(matches!(error, DerivedCommitError::SnapshotBuild(_)));
        let after = store.load();
        assert_eq!(after.epoch, before.epoch);
        assert!(after.is_source_live(first.source));
        assert_eq!(first_derived_value(&after, first.source), 1.0);
        assert!(ing.identity.live_source(first.source).is_some());
    }

    #[test]
    fn derived_remove_publishes_once_and_acknowledges_the_new_epoch() {
        let mut recorder = Recorder::default();
        let mut ing = Ingestor::new(&mut recorder);
        let store = ing.store();
        let first = process_derived_commit(
            &mut ing,
            derived_commit(
                "external:diagnosis/error",
                "diagnosis",
                "error",
                1,
                None,
                &[1.0],
            ),
        )
        .unwrap();
        let before = store.load().epoch;

        let removed_epoch = process_derived_remove(&mut ing, first.source).unwrap();

        assert_eq!(removed_epoch, before + 1);
        assert_eq!(store.load().epoch, before + 1);
        assert!(!store.load().is_source_live(first.source));
        assert_eq!(recorder.removes, vec![first.source]);
    }

    #[test]
    fn derived_same_logical_topic_is_isolated_between_owners() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let a = process_derived_commit(
            &mut ing,
            derived_commit("external:alpha/error", "alpha", "error", 1, None, &[1.0]),
        )
        .unwrap();
        let b = process_derived_commit(
            &mut ing,
            derived_commit("external:bravo/error", "bravo", "error", 1, None, &[2.0]),
        )
        .unwrap();

        let current = store.load();
        assert_ne!(a.source, b.source);
        assert_eq!(first_derived_value(&current, a.source), 1.0);
        assert_eq!(first_derived_value(&current, b.source), 2.0);
    }

    #[test]
    fn publication_barrier_flushes_before_acknowledging() {
        let mut ing = Ingestor::new(NullObserver);
        let source = open(&mut ing, "sequence-input", SourceKind::File);
        ing.process(IngestMsg::Batch(batch(source, "GPS", &[1, 2])));
        assert_eq!(ing.chunks_sealed(), 0);
        let (sender, receiver) = crate::ingest::ingest_channel();
        let receipt = sender.publication_barrier().unwrap();
        assert!(matches!(
            receipt.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        ing.process(receiver.try_recv().unwrap());
        assert_eq!(receipt.try_recv().unwrap(), Ok(()));
        assert_eq!(ing.chunks_sealed(), 1);
        assert_eq!(ing.rows_ingested(), 2);
    }

    #[test]
    fn live_source_seals_at_the_row_threshold_and_publishes() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let source = open(&mut ing, "live", SourceKind::Live);

        let mut t = 0_i64;
        while ing.chunks_sealed() == 0 {
            let times: Vec<i64> = (t..t + 8).collect();
            t += 8;
            ing.process(IngestMsg::Batch(batch(source, "GPS", &times)));
        }
        assert_eq!(ing.chunks_sealed(), 1);

        let snap = store.load();
        assert!(snap.epoch >= 1);
        let topic = snap
            .topics
            .iter()
            .find(|t| t.entry.name == "GPS")
            .unwrap()
            .entry
            .id;
        let topic_store = snap.topic_store(topic).unwrap();
        assert_eq!(topic_store.rows, LIVE_CHUNK_ROWS as u64);
        assert_eq!(topic_store.chunks[0].stats[0].min, 0.0);
        assert_eq!(
            topic_store.chunks[0].stats[0].max,
            (LIVE_CHUNK_ROWS - 1) as f64
        );
    }

    #[test]
    fn live_age_flush_only_seals_topics_older_than_max_age() {
        let mut ing = Ingestor::new(NullObserver);
        let source = open(&mut ing, "live", SourceKind::Live);

        ing.process(IngestMsg::Batch(batch(source, "GPS", &[1])));
        ing.flush_aged_live();
        assert_eq!(ing.chunks_sealed(), 0, "fresh pending rows stay pending");

        for pending in ing.sources.get_mut(&source).unwrap().pending.values_mut() {
            pending.first_buffered_at = Instant::now() - LIVE_MAX_AGE - Duration::from_millis(1);
        }
        ing.flush_aged_live();
        assert_eq!(ing.chunks_sealed(), 1, "expired pending rows seal");
    }

    #[test]
    fn live_derived_source_seals_on_age_like_live() {
        let mut ing = Ingestor::new(NullObserver);
        let source = open_with(&mut ing, "script:live_math", SourceKind::LiveDerived);

        ing.process(IngestMsg::Batch(batch(source, "NAV_RAD", &[1])));
        ing.flush_aged_live();
        assert_eq!(ing.chunks_sealed(), 0, "not stale yet");

        std::thread::sleep(LIVE_MAX_AGE + Duration::from_millis(10));
        ing.flush_aged_live();

        assert_eq!(
            ing.chunks_sealed(),
            1,
            "live-derived pending rows seal by age"
        );
        let snap = ing.store().load();
        let topic = snap
            .topics
            .iter()
            .find(|t| t.entry.name == "NAV_RAD")
            .expect("derived live topic registered");
        assert_eq!(snap.topic_store(topic.entry.id).unwrap().chunks.len(), 1);
    }

    #[test]
    fn on_batch_observer_fires_only_for_opened_non_empty_batches() {
        let mut recorder = Recorder::default();
        {
            let mut ing = Ingestor::new(&mut recorder);
            let store = ing.store();
            let source = open_with(&mut ing, "script:live_math", SourceKind::LiveDerived);
            assert!(
                store.load().source(source).is_none(),
                "open is not published yet"
            );

            ing.process(IngestMsg::Batch(batch(source, "NAV_RAD", &[1, 2])));
            assert!(
                store.load().source(source).is_none(),
                "observer runs while the first batch remains unpublished"
            );
            ing.process(IngestMsg::Batch(batch(
                SourceId(99),
                "UNOPENED_NAV_RAD",
                &[3],
            )));
            ing.process(IngestMsg::Batch(batch(source, "EMPTY_NAV_RAD", &[])));
        }

        assert_eq!(
            recorder.batches,
            vec![(
                SourceKind::LiveDerived,
                "script:live_math".to_owned(),
                "NAV_RAD".to_owned(),
                2
            )]
        );
    }

    #[test]
    fn close_flushes_the_partial_tail_chunk() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let source = open(&mut ing, "live", SourceKind::Live);

        ing.process(IngestMsg::Batch(batch(source, "GPS", &[1, 2, 3])));
        assert_eq!(ing.chunks_sealed(), 0);

        ing.process(IngestMsg::CloseSource {
            source,
            summary: ParseSummary::default(),
        });
        assert_eq!(ing.chunks_sealed(), 1);
        let snap = store.load();
        let topic = snap
            .topics
            .iter()
            .find(|t| t.entry.name == "GPS")
            .unwrap()
            .entry
            .id;
        assert_eq!(snap.topic_store(topic).unwrap().rows, 3);
    }

    #[test]
    fn out_of_order_batch_seals_before_overlapping() {
        let mut ing = Ingestor::new(NullObserver);
        let source = open(&mut ing, "live", SourceKind::Live);

        ing.process(IngestMsg::Batch(batch(source, "GPS", &[100, 200])));
        ing.process(IngestMsg::Batch(batch(source, "GPS", &[150, 160])));
        assert_eq!(ing.chunks_sealed(), 1, "overlap forced an early seal");
    }

    #[test]
    fn multiple_topics_become_distinct_stores() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let source = open(&mut ing, "live", SourceKind::Live);

        ing.process(IngestMsg::Batch(batch(source, "GPS", &[1])));
        ing.process(IngestMsg::Batch(batch(source, "BARO", &[1])));
        ing.process(IngestMsg::CloseSource {
            source,
            summary: ParseSummary::default(),
        });

        let snap = store.load();
        let names: Vec<&str> = snap.topics.iter().map(|t| t.entry.name.as_str()).collect();
        assert!(names.contains(&"GPS") && names.contains(&"BARO"));
    }

    #[test]
    fn close_source_notifies_the_observer() {
        let mut recorder = Recorder::default();
        {
            let mut ing = Ingestor::new(&mut recorder);
            let source = open_with(&mut ing, "live", SourceKind::Live);
            ing.process(IngestMsg::Batch(batch(source, "GPS", &[1, 2])));
            ing.process(IngestMsg::CloseSource {
                source,
                summary: ParseSummary {
                    row_count: 2,
                    ..ParseSummary::default()
                },
            });
        }
        assert_eq!(recorder.closes.len(), 1);
        assert_eq!(recorder.closes[0].1.row_count, 2);
        assert!(recorder.diags.is_empty());
    }

    #[test]
    fn close_source_publishes_source_metadata() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let source = open(&mut ing, "flight.ulg", SourceKind::File);
        let meta = SourceMetadata {
            params: vec![SourceParam {
                name: "MPC_XY_CRUISE".to_owned(),
                ty: "float".to_owned(),
                value: "5.5".to_owned(),
                default: None,
            }],
            auto_markers: vec![AutoMarker {
                time_us: 42,
                level: Some(6),
                text: "takeoff".to_owned(),
            }],
        };

        ing.process(IngestMsg::CloseSource {
            source,
            summary: ParseSummary {
                source_meta: meta.clone(),
                ..ParseSummary::default()
            },
        });

        let snap = store.load();
        assert_eq!(snap.source(source).unwrap().entry.meta, meta);
    }

    #[test]
    fn unsorted_batch_is_reordered_and_diagnosed() {
        let mut recorder = Recorder::default();
        let topic_rows;
        {
            let mut ing = Ingestor::new(&mut recorder);
            let store = ing.store();
            let source = open_with(&mut ing, "live", SourceKind::Live);

            ing.process(IngestMsg::Batch(batch(source, "GPS", &[30, 10, 20])));
            ing.process(IngestMsg::CloseSource {
                source,
                summary: ParseSummary::default(),
            });

            let snap = store.load();
            let topic = snap
                .topics
                .iter()
                .find(|t| t.entry.name == "GPS")
                .unwrap()
                .entry
                .id;
            let chunk = &snap.topic_store(topic).unwrap().chunks[0];
            assert_eq!(chunk.t.values(), &[10, 20, 30]);
            topic_rows = chunk.len();
        }
        assert_eq!(topic_rows, 3);
        assert!(recorder.diags.iter().any(|d| d.code == "unsorted-batch"));
    }

    #[test]
    fn cross_chunk_regression_is_diagnosed_but_tolerated() {
        let mut recorder = Recorder::default();
        {
            let mut ing = Ingestor::new(&mut recorder);
            let source = open_with(&mut ing, "live", SourceKind::Live);
            ing.process(IngestMsg::Batch(batch(source, "GPS", &[100, 200])));
            ing.process(IngestMsg::Batch(batch(source, "GPS", &[150, 160])));
            ing.process(IngestMsg::CloseSource {
                source,
                summary: ParseSummary::default(),
            });
        }
        let regression = recorder
            .diags
            .iter()
            .find(|d| d.code == "timestamp-regression")
            .expect("regression diagnostic emitted");
        assert_eq!(regression.time_us, Some(150));
    }

    #[test]
    fn set_source_offset_updates_the_snapshot_and_bumps_the_epoch() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let source = open(&mut ing, "flight.bin", SourceKind::File);
        ing.process(IngestMsg::Batch(batch(source, "GPS", &[100, 200])));
        ing.process(IngestMsg::CloseSource {
            source,
            summary: ParseSummary::default(),
        });
        let before = store.load();
        assert_eq!(before.global_time_range(), TimeRange::new(100, 200));

        ing.process(IngestMsg::SetSourceOffset {
            source,
            offset_us: 1_000,
        });

        let after = store.load();
        assert!(after.epoch > before.epoch, "offset change publishes");
        assert_eq!(after.source(source).unwrap().entry.offset_us, 1_000);
        assert_eq!(after.global_time_range(), TimeRange::new(1_100, 1_200));
    }

    #[test]
    fn set_source_offset_on_an_unknown_source_is_ignored() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let before = store.load();
        ing.process(IngestMsg::SetSourceOffset {
            source: SourceId(7),
            offset_us: 1_000,
        });
        assert_eq!(store.load().epoch, before.epoch, "no spurious publish");
    }

    fn two_closed_sources() -> (Ingestor<NullObserver>, Arc<DataStore>, SourceId, SourceId) {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let a = open(&mut ing, "a", SourceKind::File);
        let b = open(&mut ing, "b", SourceKind::File);
        for (source, times) in [(a, [100, 200]), (b, [300, 400])] {
            ing.process(IngestMsg::Batch(batch(source, "GPS", &times)));
            ing.process(IngestMsg::CloseSource {
                source,
                summary: ParseSummary::default(),
            });
        }
        (ing, store, a, b)
    }

    #[test]
    fn source_offset_batch_publishes_once_with_all_values() {
        let (mut ing, store, a, b) = two_closed_sources();
        let before = store.load().epoch;
        ing.process(IngestMsg::SetSourceOffsets {
            offsets: vec![(a, 125_000), (b, -250_000)],
        });
        let after = store.load();
        assert_eq!(after.epoch, before + 1);
        assert_eq!(after.source(a).unwrap().entry.offset_us, 125_000);
        assert_eq!(after.source(b).unwrap().entry.offset_us, -250_000);
    }

    #[test]
    fn invalid_source_offset_batch_is_all_or_nothing() {
        let (mut ing, store, a, _b) = two_closed_sources();
        let before = store.load();
        ing.process(IngestMsg::SetSourceOffsets {
            offsets: vec![(a, 125_000), (SourceId(u32::MAX), 7)],
        });
        let after = store.load();
        assert_eq!(after.epoch, before.epoch);
        assert_eq!(after.source(a).unwrap().entry.offset_us, 0);
    }

    #[test]
    fn duplicate_source_offset_batch_is_rejected() {
        let (mut ing, store, a, _b) = two_closed_sources();
        let before = store.load();
        ing.process(IngestMsg::SetSourceOffsets {
            offsets: vec![(a, 1), (a, 2)],
        });
        assert_eq!(store.load().epoch, before.epoch);
        assert_eq!(store.load().source(a).unwrap().entry.offset_us, 0);
    }

    #[test]
    fn overflowing_source_offset_batch_is_all_or_nothing() {
        let (mut ing, store, a, b) = two_closed_sources();
        let before = store.load();
        ing.process(IngestMsg::SetSourceOffsets {
            offsets: vec![(a, 125_000), (b, i64::MAX)],
        });
        assert_eq!(store.load().epoch, before.epoch);
        assert_eq!(store.load().source(a).unwrap().entry.offset_us, 0);
    }

    #[test]
    fn pending_timestamp_overflow_rejects_source_offset_batch() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let a = open(&mut ing, "a", SourceKind::Live);
        let b = open(&mut ing, "b", SourceKind::Live);
        ing.process(IngestMsg::Batch(batch(a, "GPS", &[100, 200])));
        ing.process(IngestMsg::Batch(batch(b, "GPS", &[1, 2])));
        assert_eq!(ing.chunks_sealed(), 0);
        let before_epoch = store.load().epoch;

        ing.process(IngestMsg::SetSourceOffsets {
            offsets: vec![(a, 125_000), (b, i64::MAX)],
        });

        assert_eq!(store.load().epoch, before_epoch);
        assert_eq!(ing.identity.source(a).unwrap().offset_us, 0);
        assert_eq!(ing.identity.source(b).unwrap().offset_us, 0);
        assert_eq!(ing.chunks_sealed(), 0);
    }

    #[test]
    fn derived_source_seals_at_the_file_threshold() {
        let mut ing = Ingestor::new(NullObserver);
        let source = open(&mut ing, "script:test", SourceKind::Derived);

        ing.process(IngestMsg::Batch(batch(source, "DERIVED", &[1, 2, 3])));
        assert_eq!(ing.chunks_sealed(), 0);

        ing.process(IngestMsg::CloseSource {
            source,
            summary: ParseSummary::default(),
        });
        assert_eq!(ing.chunks_sealed(), 1);
    }

    #[test]
    fn remove_source_drops_its_stores_and_republishes() {
        let mut recorder = Recorder::default();
        let mut ing = Ingestor::new(&mut recorder);
        let store = ing.store();
        let source = open_with(&mut ing, "script:test", SourceKind::Derived);
        ing.process(IngestMsg::Batch(batch(source, "DERIVED", &[1, 2, 3])));
        ing.process(IngestMsg::CloseSource {
            source,
            summary: ParseSummary::default(),
        });
        let before = store.load();
        let topic = before
            .topics
            .iter()
            .find(|t| t.entry.name == "DERIVED")
            .unwrap()
            .entry
            .id;
        assert!(before.topic_store(topic).is_some());

        ing.process(IngestMsg::RemoveSource { source });

        let after = store.load();
        assert!(after.epoch > before.epoch, "removal publishes a new epoch");
        assert!(!after.is_source_live(source));
        assert!(after.topic_store(topic).is_none());
        assert_eq!(recorder.removes, vec![source]);
    }

    #[test]
    fn run_on_a_thread_drains_a_real_channel() {
        let ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let (tx, rx) = ingest_channel();
        let handle = thread::spawn(move || ing.run(rx));

        let mut sink = tx.file_sink();
        let source = sink.open_source("flight.bin", SourceKind::File);
        sink.submit(batch(source, "ATT", &[10, 20, 30]));
        sink.close_source(
            source,
            ParseSummary {
                row_count: 3,
                ..ParseSummary::default()
            },
        );
        drop(sink);
        drop(tx);
        handle.join().unwrap();

        assert_eq!(source, SourceId(0));
        let snap = store.load();
        let topic = snap
            .topics
            .iter()
            .find(|t| t.entry.name == "ATT")
            .unwrap()
            .entry
            .id;
        assert_eq!(snap.topic_store(topic).unwrap().rows, 3);
    }
}
