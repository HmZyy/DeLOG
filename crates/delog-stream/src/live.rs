//! Live MAVLink frame consumer: sysid demux, field extraction, batching and
//! optional raw-frame recording (PLAN.md LIV-05..LIV-11).

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arrow::array::{
    ArrayRef, Float32Builder, Float64Builder, Int8Builder, Int16Builder, Int32Builder,
    Int64Builder, StringBuilder, UInt8Builder, UInt16Builder, UInt32Builder, UInt64Builder,
};
use arrow::datatypes::DataType;
use crossbeam_channel::{Receiver, RecvTimeoutError, unbounded};
use delog_core::diagnostics::Diag;
use delog_core::identity::{SourceId, SourceMetadata};
use delog_core::ingest::{IngestSender, IngestSink, ParseSummary, ParsedBatch, SourceKind};
use delog_core::ingestor::LIVE_CHUNK_ROWS;
use delog_core::metrics::MetricsRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::time::TimeRange;
use delog_parsers::mavlink::{DecodedFrame, Scalar, extract_fields};
use mavlink::Message;

use crate::Endpoint;
use crate::reader::{LinkReader, LinkState, LinkStats};
use crate::recorder::TlogRecorder;

const LIVE_BATCH_AGE: Duration = Duration::from_millis(100);
const RX_RATE_PERIOD: Duration = Duration::from_secs(1);

type TopicKey = (u8, &'static str);

/// A running link: one byte reader plus one frame→ingest worker. Dropping it
/// stops both threads.
pub struct LiveLink {
    endpoint: Endpoint,
    reader: Option<LinkReader>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    stats: Arc<LiveIngestStats>,
    recording: Option<PathBuf>,
}

/// Live ingest counters owned by the frame consumer.
#[derive(Debug, Default)]
pub struct LiveIngestStats {
    rows: AtomicU64,
    batches: AtomicU64,
    sources: AtomicU64,
    recorder_records: AtomicU64,
    recorder_errors: AtomicU64,
}

/// Point-in-time live ingest stats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveStats {
    pub rows: u64,
    pub batches: u64,
    pub sources: u64,
    pub recorder_records: u64,
    pub recorder_errors: u64,
}

/// Combined status for app/UI callers.
#[derive(Debug, Clone)]
pub struct LiveLinkStatus {
    pub endpoint: Endpoint,
    pub state: LinkState,
    pub link: LinkStats,
    pub ingest: LiveStats,
    pub recording: Option<PathBuf>,
}

impl LiveLink {
    pub fn spawn(
        endpoint: Endpoint,
        sender: IngestSender,
        metrics: Arc<MetricsRegistry>,
        recording: Option<PathBuf>,
    ) -> io::Result<Self> {
        let (tx, rx) = unbounded::<DecodedFrame>();
        let reader = LinkReader::spawn(&endpoint, tx)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(LiveIngestStats::default());
        let worker = {
            let endpoint = endpoint.clone();
            let stop = Arc::clone(&stop);
            let stats = Arc::clone(&stats);
            let recording = recording.clone();
            thread::Builder::new()
                .name(format!("live-ingest {endpoint}"))
                .spawn(move || {
                    let mut sink = sender.live_sink(metrics.clone());
                    let recorder = match recording {
                        Some(path) => match TlogRecorder::create(&path) {
                            Ok(recorder) => Some(recorder),
                            Err(err) => {
                                sink.diagnostic(Diag::warning(
                                    "live-recorder-open",
                                    format!("{}: {err}", path.display()),
                                ));
                                None
                            }
                        },
                        None => None,
                    };
                    let mut consumer = LiveConsumer::new(endpoint, sink, metrics, stats, recorder);
                    consumer.run(rx, &stop);
                })?
        };
        Ok(Self {
            endpoint,
            reader: Some(reader),
            stop,
            worker: Some(worker),
            stats,
            recording,
        })
    }

