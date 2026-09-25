use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arrow::array::{Array, ArrayRef, Int64Array};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::record_batch::RecordBatch;
use arrow_ipc::writer::StreamWriter;
use axum::body::Body;
use bytes::Bytes;
use delog_core::chunk::Chunk;
use delog_core::identity::TopicId;
use delog_core::store::TopicStore;
use futures_util::stream;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_util::sync::CancellationToken;

use crate::handles::OpaqueId;
use crate::leases::LeaseReadGuard;
use crate::protocol::v1::error::ApiError;
use crate::protocol::v1::{API_MAJOR, API_MAX_MINOR};

pub const DELOG_TIME_COLUMN: &str = "__delog_time_ns";
pub const SOURCE_TIME_COLUMN: &str = "__source_time_ns";

const CHANNEL_CAPACITY: usize = 2;
const FULL_CHANNEL_PARK: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadQuery {
    pub fields: Option<Vec<OpaqueId>>,
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
}

#[derive(Debug)]
pub struct TopicBatchIter {
    guard: LeaseReadGuard,
    topic: TopicId,
    projection: Vec<usize>,
    chunk_index: usize,
    start_ns: Option<i64>,
    end_ns: Option<i64>,
    offset_us: i64,
    schema: SchemaRef,
    done: bool,
}

impl TopicBatchIter {
    pub fn new(guard: LeaseReadGuard, topic: TopicId, query: ReadQuery) -> Result<Self, ApiError> {
        if let (Some(start), Some(end)) = (query.start_ns, query.end_ns)
            && start > end
        {
            return Err(ApiError::invalid_input(format!(
                "start_ns {start} is after end_ns {end}"
            )));
        }

        let snapshot = Arc::clone(guard.snapshot());
        let topic_snapshot = snapshot
            .topic(topic)
            .filter(|entry| !entry.entry.removed)
            .ok_or_else(|| ApiError::not_found("topic not found in this snapshot"))?;
        let source = snapshot
            .source(topic_snapshot.entry.source)
            .filter(|entry| !entry.entry.removed)
            .ok_or_else(|| ApiError::not_found("source not found in this snapshot"))?;

        let projection = match (&query.fields, topic_snapshot.store.as_ref()) {
            (None, None) => Vec::new(),
            (None, Some(store)) => default_projection(&snapshot.fields, topic, store),
            (Some(handles), store) => explicit_projection(&guard, topic, store, handles)?,
        };

        let mut columns = vec![
            Field::new(DELOG_TIME_COLUMN, DataType::Int64, false),
            Field::new(SOURCE_TIME_COLUMN, DataType::Int64, false),
        ];
        if let Some(store) = topic_snapshot.store.as_ref() {
            for &index in &projection {
                let field = store
                    .schema
                    .field(index)
                    .ok_or_else(|| ApiError::internal("projected field is missing"))?;
                let mut metadata = HashMap::new();
                if let Some(unit) = &field.unit {
                    metadata.insert("delog.unit".to_owned(), unit.clone());
                }
                if let Some(description) = &field.description {
                    metadata.insert("delog.description".to_owned(), description.clone());
                }
                metadata.insert("delog.multiplier".to_owned(), field.multiplier.to_string());
                columns.push(
                    Field::new(field.name.clone(), field.dtype.clone(), true)
                        .with_metadata(metadata),
                );
            }
        }

        let metadata = HashMap::from([
            (
                "delog.api_version".to_owned(),
                format!("{API_MAJOR}.{API_MAX_MINOR}"),
            ),
            (
                "delog.snapshot_epoch".to_owned(),
                snapshot.epoch.to_string(),
            ),
            ("delog.source".to_owned(), source.entry.label.clone()),
            ("delog.topic".to_owned(), topic_snapshot.entry.name.clone()),
        ]);
        let schema = Arc::new(Schema::new_with_metadata(columns, metadata));
        let offset_us = source.entry.offset_us;

        Ok(Self {
            guard,
            topic,
            projection,
            chunk_index: 0,
            start_ns: query.start_ns,
            end_ns: query.end_ns,
            offset_us,
            schema,
            done: false,
        })
    }

    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    fn cancellation(&self) -> &CancellationToken {
        self.guard.cancellation()
    }

