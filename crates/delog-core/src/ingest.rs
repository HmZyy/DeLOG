//! Ingestion pipeline vocabulary: the parser-facing [`IngestSink`], the
//! [`IngestMsg`] wire type, and the bounded channel that funnels every source
//! into the single ingest thread (PLAN.md §5, ING-01).
//!
//! Parsers and live decoders never touch the store directly: they hold an
//! [`IngestSink`] and emit messages. A single ingest thread (ING-02) is the
//! only store writer, which makes the epoch-snapshot concurrency model (§4.4)
//! correct by construction. The channel is bounded at [`INGEST_CHANNEL_CAP`];
//! the backpressure *policy* over that bound (file-block vs live-drop) is
//! ING-03 — this module ships the blocking, file-parser sink.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::Duration;

use arrow::array::{ArrayRef, Int64Array};

use crate::diagnostics::Diag;
use crate::identity::SourceId;
use crate::schema::TopicSchema;
use crate::time::TimeRange;

/// Bounded ingest channel capacity (§5). Small enough to bound memory and make
/// backpressure bite promptly, large enough to absorb bursty parser flushes.
pub const INGEST_CHANNEL_CAP: usize = 256;

/// What kind of source produced a stream of batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A log file parsed start-to-finish; may block on backpressure (§5).
    File,
    /// A live link; must never block — full channel drops the batch (ING-03).
    Live,
}

/// A parsed slice of one topic: sorted `i64` µs timestamps plus original-dtype
/// Arrow columns, moved (never copied) from the parser's builders — upholds
/// ZC-1. The ingest thread validates and seals these into immutable chunks.
///
/// The topic name is the [`schema`](ParsedBatch::schema) name — multi-instance
/// topics carry their `[N]` suffix there (§4.3), so there is exactly one name.
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

    /// The topic name this batch belongs to.
    pub fn topic(&self) -> &str {
        self.schema.name()
    }

    pub fn rows(&self) -> usize {
        self.timestamps.len()
    }
}

/// Tally a parser reports when it finishes a source (§6.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseSummary {
    pub topic_count: u64,
    pub row_count: u64,
    pub time_range: Option<TimeRange>,
    pub diagnostics: u64,
}

/// One message on the ingest channel (§5).
#[derive(Debug)]
pub enum IngestMsg {
    /// Register a source; the ingest thread (single writer) assigns the dense
    /// [`SourceId`] and returns it on `reply`.
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
}

/// The parser-facing handle. Every method is infallible: once the ingest thread
/// has gone away (app shutdown), the sink goes *inert* — submissions become
/// no-ops and `open_source` returns [`SourceId`]`(0)` — so a still-running
/// parser cannot panic or corrupt the store. Stopping such a parser promptly is
/// the cancellation token's job (ING-04), not the sink's.
pub trait IngestSink: Send {
    fn open_source(&mut self, key: &str, kind: SourceKind) -> SourceId;
    fn submit(&mut self, batch: ParsedBatch);
    fn diagnostic(&mut self, diag: Diag);
    fn progress(&mut self, source: SourceId, frac: f32);
    fn close_source(&mut self, source: SourceId, summary: ParseSummary);
}

/// Cloneable producer end shared by every parser/decoder thread.
#[derive(Debug, Clone)]
pub struct IngestSender {
    tx: SyncSender<IngestMsg>,
}

/// Single-consumer end drained by the ingest thread (ING-02).
#[derive(Debug)]
pub struct IngestReceiver {
    rx: Receiver<IngestMsg>,
}

/// Build the bounded ingest channel (cap [`INGEST_CHANNEL_CAP`]).
pub fn ingest_channel() -> (IngestSender, IngestReceiver) {
    let (tx, rx) = sync_channel(INGEST_CHANNEL_CAP);
    (IngestSender { tx }, IngestReceiver { rx })
}

impl IngestSender {
    /// A blocking, file-parser sink: a full channel parks the caller until the
    /// ingest thread drains, trading latency for zero loss (§5). Live decoders
    /// use the dropping sink from ING-03 instead.
    pub fn file_sink(&self) -> ChannelSink {
        ChannelSink {
            tx: self.tx.clone(),
            connected: true,
        }
    }
}

impl IngestReceiver {
    /// Receive the next message, blocking until one is available or every
    /// sender has dropped (returns `None`).
    pub fn recv(&self) -> Option<IngestMsg> {
        self.rx.recv().ok()
    }

    /// Non-blocking receive of a ready message.
    pub fn try_recv(&self) -> Option<IngestMsg> {
        self.rx.try_recv().ok()
    }

    /// Block up to `timeout` for a message. [`RecvOutcome::Idle`] means the
    /// deadline passed with nothing ready (the ingest loop uses this to flush
    /// aged live chunks, §4.3); [`RecvOutcome::Disconnected`] means every sender
    /// has dropped.
    pub fn recv_timeout(&self, timeout: Duration) -> RecvOutcome {
        match self.rx.recv_timeout(timeout) {
            Ok(msg) => RecvOutcome::Message(msg),
            Err(RecvTimeoutError::Timeout) => RecvOutcome::Idle,
            Err(RecvTimeoutError::Disconnected) => RecvOutcome::Disconnected,
        }
    }
}

/// Result of a timed receive on the ingest channel.
#[derive(Debug)]
pub enum RecvOutcome {
    Message(IngestMsg),
    /// Timeout elapsed with no message ready.
    Idle,
    /// Every sender has dropped; the loop should finish.
    Disconnected,
}

/// Blocking sink (file-parser semantics). See [`IngestSender::file_sink`].
#[derive(Debug)]
pub struct ChannelSink {
    tx: SyncSender<IngestMsg>,
    connected: bool,
}

impl ChannelSink {
    /// Send blocking, marking the sink inert if the ingest thread has gone.
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

        // Stand in for the ingest thread: assign id 7 to the open request.
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

        // No panic; submissions are silently dropped and open returns id 0.
        sink.submit(batch(SourceId(0)));
        sink.diagnostic(Diag::error("late", "after shutdown"));
        assert_eq!(sink.open_source("late", SourceKind::Live), SourceId(0));
    }

    #[test]
    fn many_messages_flow_through_the_bounded_channel() {
        // More than the channel cap, with a concurrent drainer: blocking sends
        // make progress as the receiver consumes — nothing is lost.
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
