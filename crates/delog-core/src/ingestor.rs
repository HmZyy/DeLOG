//! The single ingest thread: the only writer of the store (PLAN.md §5, ING-02).
//!
//! [`Ingestor`] drains [`IngestMsg`]s, registers sources/topics/fields, seals
//! accumulated rows into immutable [`Chunk`]s, and publishes a fresh
//! [`StoreSnapshot`] on every seal. Being the sole writer is what makes the
//! epoch-snapshot model (§4.4) correct with no locks.
//!
//! Sealing policy (§4.3): a file source seals at 64Ki rows; a live source seals
//! at 512 rows *or* after [`LIVE_MAX_AGE`] of inactivity (driven by the loop's
//! `recv_timeout`). Per-chunk [`ColStats`](crate::chunk::ColStats) are computed
//! once, at seal, inside [`Chunk::try_new`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arrow::array::{Array, ArrayRef, Int64Array};
use arrow::compute::{concat, sort_to_indices, take};

use crate::chunk::Chunk;
use crate::diagnostics::Diag;
use crate::identity::{IdentityRegistry, SourceId, TopicId};
use crate::ingest::{
    IngestMsg, IngestReceiver, ParseSummary, ParsedBatch, RecvOutcome, SourceKind,
};
use crate::schema::TopicSchema;
use crate::snapshot::{DataStore, StoreSnapshot};
use crate::store::TopicStore;

/// File source chunk target: 64Ki rows (§4.3).
pub const FILE_CHUNK_ROWS: usize = 64 * 1024;
/// Live source chunk target: 512 rows (§4.3).
pub const LIVE_CHUNK_ROWS: usize = 512;
/// Live source max chunk age before a partial seal (§4.3).
pub const LIVE_MAX_AGE: Duration = Duration::from_millis(100);

/// Callbacks for the side-channels the ingest loop fans out (diagnostics,
/// progress, close summaries). All default to no-ops so tests and headless
/// callers can ignore them; the app wires them to the diagnostics hub and
/// progress UI. Epoch/repaint notification rides the store's own subscriber
/// channel (CORE-06), so it is not duplicated here.
pub trait IngestObserver: Send {
    fn on_diagnostic(&mut self, _diag: Diag) {}
    fn on_progress(&mut self, _source: SourceId, _frac: f32) {}
    fn on_close(&mut self, _source: SourceId, _summary: ParseSummary) {}
}

/// An observer that drops everything.
#[derive(Debug, Default)]
pub struct NullObserver;
impl IngestObserver for NullObserver {}

/// Rows accumulated for one topic since its last seal.
struct Pending {
    schema: Arc<TopicSchema>,
    timestamps: Vec<Int64Array>,
    columns: Vec<Vec<ArrayRef>>,
    rows: usize,
    /// Last timestamp accepted, to guard the cross-batch join (kept sorted).
    last_ts: Option<i64>,
}

struct SourceState {
    kind: SourceKind,
    seal_rows: usize,
    /// topic name → its TopicId and pending accumulator.
    topics: HashMap<String, TopicId>,
    pending: HashMap<TopicId, Pending>,
}

