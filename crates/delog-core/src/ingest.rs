//! Ingestion vocabulary. A single ingest thread is the only store writer, which
//! makes the epoch-snapshot concurrency model correct by construction.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use arrow::array::{ArrayRef, Int64Array};

use crate::diagnostics::Diag;
use crate::identity::{SourceId, SourceMetadata};
use crate::metrics::MetricsRegistry;
use crate::schema::TopicSchema;
use crate::time::TimeRange;

pub const INGEST_CHANNEL_CAP: usize = 256;

pub const METRIC_DROPPED_BATCHES: &str = "ingest_dropped_batches";

/// Emit a drop diagnostic on the 1st drop and every Nth thereafter, so a
/// saturated link reports without flooding the channel it is already starving.
const DROP_DIAG_INTERVAL: u64 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// May block on backpressure.
    File,
    /// Must never block — full channel drops the batch.
    Live,
    Derived,
    LiveDerived,
}

/// A parsed slice of one topic: sorted i64 µs timestamps plus original-dtype
/// Arrow columns.
#[derive(Debug, Clone)]
pub struct ParsedBatch {
    pub source: SourceId,
    pub schema: Arc<TopicSchema>,
    pub timestamps: Int64Array,
    pub columns: Vec<ArrayRef>,
}

impl ParsedBatch {
    pub fn new(
        source: SourceId,
        schema: Arc<TopicSchema>,
        timestamps: Int64Array,
        columns: Vec<ArrayRef>,
    ) -> Self {
        Self {
            source,
            schema,
            timestamps,
            columns,
        }
    }

    pub fn topic(&self) -> &str {
        self.schema.name()
    }

    pub fn rows(&self) -> usize {
        self.timestamps.len()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseSummary {
    pub topic_count: u64,
    pub row_count: u64,
    pub time_range: Option<TimeRange>,
    pub diagnostics: u64,
    pub source_meta: SourceMetadata,
}

#[derive(Debug)]
pub enum IngestMsg {
    /// The single-writer ingest thread assigns the dense `SourceId`, returned on `reply`.
    OpenSource {
        key: String,
        kind: SourceKind,
        reply: SyncSender<SourceId>,
    },
    Batch(ParsedBatch),
    Diagnostic(Diag),
    Progress {
        source: SourceId,
        frac: f32,
    },
    CloseSource {
        source: SourceId,
        summary: ParseSummary,
    },
    /// Routed through the ingest thread because it is the only registry writer.
    SetSourceOffset {
        source: SourceId,
        offset_us: i64,
    },
    RemoveSource {
        source: SourceId,
    },
}

/// Infallible: once the ingest thread is gone the sink goes inert (submits
/// become no-ops, `open_source` returns `SourceId(0)`) so a running parser
/// cannot panic or corrupt the store.
pub trait IngestSink: Send {
    fn open_source(&mut self, key: &str, kind: SourceKind) -> SourceId;
    fn submit(&mut self, batch: ParsedBatch);
    fn diagnostic(&mut self, diag: Diag);
    fn progress(&mut self, source: SourceId, frac: f32);
    fn close_source(&mut self, source: SourceId, summary: ParseSummary);
}

#[derive(Debug, Clone)]
pub struct IngestSender {
    tx: SyncSender<IngestMsg>,
}

#[derive(Debug)]
pub struct IngestReceiver {
    rx: Receiver<IngestMsg>,
}

pub fn ingest_channel() -> (IngestSender, IngestReceiver) {
    let (tx, rx) = sync_channel(INGEST_CHANNEL_CAP);
    (IngestSender { tx }, IngestReceiver { rx })
}

impl IngestSender {
    /// Blocking: a full channel parks the caller until the ingest thread drains.
    pub fn file_sink(&self) -> ChannelSink {
        ChannelSink {
            tx: self.tx.clone(),
            connected: true,
        }
    }

    pub fn set_source_offset(&self, source: SourceId, offset_us: i64) {
        let _ = self
            .tx
            .send(IngestMsg::SetSourceOffset { source, offset_us });
    }

    pub fn remove_source(&self, source: SourceId) {
        let _ = self.tx.send(IngestMsg::RemoveSource { source });
    }

    /// Non-blocking: a full channel drops the batch and bumps
    /// `METRIC_DROPPED_BATCHES` rather than stalling the link and overflowing OS
    /// socket buffers.
    pub fn live_sink(&self, metrics: Arc<MetricsRegistry>) -> LiveSink {
        LiveSink {
            tx: self.tx.clone(),
            connected: true,
            metrics,
            drops: 0,
        }
    }
}

impl IngestReceiver {
    pub fn recv(&self) -> Option<IngestMsg> {
        self.rx.recv().ok()
    }

    pub fn try_recv(&self) -> Option<IngestMsg> {
        self.rx.try_recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> RecvOutcome {
        match self.rx.recv_timeout(timeout) {
            Ok(msg) => RecvOutcome::Message(msg),
            Err(RecvTimeoutError::Timeout) => RecvOutcome::Idle,
            Err(RecvTimeoutError::Disconnected) => RecvOutcome::Disconnected,
        }
    }
}

#[derive(Debug)]
pub enum RecvOutcome {
    Message(IngestMsg),
    Idle,
    Disconnected,
}

#[derive(Debug)]
pub struct ChannelSink {
    tx: SyncSender<IngestMsg>,
    connected: bool,
}

impl ChannelSink {
    fn send(&mut self, msg: IngestMsg) {
        if !self.connected {
            return;
        }
        if self.tx.send(msg).is_err() {
            self.connected = false;
        }
    }
}

impl IngestSink for ChannelSink {
    fn open_source(&mut self, key: &str, kind: SourceKind) -> SourceId {
        if !self.connected {
            return SourceId(0);
        }
        let (reply_tx, reply_rx) = sync_channel(1);
        self.send(IngestMsg::OpenSource {
            key: key.to_owned(),
            kind,
            reply: reply_tx,
        });
        match reply_rx.recv() {
            Ok(id) => id,
            Err(_) => {
                self.connected = false;
                SourceId(0)
            }
        }
    }

    fn submit(&mut self, batch: ParsedBatch) {
        self.send(IngestMsg::Batch(batch));
    }

    fn diagnostic(&mut self, diag: Diag) {
        self.send(IngestMsg::Diagnostic(diag));
    }

    fn progress(&mut self, source: SourceId, frac: f32) {
        self.send(IngestMsg::Progress { source, frac });
    }

    fn close_source(&mut self, source: SourceId, summary: ParseSummary) {
        self.send(IngestMsg::CloseSource { source, summary });
    }
}

/// Live-decoder sink. `open_source` blocks for its reply (it runs once at
/// connect, before data flows); every data-bearing call uses `try_send` and
/// drops on a full channel rather than parking the link reader.
pub struct LiveSink {
    tx: SyncSender<IngestMsg>,
    connected: bool,
    metrics: Arc<MetricsRegistry>,
    drops: u64,
}

impl std::fmt::Debug for LiveSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSink")
            .field("connected", &self.connected)
            .field("drops", &self.drops)
            .finish_non_exhaustive()
    }
}

impl LiveSink {
    /// Returns the message back on a full channel so it can be counted as dropped.
    fn try_send(&mut self, msg: IngestMsg) -> Option<IngestMsg> {
        if !self.connected {
            return None;
        }
        match self.tx.try_send(msg) {
            Ok(()) => None,
            Err(TrySendError::Full(msg)) => Some(msg),
            Err(TrySendError::Disconnected(_)) => {
                self.connected = false;
                None
            }
        }
    }