    pub fn status(&self) -> LiveLinkStatus {
        LiveLinkStatus {
            endpoint: self.endpoint.clone(),
            state: self
                .reader
                .as_ref()
                .map(LinkReader::state)
                .unwrap_or(LinkState::Lost),
            link: self
                .reader
                .as_ref()
                .map(LinkReader::stats)
                .unwrap_or_default(),
            ingest: self.stats.snapshot(),
            recording: self.recording.clone(),
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(reader) = &self.reader {
            reader.stop();
        }
    }

    pub fn join(mut self) -> io::Result<()> {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        match self.reader.take() {
            Some(reader) => reader.join(),
            None => Ok(()),
        }
    }
}

impl Drop for LiveLink {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl LiveIngestStats {
    fn add_rows(&self, n: u64) {
        self.rows.fetch_add(n, Ordering::Relaxed);
    }

    fn add_batch(&self) {
        self.batches.fetch_add(1, Ordering::Relaxed);
    }

    fn set_sources(&self, n: u64) {
        self.sources.store(n, Ordering::Relaxed);
    }

    fn add_record(&self) {
        self.recorder_records.fetch_add(1, Ordering::Relaxed);
    }

    fn add_recorder_error(&self) {
        self.recorder_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> LiveStats {
        LiveStats {
            rows: self.rows.load(Ordering::Relaxed),
            batches: self.batches.load(Ordering::Relaxed),
            sources: self.sources.load(Ordering::Relaxed),
            recorder_records: self.recorder_records.load(Ordering::Relaxed),
            recorder_errors: self.recorder_errors.load(Ordering::Relaxed),
        }
    }
}

struct LiveConsumer<S: IngestSink> {
    endpoint: Endpoint,
    sink: S,
    metrics: Arc<MetricsRegistry>,
    stats: Arc<LiveIngestStats>,
    recorder: Option<TlogRecorder>,
    sources: HashMap<u8, SourceState>,
    unknown_seen: HashSet<u32>,
    started: Instant,
    last_rate: Instant,
    frames_since_rate: u64,
}

struct SourceState {
    id: SourceId,
    topics: HashMap<TopicKey, Topic>,
    instances: HashMap<&'static str, u32>,
    topic_count: u64,
    rows: u64,
    diagnostics: u64,
    time_range: Option<TimeRange>,
}

struct Topic {
    schema: Arc<TopicSchema>,
    ts: Int64Builder,
    cols: Vec<Col>,
    rows: usize,
    last_flush: Instant,
}

impl<S: IngestSink> LiveConsumer<S> {
    fn new(
        endpoint: Endpoint,
        sink: S,
        metrics: Arc<MetricsRegistry>,
        stats: Arc<LiveIngestStats>,
        recorder: Option<TlogRecorder>,
    ) -> Self {
        let now = Instant::now();
        Self {
            endpoint,
            sink,
            metrics,
            stats,
            recorder,
            sources: HashMap::new(),
            unknown_seen: HashSet::new(),
            started: now,
            last_rate: now,
            frames_since_rate: 0,
        }
    }
    fn run(&mut self, rx: Receiver<DecodedFrame>, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(frame) => self.accept(frame),
                Err(RecvTimeoutError::Timeout) => self.flush_aged(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.flush_all();
        self.close_sources();
    }

    fn accept(&mut self, frame: DecodedFrame) {
        self.frames_since_rate += 1;
        self.record_rate();

        if let Some(recorder) = &mut self.recorder {
            match recorder.write_frame(unix_now_us(), &frame.raw) {
                Ok(()) => self.stats.add_record(),
                Err(err) => {
                    self.stats.add_recorder_error();
                    self.sink.diagnostic(Diag::warning(
                        "live-recorder-write",
                        format!("{}: {err}", self.endpoint),
                    ));
                    self.recorder = None;
                }
            }
        }

        let Some(message) = frame.message.as_ref() else {
            if self.unknown_seen.insert(frame.message_id) {
                self.sink.diagnostic(Diag::info(
                    "live-unknown-message",
                    format!(
                        "{} sysid {} compid {} message id {} is not in the ardupilotmega dialect",
                        self.endpoint, frame.system_id, frame.component_id, frame.message_id
                    ),
                ));
            }
            return;
        };

        let fields = extract_fields(message);
        if fields.is_empty() {
            return;
        }
        let ts_us = self.started.elapsed().as_micros().min(i64::MAX as u128) as i64;
        let msg_name = message.message_name();

        if !self.sources.contains_key(&frame.system_id) {
            let key = format!("mavlink:{}:sysid{}", self.endpoint, frame.system_id);
            let id = self.sink.open_source(&key, SourceKind::Live);
            self.sources.insert(
                frame.system_id,
                SourceState {
                    id,
                    topics: HashMap::new(),
                    instances: HashMap::new(),
                    topic_count: 0,
                    rows: 0,
                    diagnostics: 0,
                    time_range: None,
                },
            );
            self.stats.set_sources(self.sources.len() as u64);
        }

        self.accept_fields(
            frame.system_id,
            frame.component_id,
            msg_name,
            ts_us,
            &fields,
        );
    }

    fn accept_fields(
        &mut self,
        sysid: u8,
        compid: u8,
        msg_name: &'static str,
        ts_us: i64,
        fields: &[(String, Scalar)],
    ) {
        let source = self.sources.get_mut(&sysid).expect("source created above");
        let key = (compid, msg_name);
        if !source.topics.contains_key(&key) {
            let instance = {
                let counter = source.instances.entry(msg_name).or_insert(0);
                let i = *counter;
                *counter += 1;
                i
            };
            let topic_name = if instance == 0 {
                msg_name.to_owned()
            } else {
                format!("{msg_name}[{instance}]")
            };
            let field_schemas: Vec<_> = fields
                .iter()
                .filter_map(|(name, scalar)| {
                    FieldSchema::new(name.clone(), scalar_dtype(scalar), None::<String>, 1.0).ok()
                })
                .collect();
            if field_schemas.len() != fields.len() {
                source.diagnostics += 1;
                self.sink.diagnostic(
                    Diag::warning(
                        "live-bad-field",
                        format!("`{msg_name}` has an unrepresentable field; topic skipped"),
                    )
                    .with_source(source.id),
                );
                return;
            }
            let schema = match TopicSchema::new(topic_name, field_schemas) {
                Ok(schema) => Arc::new(schema),
                Err(err) => {
                    source.diagnostics += 1;
                    self.sink.diagnostic(
                        Diag::warning("live-bad-schema", format!("`{msg_name}`: {err}"))
                            .with_source(source.id),
                    );
                    return;
                }
            };
            source.topic_count += 1;
            source.topics.insert(
                key,
                Topic {
                    schema,
                    ts: Int64Builder::new(),
                    cols: fields
                        .iter()
                        .map(|(_, scalar)| Col::for_scalar(scalar))
                        .collect(),
                    rows: 0,
                    last_flush: Instant::now(),
                },
            );
        }

        let source_id = source.id;
        let topic = source.topics.get_mut(&key).expect("topic inserted above");
        if fields.len() != topic.cols.len() {
            source.diagnostics += 1;
            self.sink.diagnostic(
                Diag::warning(
                    "live-field-drift",
                    format!("`{msg_name}` field set changed; row skipped"),
                )
                .with_source(source_id)
                .at_time(ts_us),
            );
            return;
        }
        topic.ts.append_value(ts_us);
        for (col, (_, scalar)) in topic.cols.iter_mut().zip(fields) {
            if !col.append(scalar) {
                col.append_null();
            }
        }
        topic.rows += 1;
        source.rows += 1;
        source.time_range = Some(match source.time_range {
            Some(range) => range.include(ts_us),
            None => TimeRange::point(ts_us),
        });
        self.stats.add_rows(1);

        if topic.rows >= LIVE_CHUNK_ROWS {
            let batch = topic.take_batch(source_id);
            self.stats.add_batch();
            self.sink.submit(batch);
        }
    }

    fn flush_aged(&mut self) {
        let now = Instant::now();
        let mut batches = Vec::new();
        for source in self.sources.values_mut() {
            for topic in source.topics.values_mut() {
                if topic.rows > 0 && now.duration_since(topic.last_flush) >= LIVE_BATCH_AGE {
                    batches.push(topic.take_batch(source.id));
                }
            }
        }
        for batch in batches {
            self.stats.add_batch();
            self.sink.submit(batch);
        }
    }

    fn flush_all(&mut self) {
        let mut batches = Vec::new();
        for source in self.sources.values_mut() {
            for topic in source.topics.values_mut() {
                if topic.rows > 0 {
                    batches.push(topic.take_batch(source.id));
                }
            }
        }
        for batch in batches {
            self.stats.add_batch();
            self.sink.submit(batch);
        }
    }

    fn close_sources(&mut self) {
        for source in self.sources.values() {
            self.sink.close_source(
                source.id,
                ParseSummary {
                    topic_count: source.topic_count,
                    row_count: source.rows,
                    time_range: source.time_range,
                    diagnostics: source.diagnostics,
                    source_meta: SourceMetadata::default(),
                },
            );
        }
    }

    fn record_rate(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_rate);
        if dt >= RX_RATE_PERIOD {
            self.metrics.record(
                "live_rx_rate",
                self.frames_since_rate as f32 / dt.as_secs_f32().max(f32::EPSILON),
            );
            self.frames_since_rate = 0;
            self.last_rate = now;
        }
    }
}

impl Topic {
    fn take_batch(&mut self, source: SourceId) -> ParsedBatch {
        let timestamps = self.ts.finish();
        let columns = self.cols.iter_mut().map(Col::finish).collect();
        let rows = timestamps.len();
        self.rows = 0;
        self.last_flush = Instant::now();
        debug_assert!(rows > 0);
        ParsedBatch::new(source, Arc::clone(&self.schema), timestamps, columns)
    }
}

fn scalar_dtype(scalar: &Scalar) -> DataType {
    match scalar {
        Scalar::U8(_) => DataType::UInt8,
        Scalar::I8(_) => DataType::Int8,
        Scalar::U16(_) => DataType::UInt16,
        Scalar::I16(_) => DataType::Int16,
        Scalar::U32(_) => DataType::UInt32,
        Scalar::I32(_) => DataType::Int32,
        Scalar::U64(_) => DataType::UInt64,
        Scalar::I64(_) => DataType::Int64,
        Scalar::F32(_) => DataType::Float32,
        Scalar::F64(_) => DataType::Float64,
        Scalar::Str(_) => DataType::Utf8,
    }
}

enum Col {
    U8(UInt8Builder),
    I8(Int8Builder),
    U16(UInt16Builder),
    I16(Int16Builder),
    U32(UInt32Builder),
    I32(Int32Builder),
    U64(UInt64Builder),
    I64(Int64Builder),
    F32(Float32Builder),
    F64(Float64Builder),
    Str(StringBuilder),
}

impl Col {
    fn for_scalar(scalar: &Scalar) -> Self {
        match scalar {
            Scalar::U8(_) => Self::U8(UInt8Builder::new()),
            Scalar::I8(_) => Self::I8(Int8Builder::new()),
            Scalar::U16(_) => Self::U16(UInt16Builder::new()),
            Scalar::I16(_) => Self::I16(Int16Builder::new()),
            Scalar::U32(_) => Self::U32(UInt32Builder::new()),
            Scalar::I32(_) => Self::I32(Int32Builder::new()),
            Scalar::U64(_) => Self::U64(UInt64Builder::new()),
            Scalar::I64(_) => Self::I64(Int64Builder::new()),
            Scalar::F32(_) => Self::F32(Float32Builder::new()),
            Scalar::F64(_) => Self::F64(Float64Builder::new()),
            Scalar::Str(_) => Self::Str(StringBuilder::new()),
        }
    }