/// The store writer. Construct it, hand readers [`Ingestor::store`], then drive
/// it with [`Ingestor::run`] on a dedicated thread.
pub struct Ingestor<O: IngestObserver> {
    identity: IdentityRegistry,
    store: Arc<DataStore>,
    /// Latest sealed store per topic; the snapshot is rebuilt from these.
    stores: HashMap<TopicId, Arc<TopicStore>>,
    sources: HashMap<SourceId, SourceState>,
    /// Highest timestamp seen per topic, for the cross-chunk regression check.
    topic_max_ts: HashMap<TopicId, i64>,
    observer: O,
    chunks_sealed: u64,
    rows_ingested: u64,
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
        }
    }

    /// The published store readers load from.
    pub fn store(&self) -> Arc<DataStore> {
        Arc::clone(&self.store)
    }

    pub fn chunks_sealed(&self) -> u64 {
        self.chunks_sealed
    }

    pub fn rows_ingested(&self) -> u64 {
        self.rows_ingested
    }

    /// Drain `rx` until every sender drops. Live chunks that go quiet for
    /// [`LIVE_MAX_AGE`] are flushed on the idle tick.
    pub fn run(mut self, rx: IngestReceiver) {
        loop {
            match rx.recv_timeout(LIVE_MAX_AGE) {
                RecvOutcome::Message(msg) => self.process(msg),
                RecvOutcome::Idle => self.flush_aged_live(),
                RecvOutcome::Disconnected => break,
            }
        }
        self.flush_all();
    }

    /// Apply one message. Public for step-driven testing.
    pub fn process(&mut self, msg: IngestMsg) {
        match msg {
            IngestMsg::OpenSource { key, kind, reply } => {
                let id = self.open_source(&key, kind);
                // The parser is gone if this fails; nothing more to do.
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
        }
    }

    fn open_source(&mut self, key: &str, kind: SourceKind) -> SourceId {
        let id = self.identity.add_source(key);
        let seal_rows = match kind {
            SourceKind::File => FILE_CHUNK_ROWS,
            SourceKind::Live => LIVE_CHUNK_ROWS,
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

    fn accept_batch(&mut self, batch: ParsedBatch) {
        if batch.rows() == 0 {
            return;
        }
        let Some(source) = self.sources.get(&batch.source) else {
            self.observer.on_diagnostic(Diag::warning(
                "batch-unknown-source",
                format!("batch for unopened source {:?} dropped", batch.source),
            ));
            return;
        };
        let seal_rows = source.seal_rows;
        let source_id = batch.source;

        let topic_id = match self.ensure_topic(source_id, &batch.schema) {
            Some(id) => id,
            None => return,
        };

        let schema = batch.schema;

        // Defensive within-batch sort (ING-05): parsers should hand us sorted
        // timestamps, but a malformed log may not. Sorting is the one corrective
        // copy we accept on this path; sorted batches pass through untouched.
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

        // Cross-chunk regression (ING-05): a batch starting before the highest
        // timestamp seen for this topic means the source emitted out of order.
        // We tolerate it (chunks may overlap, §4.3) but report it.
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

        // Seal the current accumulator first if this batch would start before it
        // ends — keeps every sealed chunk internally sorted (§4.3).
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

    /// Register the topic and its fields on first sighting; create its store.
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
                last_ts: None,
            })
    }

    /// Seal one topic's pending rows into a chunk, append, and publish.
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

    /// Flush every pending topic of one source (used on `CloseSource`).
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

    /// Flush partial chunks for live sources only (the idle-tick path).
    fn flush_aged_live(&mut self) {
        let stale: Vec<(SourceId, TopicId)> = self
            .sources
            .iter()
            .filter(|(_, state)| state.kind == SourceKind::Live)
            .flat_map(|(&source, state)| state.pending.keys().map(move |&topic| (source, topic)))
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

    /// Rebuild and publish the snapshot from the registry and current stores.
    fn publish(&self) {
        let topic_stores = self
            .stores
            .iter()
            .map(|(&id, store)| (id, Arc::clone(store)));
        match StoreSnapshot::from_registry(&self.identity, topic_stores, 0) {
            Ok(snapshot) => {
                let _ = self.store.publish(snapshot);
            }
            Err(err) => {
                // A snapshot build failure is a writer-side bug, not bad input.
                debug_assert!(false, "snapshot rebuild failed: {err}");
            }
        }
    }
}

/// Whether timestamps are non-decreasing (the common, copy-free case).
fn is_sorted(timestamps: &Int64Array) -> bool {
    timestamps
        .values()
        .windows(2)
        .all(|pair| pair[0] <= pair[1])
}

/// Stable-sort a batch by timestamp, reordering every column the same way.
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

/// Concatenate accumulated batch arrays into one sorted timestamp array and one
/// column set. A single pending batch is moved through without copying.
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
    use crate::identity::{AutoMarker, SourceMetadata, SourceParam};
    use crate::ingest::{IngestSink, ingest_channel};
    use crate::schema::FieldSchema;

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
    }
    impl IngestObserver for &mut Recorder {
        fn on_diagnostic(&mut self, diag: Diag) {
            self.diags.push(diag);
        }
        fn on_close(&mut self, source: SourceId, summary: ParseSummary) {
            self.closes.push((source, summary));
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

    #[test]
    fn live_source_seals_at_the_row_threshold_and_publishes() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let source = open(&mut ing, "live", SourceKind::Live);

        // Feed LIVE_CHUNK_ROWS rows in small batches; the last push crosses the
        // threshold and seals exactly one chunk.
        let mut t = 0_i64;
        while ing.chunks_sealed() == 0 {
            let times: Vec<i64> = (t..t + 64).collect();
            t += 64;
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
        // Stats were computed at seal.
        assert_eq!(topic_store.chunks[0].stats[0].min, 0.0);
        assert_eq!(
            topic_store.chunks[0].stats[0].max,
            (LIVE_CHUNK_ROWS - 1) as f64
        );
    }

    #[test]
    fn close_flushes_the_partial_tail_chunk() {
        let mut ing = Ingestor::new(NullObserver);
        let store = ing.store();
        let source = open(&mut ing, "live", SourceKind::Live);

        ing.process(IngestMsg::Batch(batch(source, "GPS", &[1, 2, 3])));
        assert_eq!(ing.chunks_sealed(), 0); // below threshold, still pending

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
        // Next batch starts before the previous ended → seal first, no regression.
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
            }],
            auto_markers: vec![AutoMarker {
                time_us: 42,
                level: 6,
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

            // Timestamps out of order within one batch.
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
            // Sealed chunk is sorted (Chunk::try_new would have rejected otherwise).
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
            // Later batch starts before the previous max → regression.
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
