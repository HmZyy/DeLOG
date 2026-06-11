//! MAVLink `.tlog` parser (PLAN.md §6.4, PAR-11).
//!
//! A tlog is a flat sequence of records, each an 8-byte **big-endian Unix-µs**
//! capture timestamp followed by a raw MAVLink v1/v2 frame. The envelope is the
//! only framing; a frame's length lives in its own MAVLink header. This parser
//! reuses the shared decoder ([`crate::mavlink::frame_len`] /
//! [`crate::mavlink::decode_frame`]) and field extractor
//! ([`crate::mavlink::extract_fields`]) — exactly the code path live streaming
//! uses — so the tlog and the wire share one set of bugs and recordings
//! round-trip by construction (§7.5).
//!
//! One topic per MAVLink message type, columns are the message's flattened
//! fields, and the envelope timestamp drives the time axis. Streams are keyed by
//! `(sysid, compid, message)`; the first stream for a message keeps the bare
//! name and any later `(sysid, compid)` emitting the same message gets a
//! `message[N]` instance suffix (§4.3) — full sysid→source demux is LIV-06.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufReader, Read};
use std::sync::Arc;

use arrow::array::{
    ArrayRef, Float32Builder, Float64Builder, Int8Builder, Int16Builder, Int32Builder,
    Int64Builder, StringBuilder, UInt8Builder, UInt16Builder, UInt32Builder, UInt64Builder,
};
use arrow::datatypes::DataType;
use delog_core::diagnostics::Diag;
use delog_core::identity::{SourceId, SourceMetadata};
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch};
use delog_core::parse_ctl::ParseCtl;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::time::TimeRange;
use mavlink::Message;
use mavlink::dialects::ardupilotmega::MavMessage;

use crate::mavlink::{Scalar, decode_frame, extract_fields, frame_len};
use crate::parser::{LogParser, ParseError, ReadSeek, Sniff};

const V1_MAGIC: u8 = 0xFE;
const V2_MAGIC: u8 = 0xFD;
/// Capture-timestamp envelope width.
const TS_LEN: usize = 8;
/// Bytes of header needed to compute a frame's length: v2 reads the incompat
/// flag at index 2, v1 only the length at index 1.
const V2_HEADER_PEEK: usize = 3;
const V1_HEADER_PEEK: usize = 2;
const BATCH_ROWS: usize = 8192;
/// Rate-limit for repetitive skip diagnostics.
const SKIP_DIAG_INTERVAL: u64 = 1024;

#[derive(Debug, Default)]
pub struct TlogParser;

impl LogParser for TlogParser {
    fn name(&self) -> &'static str {
        "tlog"
    }

    fn sniff(&self, head: &[u8]) -> Sniff {
        if head.len() < TS_LEN + 1 {
            return Sniff::no();
        }
        let frame = &head[TS_LEN..];
        if frame[0] != V1_MAGIC && frame[0] != V2_MAGIC {
            return Sniff::no();
        }
        match frame_len(frame) {
            Some(len) if frame.len() >= len => {
                if decode_frame(&frame[..len]).is_some() {
                    Sniff::new(96, "µs envelope + CRC-valid MAVLink frame")
                } else {
                    // Magic at the envelope offset but the CRC fails — too weak
                    // to claim on its own; let the picker decide.
                    Sniff::new(40, "µs-envelope magic without a CRC-valid frame")
                }
            }
            // First frame runs past the sniff head; magic alone is suggestive.
            _ => Sniff::new(55, "µs-envelope MAVLink magic"),
        }
    }

    fn parse(
        &self,
        src: Box<dyn ReadSeek>,
        sink: &mut dyn IngestSink,
        ctl: &ParseCtl,
    ) -> Result<ParseSummary, ParseError> {
        Decoder::new(sink, ctl).run(src)
    }
}

/// A per-message-type accumulator. MAVLink messages of a given type always
/// extract the same fields in the same order, so the schema is built once on
/// first sight and rows append by position thereafter.
struct Topic {
    schema: Arc<TopicSchema>,
    ts: Int64Builder,
    cols: Vec<Col>,
    rows: usize,
}

impl Topic {
    fn take_batch(&mut self, source: SourceId) -> ParsedBatch {
        let timestamps = self.ts.finish();
        let columns = self.cols.iter_mut().map(Col::finish).collect();
        self.rows = 0;
        ParsedBatch::new(source, Arc::clone(&self.schema), timestamps, columns)
    }
}