    fn record_drop(&mut self) {
        self.drops += 1;
        self.metrics.add(METRIC_DROPPED_BATCHES, 1);
        if self.drops == 1 || self.drops.is_multiple_of(DROP_DIAG_INTERVAL) {
            // If the channel is still full the diagnostic is itself dropped, but
            // the counter above is the authoritative record.
            let _ = self.tx.try_send(IngestMsg::Diagnostic(Diag::warning(
                "ingest-dropped-batch",
                format!("live channel full: dropped {} batch(es)", self.drops),
            )));
        }
    }

    pub fn dropped(&self) -> u64 {
        self.drops
    }
}

impl IngestSink for LiveSink {
    fn open_source(&mut self, key: &str, kind: SourceKind) -> SourceId {
        if !self.connected {
            return SourceId(0);
        }
        let (reply_tx, reply_rx) = sync_channel(1);
        // Opening blocks (it precedes data); send rather than try_send.
        if self
            .tx
            .send(IngestMsg::OpenSource {
                key: key.to_owned(),
                kind,
                reply: reply_tx,
            })
            .is_err()
        {
            self.connected = false;
            return SourceId(0);
        }
        reply_rx.recv().unwrap_or_else(|_| {
            self.connected = false;
            SourceId(0)
        })
    }

    fn submit(&mut self, batch: ParsedBatch) {
        if self.try_send(IngestMsg::Batch(batch)).is_some() {
            self.record_drop();
        }
    }

    fn diagnostic(&mut self, diag: Diag) {
        let _ = self.try_send(IngestMsg::Diagnostic(diag));
    }

    fn progress(&mut self, source: SourceId, frac: f32) {
        let _ = self.try_send(IngestMsg::Progress { source, frac });
    }