    fn append(&mut self, scalar: &Scalar) -> bool {
        match (self, scalar) {
            (Self::U8(b), Scalar::U8(v)) => b.append_value(*v),
            (Self::I8(b), Scalar::I8(v)) => b.append_value(*v),
            (Self::U16(b), Scalar::U16(v)) => b.append_value(*v),
            (Self::I16(b), Scalar::I16(v)) => b.append_value(*v),
            (Self::U32(b), Scalar::U32(v)) => b.append_value(*v),
            (Self::I32(b), Scalar::I32(v)) => b.append_value(*v),
            (Self::U64(b), Scalar::U64(v)) => b.append_value(*v),
            (Self::I64(b), Scalar::I64(v)) => b.append_value(*v),
            (Self::F32(b), Scalar::F32(v)) => b.append_value(*v),
            (Self::F64(b), Scalar::F64(v)) => b.append_value(*v),
            (Self::Str(b), Scalar::Str(v)) => b.append_value(v),
            _ => return false,
        }
        true
    }

    fn append_null(&mut self) {
        match self {
            Self::U8(b) => b.append_null(),
            Self::I8(b) => b.append_null(),
            Self::U16(b) => b.append_null(),
            Self::I16(b) => b.append_null(),
            Self::U32(b) => b.append_null(),
            Self::I32(b) => b.append_null(),
            Self::U64(b) => b.append_null(),
            Self::I64(b) => b.append_null(),
            Self::F32(b) => b.append_null(),
            Self::F64(b) => b.append_null(),
            Self::Str(b) => b.append_null(),
        }
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            Self::U8(b) => Arc::new(b.finish()),
            Self::I8(b) => Arc::new(b.finish()),
            Self::U16(b) => Arc::new(b.finish()),
            Self::I16(b) => Arc::new(b.finish()),
            Self::U32(b) => Arc::new(b.finish()),
            Self::I32(b) => Arc::new(b.finish()),
            Self::U64(b) => Arc::new(b.finish()),
            Self::I64(b) => Arc::new(b.finish()),
            Self::F32(b) => Arc::new(b.finish()),
            Self::F64(b) => Arc::new(b.finish()),
            Self::Str(b) => Arc::new(b.finish()),
        }
    }
}

fn unix_now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver as StdReceiver, SyncSender, sync_channel};