type StreamKey = (u8, u8, &'static str);

struct Decoder<'a> {
    sink: &'a mut dyn IngestSink,
    ctl: &'a ParseCtl,
    source: SourceId,
    streams: HashMap<StreamKey, Topic>,
    /// Distinct `(sysid, compid)` streams already registered per message name,
    /// used to assign instance suffixes without renaming earlier topics.
    instances: HashMap<&'static str, u32>,
    unknown_seen: HashSet<u32>,
    crc_failures: u64,
    invalid_timestamps: u64,
    row_count: u64,
    diagnostics: u64,
    time_range: Option<TimeRange>,
}

impl<'a> Decoder<'a> {
    fn new(sink: &'a mut dyn IngestSink, ctl: &'a ParseCtl) -> Self {
        Self {
            sink,
            ctl,
            source: ctl.source(),
            streams: HashMap::new(),
            instances: HashMap::new(),
            unknown_seen: HashSet::new(),
            crc_failures: 0,
            invalid_timestamps: 0,
            row_count: 0,
            diagnostics: 0,
            time_range: None,
        }
    }

    fn run(mut self, src: Box<dyn ReadSeek>) -> Result<ParseSummary, ParseError> {
        let mut r = ByteReader::new(Box::new(BufReader::new(src)));
        let mut record_index = 0u64;
        loop {
            if !r.fill(TS_LEN)? {
                if r.available() > 0 {
                    self.diag(
                        Diag::warning("tlog-truncated", "trailing bytes shorter than a timestamp")
                            .at_byte(r.offset()),
                    );
                }
                break;
            }
            let ts_us = i64::from_be_bytes(r.peek(TS_LEN).try_into().expect("8-byte peek"));
            let record_offset = r.offset();
            r.consume(TS_LEN);

            if !r.fill(1)? {
                self.diag(
                    Diag::warning("tlog-truncated", "timestamp at end of file with no frame")
                        .at_byte(record_offset),
                );
                break;
            }
            let magic = r.peek(1)[0];
            if magic != V1_MAGIC && magic != V2_MAGIC {
                // The frame length is the only delimiter; a non-magic byte here
                // means envelope sync is lost and cannot be recovered. Keep the
                // data parsed so far (§6.1: torn tails are the logs you need).
                self.diag(
                    Diag::warning(
                        "tlog-desync",
                        format!(
                            "expected a MAVLink frame after the timestamp, found 0x{magic:02X}"
                        ),
                    )
                    .at_byte(r.offset()),
                );
                break;
            }

            let header_peek = if magic == V2_MAGIC {
                V2_HEADER_PEEK
            } else {
                V1_HEADER_PEEK
            };
            if !r.fill(header_peek)? {
                self.diag(
                    Diag::warning("tlog-truncated", "frame header runs past end of file")
                        .at_byte(r.offset()),
                );
                break;
            }
            let Some(len) = frame_len(r.peek(header_peek)) else {
                self.diag(
                    Diag::warning("tlog-truncated", "frame header runs past end of file")
                        .at_byte(r.offset()),
                );
                break;
            };
            if !r.fill(len)? {
                self.diag(
                    Diag::warning("tlog-truncated", "frame extends past end of file")
                        .at_byte(r.offset()),
                );
                break;
            }

            // A bad CRC trusts the declared length and skips exactly one frame,
            // so the envelope stays in sync for the next record.
            match decode_frame(r.peek(len)) {
                None => self.note_crc_failure(r.offset()),
                Some(decoded) => match decoded.message.as_ref() {
                    Some(msg) => self.ingest(
                        ts_us,
                        decoded.system_id,
                        decoded.component_id,
                        msg,
                        r.offset(),
                    ),
                    None => self.note_unknown(decoded.message_id, r.offset()),
                },
            }
            r.consume(len);

            record_index += 1;
            if self.ctl.cancelled_at(record_index) {
                self.flush_all();
                let summary = self.summary();
                self.sink.close_source(self.source, summary);
                return Err(ParseError::Cancelled);
            }
            self.ctl.report_progress(self.sink, r.offset());
        }

        self.flush_all();
        let summary = self.summary();
        self.sink.close_source(self.source, summary.clone());
        Ok(summary)
    }