    fn batch_for(&self, chunk: &Chunk) -> Result<Option<RecordBatch>, ApiError> {
        let times = chunk.t.values();
        let offset = i128::from(self.offset_us);
        let effective_ns = |raw: i64| (i128::from(raw) + offset) * 1000;

        let begin = match self.start_ns {
            Some(start) => times.partition_point(|&raw| effective_ns(raw) < i128::from(start)),
            None => 0,
        };
        let end = match self.end_ns {
            Some(end) => times.partition_point(|&raw| effective_ns(raw) <= i128::from(end)),
            None => times.len(),
        };
        if begin >= end {
            return Ok(None);
        }
        let len = end - begin;

        let raw = &times[begin..end];
        let mut delog = Vec::with_capacity(len);
        let mut source = Vec::with_capacity(len);
        for &raw_us in raw {
            delog.push(delog_time_ns(raw_us, self.offset_us)?);
            source.push(source_time_ns(raw_us)?);
        }

        let mut columns: Vec<ArrayRef> = Vec::with_capacity(self.projection.len() + 2);
        columns.push(Arc::new(Int64Array::from(delog)));
        columns.push(Arc::new(Int64Array::from(source)));
        for &index in &self.projection {
            let column = chunk
                .cols
                .get(index)
                .ok_or_else(|| ApiError::internal("projected column is missing"))?;
            columns.push(column.slice(begin, len));
        }

        RecordBatch::try_new(Arc::clone(&self.schema), columns)
            .map(Some)
            .map_err(|error| ApiError::internal(format!("failed to build record batch: {error}")))
    }
}

impl Iterator for TopicBatchIter {
    type Item = Result<RecordBatch, ApiError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let snapshot = Arc::clone(self.guard.snapshot());
        let Some(store) = snapshot.topic_store(self.topic) else {
            self.done = true;
            return None;
        };

        while let Some(chunk) = store.chunks.get(self.chunk_index) {
            if self.cancellation().is_cancelled() {
                self.done = true;
                return Some(Err(ApiError::snapshot_expired(
                    "the snapshot lease was closed during the read",
                )));
            }
            self.chunk_index += 1;
            match self.batch_for(chunk) {
                Ok(Some(batch)) => return Some(Ok(batch)),
                Ok(None) => continue,
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }

        self.done = true;
        None
    }
}

fn default_projection(
    fields: &[delog_core::identity::FieldEntry],
    topic: TopicId,
    store: &TopicStore,
) -> Vec<usize> {
    let live: HashSet<&str> = fields
        .iter()
        .filter(|field| field.topic == topic && !field.removed)
        .map(|field| field.name.as_str())
        .collect();
    store
        .schema
        .fields()
        .iter()
        .enumerate()
        .filter(|(_, field)| live.contains(field.name.as_str()))
        .map(|(index, _)| index)
        .collect()
}

fn explicit_projection(
    guard: &LeaseReadGuard,
    topic: TopicId,
    store: Option<&Arc<TopicStore>>,
    handles: &[OpaqueId],
) -> Result<Vec<usize>, ApiError> {
    let mut seen = HashSet::with_capacity(handles.len());
    let mut projection = Vec::with_capacity(handles.len());
    for handle in handles {
        if !seen.insert(handle) {
            return Err(ApiError::invalid_input(format!(
                "field handle {handle} is requested more than once"
            )));
        }
        let field = guard
            .field(handle)
            .ok_or_else(|| ApiError::not_found(format!("field handle {handle} not found")))?;
        if field.topic != topic {
            return Err(ApiError::invalid_input(format!(
                "field handle {handle} belongs to a different topic"
            )));
        }
        let index = store
            .and_then(|store| store.schema.field_index(&field.dto.name))
            .ok_or_else(|| ApiError::internal(format!("field handle {handle} has no column")))?;
        projection.push(index);
    }
    Ok(projection)
}

fn delog_time_ns(raw_us: i64, offset_us: i64) -> Result<i64, ApiError> {
    raw_us
        .checked_add(offset_us)
        .ok_or_else(|| ApiError::internal("source offset overflows the effective timestamp"))?
        .checked_mul(1000)
        .ok_or_else(|| ApiError::internal("effective timestamp overflows wire nanoseconds"))
}

fn source_time_ns(raw_us: i64) -> Result<i64, ApiError> {
    raw_us
        .checked_mul(1000)
        .ok_or_else(|| ApiError::internal("source timestamp overflows wire nanoseconds"))
}

#[derive(Debug, Default)]
pub struct ArrowStreamStats {
    chunks_sent: AtomicU64,
    complete: AtomicBool,
    finished: AtomicBool,
    queue: Mutex<QueueStats>,
}

#[derive(Debug, Default)]
struct QueueStats {
    chunks: usize,
    bytes: usize,
    max_chunks: usize,
    max_bytes: usize,
}

impl ArrowStreamStats {
    pub fn chunks_sent(&self) -> u64 {
        self.chunks_sent.load(Ordering::SeqCst)
    }

    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::SeqCst)
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    #[doc(hidden)]
    pub fn queued_chunks(&self) -> usize {
        self.queue.lock().expect("stream queue poisoned").chunks
    }

    #[doc(hidden)]
    pub fn queued_bytes(&self) -> usize {
        self.queue.lock().expect("stream queue poisoned").bytes
    }

    #[doc(hidden)]
    pub fn max_queued_chunks(&self) -> usize {
        self.queue.lock().expect("stream queue poisoned").max_chunks
    }

    #[doc(hidden)]
    pub fn max_queued_bytes(&self) -> usize {
        self.queue.lock().expect("stream queue poisoned").max_bytes
    }

    fn dequeue(&self, bytes: usize) {
        let mut queue = self.queue.lock().expect("stream queue poisoned");
        queue.chunks = queue.chunks.saturating_sub(1);
        queue.bytes = queue.bytes.saturating_sub(bytes);
    }
}