    fn close_source(&mut self, source: SourceId, summary: ParseSummary) {
        let _ = self.try_send(IngestMsg::CloseSource { source, summary });
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use arrow::datatypes::DataType;

    use super::*;
    use crate::diagnostics::Severity;
    use crate::schema::FieldSchema;

    fn schema() -> Arc<TopicSchema> {
        Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        )
    }

    fn batch(source: SourceId) -> ParsedBatch {
        let timestamps = Int64Array::from(vec![1, 2, 3]);
        let columns: Vec<ArrayRef> = vec![Arc::new(arrow::array::Float64Array::from(vec![
            1.0, 2.0, 3.0,
        ]))];
        ParsedBatch::new(source, schema(), timestamps, columns)
    }

    #[test]
    fn channel_round_trips_messages_in_order() {
        let (tx, rx) = ingest_channel();
        let mut sink = tx.file_sink();

        sink.diagnostic(Diag::info("opened", "hello"));
        sink.submit(batch(SourceId(0)));
        sink.progress(SourceId(0), 0.5);
        sink.close_source(
            SourceId(0),
            ParseSummary {
                row_count: 3,
                ..ParseSummary::default()
            },
        );
        drop(sink);
        drop(tx);

        let mut messages = Vec::new();
        while let Some(msg) = rx.recv() {
            messages.push(msg);
        }
        assert_eq!(messages.len(), 4);
        assert!(matches!(
            &messages[0],
            IngestMsg::Diagnostic(d) if d.severity == Severity::Info
        ));
        assert!(matches!(&messages[1], IngestMsg::Batch(b) if b.rows() == 3));
        assert!(matches!(
            &messages[2],
            IngestMsg::Progress { frac, .. } if *frac == 0.5
        ));
        assert!(matches!(
            &messages[3],
            IngestMsg::CloseSource { summary, .. } if summary.row_count == 3
        ));
    }

    #[test]
    fn open_source_blocks_for_the_writer_assigned_id() {
        let (tx, rx) = ingest_channel();

        let writer = thread::spawn(move || {
            while let Some(msg) = rx.recv() {
                if let IngestMsg::OpenSource { kind, reply, .. } = msg {
                    assert_eq!(kind, SourceKind::File);
                    reply.send(SourceId(7)).unwrap();
                }
            }
        });

        let mut sink = tx.file_sink();
        let id = sink.open_source("flight.bin", SourceKind::File);
        assert_eq!(id, SourceId(7));

        drop(sink);
        drop(tx);
        writer.join().unwrap();
    }

    #[test]
    fn sink_goes_inert_after_the_receiver_drops() {
        let (tx, rx) = ingest_channel();
        let mut sink = tx.file_sink();
        drop(rx);

        sink.submit(batch(SourceId(0)));
        sink.diagnostic(Diag::error("late", "after shutdown"));
        assert_eq!(sink.open_source("late", SourceKind::Live), SourceId(0));
    }

    #[test]
    fn live_sink_drops_and_counts_when_the_channel_is_full() {
        let (tx, _rx) = ingest_channel();
        let metrics = Arc::new(MetricsRegistry::new());
        let mut sink = tx.live_sink(Arc::clone(&metrics));

        let extra = 50;
        for _ in 0..INGEST_CHANNEL_CAP + extra {
            sink.submit(batch(SourceId(0)));
        }

        assert_eq!(sink.dropped(), extra as u64);
        assert_eq!(metrics.counter(METRIC_DROPPED_BATCHES), Some(extra as u64));
    }

    #[test]
    fn live_sink_emits_a_rate_limited_drop_diagnostic() {
        let (tx, rx) = ingest_channel();
        let metrics = Arc::new(MetricsRegistry::new());
        let mut sink = tx.live_sink(Arc::clone(&metrics));

        for _ in 0..INGEST_CHANNEL_CAP - 1 {
            sink.submit(batch(SourceId(0)));
        }
        sink.submit(batch(SourceId(0)));
        sink.submit(batch(SourceId(0)));

        let mut diags = 0;
        while let Some(msg) = rx.try_recv() {
            if matches!(msg, IngestMsg::Diagnostic(_)) {
                diags += 1;
            }
        }
        assert_eq!(sink.dropped(), 1);
        assert!(diags <= 1);
    }

    #[test]
    fn many_messages_flow_through_the_bounded_channel() {
        let (tx, rx) = ingest_channel();
        let drainer = thread::spawn(move || {
            let mut count = 0;
            while let Some(msg) = rx.recv() {
                if matches!(msg, IngestMsg::Batch(_)) {
                    count += 1;
                }
            }
            count
        });

        let mut sink = tx.file_sink();
        let total = INGEST_CHANNEL_CAP * 4;
        for _ in 0..total {
            sink.submit(batch(SourceId(0)));
        }
        drop(sink);
        drop(tx);

        assert_eq!(drainer.join().unwrap(), total);
    }
}