    fn ingest(&mut self, ts_us: i64, sys: u8, comp: u8, msg: &MavMessage, offset: u64) {
        if ts_us < 0 {
            self.invalid_timestamps += 1;
            if self.invalid_timestamps == 1
                || self.invalid_timestamps.is_multiple_of(SKIP_DIAG_INTERVAL)
            {
                self.diag(
                    Diag::warning(
                        "tlog-invalid-timestamp",
                        format!(
                            "skipped {} row(s) with a negative timestamp",
                            self.invalid_timestamps
                        ),
                    )
                    .at_byte(offset),
                );
            }
            return;
        }

        let fields = extract_fields(msg);
        let name = msg.message_name();
        let key: StreamKey = (sys, comp, name);

        if !self.streams.contains_key(&key) {
            let instance = {
                let counter = self.instances.entry(name).or_insert(0);
                let i = *counter;
                *counter += 1;
                i
            };
            let topic_name = if instance == 0 {
                name.to_owned()
            } else {
                format!("{name}[{instance}]")
            };
            let field_schemas: Vec<FieldSchema> = fields
                .iter()
                .filter_map(|(fname, scalar)| {
                    FieldSchema::new(fname.clone(), scalar_dtype(scalar), None::<String>, 1.0).ok()
                })
                .collect();
            if field_schemas.len() != fields.len() {
                self.diag(
                    Diag::warning(
                        "tlog-bad-field",
                        format!("`{name}` has an unrepresentable field; topic skipped"),
                    )
                    .at_byte(offset),
                );
                return;
            }
            let schema = match TopicSchema::new(topic_name, field_schemas) {
                Ok(schema) => Arc::new(schema),
                Err(err) => {
                    self.diag(
                        Diag::warning("tlog-bad-schema", format!("`{name}`: {err}"))
                            .at_byte(offset),
                    );
                    return;
                }
            };
            let cols = fields.iter().map(|(_, s)| Col::for_scalar(s)).collect();
            self.streams.insert(
                key,
                Topic {
                    schema,
                    ts: Int64Builder::new(),
                    cols,
                    rows: 0,
                },
            );
        }

        let (appended, full) = {
            let topic = self.streams.get_mut(&key).expect("inserted above");
            if fields.len() != topic.cols.len() {
                (false, false)
            } else {
                topic.ts.append_value(ts_us);
                for (col, (_, scalar)) in topic.cols.iter_mut().zip(&fields) {
                    if !col.append(scalar) {
                        col.append_null();
                    }
                }
                topic.rows += 1;
                (true, topic.rows >= BATCH_ROWS)
            }
        };
        if !appended {
            self.diag(
                Diag::warning(
                    "tlog-field-drift",
                    format!("`{name}` field set changed mid-log; row skipped"),
                )
                .at_byte(offset),
            );
            return;
        }

        self.row_count += 1;
        self.time_range = Some(match self.time_range {
            Some(range) => range.include(ts_us),
            None => TimeRange::point(ts_us),
        });
        if full {
            let batch = self
                .streams
                .get_mut(&key)
                .expect("inserted above")
                .take_batch(self.source);
            self.sink.submit(batch);
        }
    }

    fn note_crc_failure(&mut self, offset: u64) {
        self.crc_failures += 1;
        if self.crc_failures == 1 || self.crc_failures.is_multiple_of(SKIP_DIAG_INTERVAL) {
            self.diag(
                Diag::warning(
                    "tlog-crc",
                    format!("skipped {} frame(s) failing CRC", self.crc_failures),
                )
                .at_byte(offset),
            );
        }
    }

    fn note_unknown(&mut self, message_id: u32, offset: u64) {
        if self.unknown_seen.insert(message_id) {
            self.diag(
                Diag::info(
                    "tlog-unknown-message",
                    format!("message id {message_id} is not in the ardupilotmega dialect; skipped"),
                )
                .at_byte(offset),
            );
        }
    }

    fn flush_all(&mut self) {
        let keys: Vec<StreamKey> = self.streams.keys().copied().collect();
        for key in keys {
            if let Some(topic) = self.streams.get_mut(&key)
                && topic.rows > 0
            {
                let batch = topic.take_batch(self.source);
                self.sink.submit(batch);
            }
        }
    }

    fn diag(&mut self, diag: Diag) {
        self.diagnostics += 1;
        self.sink.diagnostic(diag.with_source(self.source));
    }