struct ChunkWriter {
    sender: mpsc::Sender<Bytes>,
    cancellation: CancellationToken,
    stats: Arc<ArrowStreamStats>,
    pending: Vec<u8>,
}

impl ChunkWriter {
    fn send_pending(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut chunk = Bytes::from(std::mem::take(&mut self.pending));
        loop {
            if self.cancellation.is_cancelled() {
                return Err(io::Error::other("snapshot lease was cancelled"));
            }
            let bytes = chunk.len();
            let mut queue = self.stats.queue.lock().expect("stream queue poisoned");
            match self.sender.try_send(chunk) {
                Ok(()) => {
                    queue.chunks += 1;
                    queue.bytes += bytes;
                    queue.max_chunks = queue.max_chunks.max(queue.chunks);
                    queue.max_bytes = queue.max_bytes.max(queue.bytes);
                    drop(queue);
                    self.stats.chunks_sent.fetch_add(1, Ordering::SeqCst);
                    return Ok(());
                }
                Err(TrySendError::Closed(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "arrow stream receiver disconnected",
                    ));
                }
                Err(TrySendError::Full(returned)) => {
                    chunk = returned;
                    drop(queue);
                    std::thread::park_timeout(FULL_CHANNEL_PARK);
                    if self.sender.is_closed() {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "arrow stream receiver disconnected",
                        ));
                    }
                }
            }
        }
    }
}

impl Write for ChunkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_pending()
    }
}

pub fn arrow_body(iter: TopicBatchIter) -> Body {
    arrow_body_with_stats(iter).0
}

pub fn arrow_body_with_stats(iter: TopicBatchIter) -> (Body, Arc<ArrowStreamStats>) {
    let stats = Arc::new(ArrowStreamStats::default());
    let (sender, receiver) = mpsc::channel::<Bytes>(CHANNEL_CAPACITY);
    let writer = ChunkWriter {
        sender,
        cancellation: iter.cancellation().clone(),
        stats: Arc::clone(&stats),
        pending: Vec::new(),
    };

    let producer_stats = Arc::clone(&stats);
    tokio::task::spawn_blocking(move || {
        if let Err(error) = encode(iter, writer) {
            tracing::debug!(%error, "arrow stream stopped before completion");
        }
        producer_stats.finished.store(true, Ordering::SeqCst);
    });

    let body_stats = Arc::clone(&stats);
    let chunks = stream::unfold(
        (receiver, body_stats, false),
        |(mut receiver, stats, ended)| async move {
            if ended {
                return None;
            }
            match receiver.recv().await {
                Some(chunk) => {
                    stats.dequeue(chunk.len());
                    Some((Ok(chunk), (receiver, stats, false)))
                }
                None if stats.is_complete() => None,
                None => Some((
                    Err(io::Error::other("arrow stream ended before completion")),
                    (receiver, stats, true),
                )),
            }
        },
    );

    (Body::from_stream(chunks), stats)
}

fn encode(mut iter: TopicBatchIter, writer: ChunkWriter) -> Result<(), String> {
    let schema = Arc::clone(iter.schema());
    let mut stream = StreamWriter::try_new(writer, &schema).map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())?;

    for batch in iter.by_ref() {
        let batch = batch.map_err(|error| error.message().to_owned())?;
        stream.write(&batch).map_err(|error| error.to_string())?;
        stream.flush().map_err(|error| error.to_string())?;
    }
    stream.finish().map_err(|error| error.to_string())?;

    let stats = Arc::clone(&stream.get_ref().stats);
    stats.complete.store(true, Ordering::SeqCst);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_time_rejects_offset_overflow() {
        assert_eq!(
            delog_time_ns(i64::MAX - 10, 100).unwrap_err().code(),
            "internal"
        );
        assert_eq!(
            delog_time_ns(i64::MIN + 10, -100).unwrap_err().code(),
            "internal"
        );
    }

    #[test]
    fn effective_time_rejects_nanosecond_overflow() {
        assert_eq!(
            delog_time_ns(i64::MAX / 1000, 1).unwrap_err().code(),
            "internal"
        );
        assert_eq!(
            delog_time_ns(i64::MIN / 1000, -1).unwrap_err().code(),
            "internal"
        );
        assert_eq!(delog_time_ns(-5, 2).unwrap(), -3_000);
    }

    #[test]
    fn source_time_rejects_nanosecond_overflow() {
        assert_eq!(
            source_time_ns(i64::MAX / 1000 + 1).unwrap_err().code(),
            "internal"
        );
        assert_eq!(source_time_ns(-7).unwrap(), -7_000);
    }
}