    use delog_core::ingest::IngestMsg;
    use delog_core::parse_ctl::{CancelToken, ParseCtl};
    use delog_parsers::{LogParser, TlogParser};
    use mavlink::MavlinkVersion;
    use mavlink::dialects::ardupilotmega::{ATTITUDE_DATA, MavMessage};
    use mavlink::{MAVLinkV2MessageRaw, MavHeader};

    use super::*;

    fn frame(sys: u8, comp: u8, msg: MavMessage) -> DecodedFrame {
        DecodedFrame {
            version: MavlinkVersion::V2,
            system_id: sys,
            component_id: comp,
            sequence: 0,
            message_id: msg.message_id(),
            message: Some(msg),
            raw: vec![0xFD, 0, 0, 0],
        }
    }

    fn attitude(roll: f32) -> MavMessage {
        MavMessage::ATTITUDE(ATTITUDE_DATA {
            time_boot_ms: 1_000,
            roll,
            pitch: 0.0,
            yaw: 0.0,
            rollspeed: 0.0,
            pitchspeed: 0.0,
            yawspeed: 0.0,
        })
    }

    fn v2(seq: u8, sys: u8, comp: u8, msg: &MavMessage) -> Vec<u8> {
        let mut raw = MAVLinkV2MessageRaw::new();
        raw.serialize_message(
            MavHeader {
                system_id: sys,
                component_id: comp,
                sequence: seq,
            },
            msg,
        );
        raw.raw_bytes().to_vec()
    }