    fn summary(&self) -> ParseSummary {
        ParseSummary {
            topic_count: self.streams.len() as u64,
            row_count: self.row_count,
            time_range: self.time_range,
            diagnostics: self.diagnostics,
            source_meta: SourceMetadata::default(),
        }
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

/// A typed Arrow column builder. `append` returns `false` on a dtype mismatch
/// (a malformed stream changing field types mid-log); the caller appends a null.
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

/// A minimal buffered reader with byte-offset tracking and look-ahead. The tlog
/// envelope has no record length, so framing needs to peek a frame's header
/// before committing to consume it.
struct ByteReader {
    inner: Box<dyn Read>,
    buf: Vec<u8>,
    start: usize,
    offset: u64,
    eof: bool,
}

impl ByteReader {
    fn new(inner: Box<dyn Read>) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            start: 0,
            offset: 0,
            eof: false,
        }
    }

    fn available(&self) -> usize {
        self.buf.len() - self.start
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    /// Ensure at least `n` unconsumed bytes are buffered. Returns `false` at a
    /// clean EOF before `n` bytes arrive.
    fn fill(&mut self, n: usize) -> Result<bool, ParseError> {
        while self.available() < n && !self.eof {
            if self.start >= self.buf.len() {
                self.buf.clear();
                self.start = 0;
            } else if self.start > 65_536 {
                self.buf.drain(..self.start);
                self.start = 0;
            }
            let mut tmp = [0u8; 8192];
            match self.inner.read(&mut tmp) {
                Ok(0) => self.eof = true,
                Ok(k) => self.buf.extend_from_slice(&tmp[..k]),
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ParseError::Io(e)),
            }
        }
        Ok(self.available() >= n)
    }

    fn peek(&self, n: usize) -> &[u8] {
        &self.buf[self.start..self.start + n]
    }