    struct Sink {
        tx: SyncSender<IngestMsg>,
        next: u32,
    }

    impl Sink {
        fn new() -> (Self, StdReceiver<IngestMsg>) {
            let (tx, rx) = sync_channel(64);
            (Self { tx, next: 1 }, rx)
        }
    }

    impl IngestSink for Sink {
        fn open_source(&mut self, key: &str, kind: SourceKind) -> SourceId {
            let id = SourceId(self.next);
            self.next += 1;
            let (reply, _rx) = sync_channel(1);
            self.tx
                .send(IngestMsg::OpenSource {
                    key: key.to_owned(),
                    kind,
                    reply,
                })
                .unwrap();
            id
        }

        fn submit(&mut self, batch: ParsedBatch) {
            self.tx.send(IngestMsg::Batch(batch)).unwrap();
        }

        fn diagnostic(&mut self, diag: Diag) {
            self.tx.send(IngestMsg::Diagnostic(diag)).unwrap();
        }

        fn progress(&mut self, source: SourceId, frac: f32) {
            self.tx.send(IngestMsg::Progress { source, frac }).unwrap();
        }

        fn close_source(&mut self, source: SourceId, summary: ParseSummary) {
            self.tx
                .send(IngestMsg::CloseSource { source, summary })
                .unwrap();
        }
    }

    #[test]
    fn live_consumer_demuxes_sysids_and_compids() {
        let (sink, rx) = Sink::new();
        let metrics = Arc::new(MetricsRegistry::new());
        let stats = Arc::new(LiveIngestStats::default());
        let mut consumer = LiveConsumer::new(
            Endpoint::UdpServer {
                bind: "127.0.0.1:14550".parse().unwrap(),
            },
            sink,
            metrics,
            Arc::clone(&stats),
            None,
        );

        consumer.accept(frame(1, 1, attitude(1.0)));
        consumer.accept(frame(2, 1, attitude(2.0)));
        consumer.accept(frame(1, 2, attitude(3.0)));
        consumer.flush_all();

        let mut opens = Vec::new();
        let mut batches = Vec::new();
        for _ in 0..5 {
            match rx.try_recv().unwrap() {
                IngestMsg::OpenSource { key, kind, .. } => {
                    assert_eq!(kind, SourceKind::Live);
                    opens.push(key);
                }
                IngestMsg::Batch(batch) => batches.push(batch.topic().to_owned()),
                other => panic!("unexpected {other:?}"),
            }
        }
        opens.sort();
        batches.sort();
        assert_eq!(opens.len(), 2);
        assert!(opens[0].contains("sysid1"));
        assert!(opens[1].contains("sysid2"));
        assert_eq!(batches, vec!["ATTITUDE", "ATTITUDE", "ATTITUDE[1]"]);
        assert_eq!(stats.snapshot().rows, 3);
    }

    #[test]
    fn recorder_writes_tlog_envelope() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("delog-recorder-{}.tlog", std::process::id()));
        {
            let mut recorder = TlogRecorder::create(&path).unwrap();
            recorder.write_frame(42, &[0xFE, 1, 2]).unwrap();
            recorder.flush().unwrap();
            assert_eq!(recorder.records(), 1);
            assert_eq!(recorder.bytes(), 11);
        }
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(&bytes[..8], &42_i64.to_be_bytes());
        assert_eq!(&bytes[8..], &[0xFE, 1, 2]);
    }

    #[test]
    fn recorder_round_trips_through_tlog_parser() {
        let path = std::env::temp_dir().join(format!(
            "delog-recorder-roundtrip-{}.tlog",
            std::process::id()
        ));
        {
            let mut recorder = TlogRecorder::create(&path).unwrap();
            recorder
                .write_frame(1_700_000_000_000_000, &v2(7, 3, 1, &attitude(4.0)))
                .unwrap();
            recorder.flush().unwrap();
        }

        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let (mut sink, rx) = Sink::new();
        let ctl = ParseCtl::new(CancelToken::new(), SourceId(1), bytes.len() as u64);
        let summary = TlogParser
            .parse(Box::new(std::io::Cursor::new(bytes)), &mut sink, &ctl)
            .unwrap();

        let mut topics = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let IngestMsg::Batch(batch) = msg {
                topics.push((batch.topic().to_owned(), batch.rows()));
            }
        }
        assert_eq!(summary.row_count, 1);
        assert_eq!(topics, vec![("ATTITUDE".to_owned(), 1)]);
    }

    #[test]
    fn consumer_closes_live_sources_with_summary() {
        let (sink, rx) = Sink::new();
        let metrics = Arc::new(MetricsRegistry::new());
        let stats = Arc::new(LiveIngestStats::default());
        let mut consumer = LiveConsumer::new(
            Endpoint::UdpServer {
                bind: "127.0.0.1:14551".parse().unwrap(),
            },
            sink,
            metrics,
            stats,
            None,
        );
        consumer.accept(frame(1, 1, attitude(1.0)));
        consumer.flush_all();
        consumer.close_sources();

        let mut saw_batch = false;
        let mut saw_close = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                IngestMsg::Batch(_) => saw_batch = true,
                IngestMsg::CloseSource {
                    source: SourceId(1),
                    summary,
                } if summary.row_count == 1 && summary.topic_count == 1 => saw_close = true,
                _ => {}
            }
        }
        assert!(saw_batch);
        assert!(saw_close);
    }
}