    fn consume(&mut self, n: usize) {
        self.start += n;
        self.offset += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use arrow::array::{Array, Float32Array, Int32Array, UInt8Array, UInt32Array};
    use delog_core::ingest::SourceKind;
    use delog_core::parse_ctl::CancelToken;

    use crate::parser::SNIFF_CONFIDENCE;
    use mavlink::dialects::ardupilotmega::{ATTITUDE_DATA, GPS_RAW_INT_DATA, HEARTBEAT_DATA};
    use mavlink::{MAVLinkV1MessageRaw, MAVLinkV2MessageRaw, MavHeader};

    use super::*;

    #[derive(Default)]
    struct Collect {
        batches: Vec<ParsedBatch>,
        diags: Vec<Diag>,
    }
    impl IngestSink for Collect {
        fn open_source(&mut self, _key: &str, _kind: SourceKind) -> SourceId {
            SourceId(0)
        }
        fn submit(&mut self, batch: ParsedBatch) {
            self.batches.push(batch);
        }
        fn diagnostic(&mut self, diag: Diag) {
            self.diags.push(diag);
        }
        fn progress(&mut self, _source: SourceId, _frac: f32) {}
        fn close_source(&mut self, _source: SourceId, _summary: ParseSummary) {}
    }

    fn header(sys: u8, comp: u8, seq: u8) -> MavHeader {
        MavHeader {
            system_id: sys,
            component_id: comp,
            sequence: seq,
        }
    }

    fn attitude(roll: f32) -> MavMessage {
        MavMessage::ATTITUDE(ATTITUDE_DATA {
            time_boot_ms: 1_000,
            roll,
            pitch: 0.5,
            yaw: -0.25,
            rollspeed: 0.0,
            pitchspeed: 0.0,
            yawspeed: 0.0,
        })
    }

    fn gps_raw_int(lat: i32, lon: i32) -> MavMessage {
        MavMessage::GPS_RAW_INT(GPS_RAW_INT_DATA {
            lat,
            lon,
            satellites_visible: 9,
            ..Default::default()
        })
    }

    fn v2(sys: u8, comp: u8, seq: u8, msg: &MavMessage) -> Vec<u8> {
        let mut raw = MAVLinkV2MessageRaw::new();
        raw.serialize_message(header(sys, comp, seq), msg);
        raw.raw_bytes().to_vec()
    }

    fn v1(sys: u8, comp: u8, seq: u8, msg: &MavMessage) -> Vec<u8> {
        let mut raw = MAVLinkV1MessageRaw::new();
        raw.serialize_message(header(sys, comp, seq), msg);
        raw.raw_bytes().to_vec()
    }

    /// Wrap a frame in the 8-byte big-endian µs capture-time envelope.
    fn record(ts_us: u64, frame: &[u8]) -> Vec<u8> {
        let mut out = ts_us.to_be_bytes().to_vec();
        out.extend_from_slice(frame);
        out
    }

    fn parse(buf: Vec<u8>) -> (ParseSummary, Collect) {
        let mut sink = Collect::default();
        let ctl = ParseCtl::new(CancelToken::new(), SourceId(0), buf.len() as u64);
        let summary = TlogParser
            .parse(Box::new(Cursor::new(buf)), &mut sink, &ctl)
            .expect("tlog parse");
        (summary, sink)
    }

    fn col<'b>(batch: &'b ParsedBatch, field: &str) -> &'b ArrayRef {
        let idx = batch
            .schema
            .field_index(field)
            .unwrap_or_else(|| panic!("no field {field}"));
        &batch.columns[idx]
    }

    fn f32s(batch: &ParsedBatch, field: &str) -> Vec<f32> {
        col(batch, field)
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap()
            .values()
            .to_vec()
    }

    /// A clean log with two `(sysid, compid)` streams of the same message and a
    /// third unsuffixed message — covers v1+v2 mixing and instance suffixing.
    fn golden_tlog() -> Vec<u8> {
        let mut buf = Vec::new();
        // ATTITUDE from the autopilot (1,1), v2.
        buf.extend(record(1_000_000, &v2(1, 1, 0, &attitude(1.0))));
        // HEARTBEAT from the GCS (255,190), v1 — a different message type.
        buf.extend(record(1_050_000, &v1(255, 190, 0, &heartbeat())));
        buf.extend(record(1_100_000, &v2(1, 1, 1, &attitude(2.0))));
        // GPS_RAW_INT from a second component (1,2) — same message later reused.
        buf.extend(record(
            1_150_000,
            &v1(1, 1, 2, &gps_raw_int(473_000_000, 85_000_000)),
        ));
        buf.extend(record(1_200_000, &v2(1, 1, 3, &attitude(3.0))));
        // A second GPS_RAW_INT stream from compid 2 → instance suffix.
        buf.extend(record(
            1_250_000,
            &v2(1, 2, 0, &gps_raw_int(-330_000_000, 1_515_000_000)),
        ));
        buf
    }

    fn heartbeat() -> MavMessage {
        MavMessage::HEARTBEAT(HEARTBEAT_DATA::default())
    }

    #[test]
    fn sniff_scores_a_crc_valid_envelope() {
        let buf = record(1_000_000, &v2(1, 1, 0, &attitude(1.0)));
        assert_eq!(TlogParser.sniff(&buf).score, 96);
        // Corrupting the CRC drops it below the confidence threshold.
        let mut bad = buf.clone();
        *bad.last_mut().unwrap() ^= 0xFF;
        assert!(TlogParser.sniff(&bad).score < SNIFF_CONFIDENCE);
        assert_eq!(TlogParser.sniff(b"ULog\x01\x12\x35\x01____").score, 0);
    }

    #[test]
    fn golden_topics_rows_and_values() {
        let (summary, sink) = parse(golden_tlog());

        // ATTITUDE, HEARTBEAT, GPS_RAW_INT, GPS_RAW_INT[1].
        assert_eq!(summary.topic_count, 4);
        assert_eq!(summary.row_count, 6);
        assert_eq!(summary.diagnostics, 0);
        assert!(sink.diags.is_empty());

        let batch = |topic: &str| {
            sink.batches
                .iter()
                .find(|b| b.topic() == topic)
                .unwrap_or_else(|| panic!("no batch for {topic}"))
        };

        let att = batch("ATTITUDE");
        assert_eq!(att.timestamps.values(), &[1_000_000, 1_100_000, 1_200_000]);
        assert_eq!(f32s(att, "roll"), vec![1.0, 2.0, 3.0]);
        assert_eq!(f32s(att, "yaw"), vec![-0.25, -0.25, -0.25]);
        // The envelope timestamp drives the axis; the message's own boot-time
        // field stays an ordinary column.
        let boot = col(att, "time_boot_ms")
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        assert_eq!(boot.values(), &[1_000, 1_000, 1_000]);

        let gps = batch("GPS_RAW_INT");
        assert_eq!(gps.timestamps.values(), &[1_150_000]);
        let lat = col(gps, "lat")
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        assert_eq!(lat.values(), &[473_000_000]);
        let sats = col(gps, "satellites_visible")
            .as_any()
            .downcast_ref::<UInt8Array>()
            .unwrap();
        assert_eq!(sats.values(), &[9]);

        // Second compid for the same message → instance-suffixed topic.
        let gps1 = batch("GPS_RAW_INT[1]");
        assert_eq!(gps1.timestamps.values(), &[1_250_000]);

        // HEARTBEAT enum fields land as Utf8 variant names.
        let hb = batch("HEARTBEAT");
        assert_eq!(hb.timestamps.values(), &[1_050_000]);
        assert_eq!(
            hb.schema.field_by_name("mavtype").map(|f| f.dtype.clone()),
            Some(DataType::Utf8)
        );
    }

    #[test]
    fn corrupt_crc_frame_is_skipped_and_the_envelope_stays_synced() {
        let mut bad = v2(1, 1, 0, &attitude(9.0));
        *bad.last_mut().unwrap() ^= 0xFF; // break the CRC, keep the length
        let mut buf = record(1_000_000, &bad);
        buf.extend(record(1_100_000, &v2(1, 1, 1, &attitude(2.0))));

        let (summary, sink) = parse(buf);
        assert_eq!(summary.row_count, 1);
        assert_eq!(
            f32s(
                sink.batches
                    .iter()
                    .find(|b| b.topic() == "ATTITUDE")
                    .unwrap(),
                "roll"
            ),
            vec![2.0]
        );
        assert!(sink.diags.iter().any(|d| d.code == "tlog-crc"));
    }

    #[test]
    fn unknown_message_id_is_diagnosed_once_and_skipped() {
        // Hand-craft a v2 frame with an unallocated message id but a valid CRC.
        let frame = unknown_message_frame();
        let mut buf = record(1_000_000, &frame);
        buf.extend(record(1_100_000, &frame)); // second occurrence: no new diag
        buf.extend(record(1_200_000, &v2(1, 1, 0, &attitude(1.0))));

        let (summary, sink) = parse(buf);
        assert_eq!(summary.row_count, 1); // only the ATTITUDE row
        assert_eq!(summary.topic_count, 1);
        let unknown = sink
            .diags
            .iter()
            .filter(|d| d.code == "tlog-unknown-message")
            .count();
        assert_eq!(unknown, 1);
    }

    /// A v2 frame carrying message id `0x00FFFF` (not in the dialect) with a
    /// correctly computed CRC, so it passes framing but fails to decode.
    fn unknown_message_frame() -> Vec<u8> {
        let payload = [0u8; 1];
        let mut frame = vec![
            V2_MAGIC,
            payload.len() as u8,
            0, // incompat
            0, // compat
            0, // seq
            1, // sysid
            1, // compid
            0xFF,
            0xFF,
            0x00, // message id 0x00FFFF
        ];
        frame.extend_from_slice(&payload);
        let crc = mavlink::calculate_crc(&frame[1..], MavMessage::extra_crc(0x00FFFF));
        frame.extend_from_slice(&crc.to_le_bytes());
        frame
    }

    #[test]
    fn truncated_tail_keeps_prior_data_and_diagnoses() {
        let mut buf = record(1_000_000, &v2(1, 1, 0, &attitude(1.0)));
        let full = record(1_100_000, &v2(1, 1, 1, &attitude(2.0)));
        buf.extend_from_slice(&full[..full.len() - 3]); // tear the last frame

        let (summary, sink) = parse(buf);
        assert_eq!(summary.row_count, 1);
        assert!(sink.diags.iter().any(|d| d.code == "tlog-truncated"));
    }

    #[test]
    fn desync_after_timestamp_stops_cleanly() {
        let mut buf = record(1_000_000, &v2(1, 1, 0, &attitude(1.0)));
        buf.extend(0u64.to_be_bytes()); // a timestamp...
        buf.extend([0x12, 0x34, 0x56]); // ...followed by non-frame bytes

        let (summary, sink) = parse(buf);
        assert_eq!(summary.row_count, 1);
        assert!(sink.diags.iter().any(|d| d.code == "tlog-desync"));
    }
}
