//! PX4 ULog `.ulg` parser.
//!
//! This implements the structural parser: the 16-byte header, length-prefixed
//! messages, `F/I/P` definitions, subscriptions (`A`), data (`D`) and sync (`S`)
//! records. Nested message formats are flattened to dotted field paths,
//! `_padding*` fields are skipped, and PX4 `multi_id` subscriptions become
//! `topic[N]` names.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanBuilder, Float32Builder, Float64Builder, Int8Builder, Int16Builder,
    Int32Builder, Int64Builder, StringBuilder, UInt8Builder, UInt16Builder, UInt32Builder,
    UInt64Builder,
};
use arrow::datatypes::DataType;
use delog_core::diagnostics::Diag;
use delog_core::identity::{AutoMarker, SourceId, SourceMetadata, SourceParam};
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch};
use delog_core::parse_ctl::ParseCtl;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::time::TimeRange;

use crate::parser::{LogParser, ParseError, ReadSeek, Sniff};

const MAGIC: &[u8; 7] = b"ULog\x01\x12\x35";
const HEADER_LEN: usize = 16;
const BATCH_ROWS: usize = 8192;
const START_SKEW_US: u64 = 1_000_000;
const MAX_LOG_SPAN_US: u64 = 24 * 60 * 60 * 1_000_000;
const INVALID_TS_DIAG_INTERVAL: u64 = 1024;

#[derive(Debug, Default)]
pub struct ULogParser;

impl LogParser for ULogParser {
    fn name(&self) -> &'static str {
        "ulog"
    }

    fn sniff(&self, head: &[u8]) -> Sniff {
        if head.starts_with(MAGIC) {
            Sniff::new(99, "ULog magic header")
        } else {
            Sniff::no()
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

#[derive(Debug, Clone)]
struct RawField {
    ty: String,
    name: String,
    array_len: usize,
}

#[derive(Debug, Clone)]
struct RawFormat {
    fields: Vec<RawField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarKind {
    Bool,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Char,
    CharString(usize),
}

impl ScalarKind {
    fn width(self) -> usize {
        match self {
            Self::Bool | Self::I8 | Self::U8 | Self::Char => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::I64 | Self::U64 | Self::F64 => 8,
            Self::CharString(len) => len,
        }
    }

    fn dtype(self) -> DataType {
        match self {
            Self::Bool => DataType::Boolean,
            Self::I8 => DataType::Int8,
            Self::I16 => DataType::Int16,
            Self::I32 => DataType::Int32,
            Self::I64 => DataType::Int64,
            Self::U8 => DataType::UInt8,
            Self::U16 => DataType::UInt16,
            Self::U32 => DataType::UInt32,
            Self::U64 => DataType::UInt64,
            Self::F32 => DataType::Float32,
            Self::F64 => DataType::Float64,
            Self::Char | Self::CharString(_) => DataType::Utf8,
        }
    }
}

#[derive(Debug, Clone)]
struct FlatField {
    name: String,
    offset: usize,
    kind: ScalarKind,
}

#[derive(Debug, Clone)]
struct Layout {
    width: usize,
    fields: Vec<FlatField>,
}

#[derive(Debug, Clone)]
struct Plan {
    topic_name: String,
    timestamp_offset: usize,
    emits: Vec<FlatField>,
    schema: Arc<TopicSchema>,
}

#[derive(Debug, Clone)]
struct Subscription {
    format_name: String,
    multi_id: u8,
    plan: Option<Plan>,
}

struct TopicAccum {
    schema: Arc<TopicSchema>,
    ts: Int64Builder,
    cols: Vec<ColBuilder>,
    rows: usize,
    last_ts: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
struct PendingDropout {
    duration_ms: u16,
    before_us: i64,
    byte_offset: u64,
}

struct Decoder<'a> {
    sink: &'a mut dyn IngestSink,
    ctl: &'a ParseCtl,
    source: SourceId,
    formats: HashMap<String, RawFormat>,
    subscriptions: HashMap<u16, Subscription>,
    topics: HashMap<String, TopicAccum>,
    start_timestamp_us: u64,
    invalid_timestamps: u64,
    last_data_timestamp_us: Option<i64>,
    pending_dropouts: Vec<PendingDropout>,
    params: Vec<SourceParam>,
    auto_markers: Vec<AutoMarker>,
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
            formats: HashMap::new(),
            subscriptions: HashMap::new(),
            topics: HashMap::new(),
            start_timestamp_us: 0,
            invalid_timestamps: 0,
            last_data_timestamp_us: None,
            pending_dropouts: Vec::new(),
            params: Vec::new(),
            auto_markers: Vec::new(),
            row_count: 0,
            diagnostics: 0,
            time_range: None,
        }
    }

    fn run(mut self, src: Box<dyn ReadSeek>) -> Result<ParseSummary, ParseError> {
        let mut reader = Reader::new(Box::new(BufReader::new(src)));
        let header =
            reader
                .read_exact(HEADER_LEN)?
                .ok_or_else(|| ParseError::UnsupportedFormat {
                    detail: "truncated ULog header".to_owned(),
                })?;
        if !header.starts_with(MAGIC) {
            return Err(ParseError::UnsupportedFormat {
                detail: "missing ULog magic header".to_owned(),
            });
        }
        self.start_timestamp_us = u64::from_le_bytes(read_8(&header, 8));

        let mut record_index = 0u64;
        while let Some((ty, payload, msg_offset)) = reader.next_msg()? {
            match ty {
                b'B' => self.read_flag_bits(&payload, msg_offset)?,
                b'F' => self.read_format(&payload, msg_offset),
                b'I' => {}
                b'P' => self.read_parameter(&payload, msg_offset),
                b'A' => self.read_add_logged(&payload, msg_offset),
                b'D' => self.read_data(&payload, msg_offset),
                b'L' => self.read_logged_message(&payload, msg_offset),
                b'S' => {}
                b'O' => self.read_dropout(&payload, msg_offset),
                _ => {}
            }

            record_index += 1;
            if self.ctl.cancelled_at(record_index) {
                self.flush_all();
                self.sink.close_source(self.source, self.summary());
                return Err(ParseError::Cancelled);
            }
            self.ctl.report_progress(self.sink, reader.offset());
        }

        self.flush_all();
        let summary = self.summary();
        self.sink.close_source(self.source, summary.clone());
        Ok(summary)
    }

    fn read_flag_bits(&mut self, payload: &[u8], msg_offset: u64) -> Result<(), ParseError> {
        if payload.len() < 40 {
            self.diagnostic(
                Diag::warning("ulog-short-flags", "short ULog flag-bits message")
                    .at_byte(msg_offset),
            );
            return Ok(());
        }
        let incompat = &payload[8..16];
        let unknown_first = incompat[0] & !0x01;
        let unknown_rest = incompat[1..].iter().any(|&b| b != 0);
        if unknown_first != 0 || unknown_rest {
            return Err(ParseError::Framing {
                byte_offset: msg_offset,
                detail: "ULog has unsupported incompatible flag bits".to_owned(),
            });
        }
        Ok(())
    }

    fn read_format(&mut self, payload: &[u8], msg_offset: u64) {
        let text = String::from_utf8_lossy(payload);
        let Some((name, fields)) = text.split_once(':') else {
            self.diagnostic(
                Diag::warning("ulog-bad-format", "format message has no ':' separator")
                    .at_byte(msg_offset),
            );
            return;
        };
        let parsed = fields
            .split(';')
            .filter(|part| !part.trim().is_empty())
            .filter_map(parse_field_decl)
            .collect::<Vec<_>>();
        self.formats
            .insert(name.trim().to_owned(), RawFormat { fields: parsed });
    }

    fn read_add_logged(&mut self, payload: &[u8], msg_offset: u64) {
        if payload.len() < 3 {
            self.diagnostic(
                Diag::warning("ulog-short-subscription", "short AddLogged message")
                    .at_byte(msg_offset),
            );
            return;
        }
        let multi_id = payload[0];
        let msg_id = u16::from_le_bytes([payload[1], payload[2]]);
        let format_name = String::from_utf8_lossy(&payload[3..]).trim().to_owned();
        let plan = self.build_plan(&format_name, multi_id, msg_offset);
        self.subscriptions.insert(
            msg_id,
            Subscription {
                format_name,
                multi_id,
                plan,
            },
        );
    }

    fn read_parameter(&mut self, payload: &[u8], msg_offset: u64) {
        let Some((ty, name, value_bytes)) = read_keyed_value(payload) else {
            self.diagnostic(
                Diag::warning("ulog-bad-param", "parameter message has a malformed key")
                    .at_byte(msg_offset),
            );
            return;
        };
        let Some(value) = metadata_value_to_string(&ty, value_bytes) else {
            self.diagnostic(
                Diag::warning(
                    "ulog-bad-param",
                    format!("parameter `{name}` has unsupported or truncated type `{ty}`"),
                )
                .at_byte(msg_offset),
            );
            return;
        };
        self.params.push(SourceParam { name, ty, value });
    }

    fn read_logged_message(&mut self, payload: &[u8], msg_offset: u64) {
        if payload.len() < 9 {
            self.diagnostic(
                Diag::warning("ulog-short-logged", "short logged-message record")
                    .at_byte(msg_offset),
            );
            return;
        }
        let level = payload[0];
        let raw_time = u64::from_le_bytes(read_8(payload, 1));
        let Some(time_us) = self.valid_timestamp(raw_time, "logged_message", msg_offset) else {
            return;
        };
        self.auto_markers.push(AutoMarker {
            time_us,
            level,
            text: c_str(&payload[9..]),
        });
    }

    fn read_dropout(&mut self, payload: &[u8], msg_offset: u64) {
        if payload.len() < 2 {
            self.diagnostic(
                Diag::warning("ulog-short-dropout", "short Dropout message").at_byte(msg_offset),
            );
            return;
        }
        let duration_ms = u16::from_le_bytes([payload[0], payload[1]]);
        let Some(before_us) = self.last_data_timestamp_us else {
            self.diagnostic(
                Diag::warning(
                    "ulog-dropout",
                    format!("dropout of {duration_ms} ms before any timestamped data"),
                )
                .at_byte(msg_offset),
            );
            return;
        };
        self.pending_dropouts.push(PendingDropout {
            duration_ms,
            before_us,
            byte_offset: msg_offset,
        });
    }

    fn read_data(&mut self, payload: &[u8], msg_offset: u64) {
        if payload.len() < 2 {
            self.diagnostic(
                Diag::warning("ulog-short-data", "short Data message").at_byte(msg_offset),
            );
            return;
        }
        let msg_id = u16::from_le_bytes([payload[0], payload[1]]);
        let data = &payload[2..];
        let Some(sub) = self.subscriptions.get(&msg_id).cloned() else {
            self.diagnostic(
                Diag::warning(
                    "ulog-unknown-subscription",
                    format!("data for unknown subscription id {msg_id}"),
                )
                .at_byte(msg_offset),
            );
            return;
        };
        let Some(plan) = sub.plan else {
            self.diagnostic(
                Diag::warning(
                    "ulog-unplanned-data",
                    format!(
                        "data for `{}`[{}] could not be decoded",
                        sub.format_name, sub.multi_id
                    ),
                )
                .at_byte(msg_offset),
            );
            return;
        };
        let Some(time_us) = read_u64(data, plan.timestamp_offset) else {
            self.diagnostic(
                Diag::warning(
                    "ulog-missing-timestamp",
                    format!("data for `{}` omits timestamp", plan.topic_name),
                )
                .at_byte(msg_offset),
            );
            return;
        };
        let Some(time_us) = self.valid_timestamp(time_us, &plan.topic_name, msg_offset) else {
            return;
        };
        self.materialize_dropouts(time_us);
        if plan
            .emits
            .iter()
            .any(|field| field.offset + field.kind.width() > data.len())
        {
            self.diagnostic(
                Diag::warning(
                    "ulog-short-data",
                    format!("data for `{}` is shorter than its format", plan.topic_name),
                )
                .at_byte(msg_offset),
            );
            return;
        }

        let accum = self
            .topics
            .entry(plan.topic_name.clone())
            .or_insert_with(|| TopicAccum {
                schema: Arc::clone(&plan.schema),
                ts: Int64Builder::new(),
                cols: plan
                    .emits
                    .iter()
                    .map(|f| ColBuilder::for_kind(f.kind))
                    .collect(),
                rows: 0,
                last_ts: None,
            });
        accum.ts.append_value(time_us);
        for (col, field) in accum.cols.iter_mut().zip(&plan.emits) {
            col.append(field.kind, data, field.offset);
        }
        accum.rows += 1;
        accum.last_ts = Some(time_us);
        self.row_count += 1;
        self.last_data_timestamp_us = Some(time_us);
        self.time_range = Some(match self.time_range {
            Some(r) => r.include(time_us),
            None => TimeRange::point(time_us),
        });
        if accum.rows >= BATCH_ROWS {
            let batch = accum.take_batch(self.source);
            self.sink.submit(batch);
        }
    }

    fn materialize_dropouts(&mut self, next_us: i64) {
        if self.pending_dropouts.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_dropouts);
        for dropout in pending {
            let gap_us = dropout_gap_time(dropout.before_us, next_us, dropout.duration_ms);
            self.diagnostic(
                Diag::warning(
                    "ulog-dropout",
                    format!("dropout of {} ms", dropout.duration_ms),
                )
                .at_byte(dropout.byte_offset)
                .at_time(gap_us),
            );
            let inserted = self.append_gap_marker(gap_us);
            if inserted > 0 {
                self.row_count += inserted as u64;
                self.time_range = Some(match self.time_range {
                    Some(r) => r.include(gap_us),
                    None => TimeRange::point(gap_us),
                });
            }
        }
    }

    fn append_gap_marker(&mut self, gap_us: i64) -> usize {
        let mut inserted = 0;
        for accum in self.topics.values_mut() {
            if accum.last_ts.is_none_or(|last_ts| last_ts > gap_us) {
                continue;
            }
            accum.ts.append_value(gap_us);
            for col in &mut accum.cols {
                col.append_null();
            }
            accum.rows += 1;
            accum.last_ts = Some(gap_us);
            inserted += 1;
        }
        inserted
    }

    fn build_plan(&mut self, format_name: &str, multi_id: u8, msg_offset: u64) -> Option<Plan> {
        let layout = match build_layout(format_name, &self.formats, &mut Vec::new()) {
            Ok(layout) => layout,
            Err(err) => {
                self.diagnostic(
                    Diag::warning("ulog-bad-layout", format!("{format_name}: {err}"))
                        .at_byte(msg_offset),
                );
                return None;
            }
        };
        let Some(timestamp) = layout
            .fields
            .iter()
            .find(|f| f.name == "timestamp" && f.kind == ScalarKind::U64)
            .cloned()
        else {
            self.diagnostic(
                Diag::info(
                    "ulog-no-timestamp",
                    format!("format `{format_name}` has no uint64_t timestamp; skipped"),
                )
                .at_byte(msg_offset),
            );
            return None;
        };

        let topic_name = format!("{format_name}[{multi_id}]");
        let emits = layout
            .fields
            .into_iter()
            .filter(|field| field.name != "timestamp")
            .collect::<Vec<_>>();
        let fields = emits
            .iter()
            .filter_map(|field| {
                FieldSchema::new(&field.name, field.kind.dtype(), None::<String>, 1.0).ok()
            })
            .collect::<Vec<_>>();
        let schema = Arc::new(TopicSchema::new(&topic_name, fields).ok()?);
        Some(Plan {
            topic_name,
            timestamp_offset: timestamp.offset,
            emits,
            schema,
        })
    }

    fn flush_all(&mut self) {
        let names: Vec<String> = self.topics.keys().cloned().collect();
        for name in names {
            if let Some(accum) = self.topics.get_mut(&name)
                && accum.rows > 0
            {
                let batch = accum.take_batch(self.source);
                self.sink.submit(batch);
            }
        }
    }

    fn valid_timestamp(&mut self, raw: u64, topic_name: &str, msg_offset: u64) -> Option<i64> {
        let valid_i64 = raw <= i64::MAX as u64;
        let valid_window = self.start_timestamp_us == 0
            || (raw.saturating_add(START_SKEW_US) >= self.start_timestamp_us
                && raw <= self.start_timestamp_us.saturating_add(MAX_LOG_SPAN_US));
        if valid_i64 && valid_window {
            return Some(raw as i64);
        }

        self.invalid_timestamps += 1;
        if self.invalid_timestamps == 1
            || self
                .invalid_timestamps
                .is_multiple_of(INVALID_TS_DIAG_INTERVAL)
        {
            self.diagnostic(
                Diag::warning(
                    "ulog-invalid-timestamp",
                    format!(
                        "skipped {} row(s) with invalid timestamp; latest `{topic_name}` timestamp was {raw}",
                        self.invalid_timestamps
                    ),
                )
                .at_byte(msg_offset),
            );
        }
        None
    }

    fn diagnostic(&mut self, diag: Diag) {
        self.diagnostics += 1;
        self.sink.diagnostic(diag.with_source(self.source));
    }

    fn summary(&self) -> ParseSummary {
        ParseSummary {
            topic_count: self.topics.len() as u64,
            row_count: self.row_count,
            time_range: self.time_range,
            diagnostics: self.diagnostics,
            source_meta: SourceMetadata {
                params: self.params.clone(),
                auto_markers: self.auto_markers.clone(),
            },
        }
    }
}

impl TopicAccum {
    fn take_batch(&mut self, source: SourceId) -> ParsedBatch {
        let timestamps = self.ts.finish();
        let columns = self.cols.iter_mut().map(ColBuilder::finish).collect();
        self.rows = 0;
        ParsedBatch::new(source, Arc::clone(&self.schema), timestamps, columns)
    }
}

enum ColBuilder {
    Bool(BooleanBuilder),
    I8(Int8Builder),
    I16(Int16Builder),
    I32(Int32Builder),
    I64(Int64Builder),
    U8(UInt8Builder),
    U16(UInt16Builder),
    U32(UInt32Builder),
    U64(UInt64Builder),
    F32(Float32Builder),
    F64(Float64Builder),
    Str(StringBuilder),
}

impl ColBuilder {
    fn for_kind(kind: ScalarKind) -> Self {
        match kind {
            ScalarKind::Bool => Self::Bool(BooleanBuilder::new()),
            ScalarKind::I8 => Self::I8(Int8Builder::new()),
            ScalarKind::I16 => Self::I16(Int16Builder::new()),
            ScalarKind::I32 => Self::I32(Int32Builder::new()),
            ScalarKind::I64 => Self::I64(Int64Builder::new()),
            ScalarKind::U8 => Self::U8(UInt8Builder::new()),
            ScalarKind::U16 => Self::U16(UInt16Builder::new()),
            ScalarKind::U32 => Self::U32(UInt32Builder::new()),
            ScalarKind::U64 => Self::U64(UInt64Builder::new()),
            ScalarKind::F32 => Self::F32(Float32Builder::new()),
            ScalarKind::F64 => Self::F64(Float64Builder::new()),
            ScalarKind::Char | ScalarKind::CharString(_) => Self::Str(StringBuilder::new()),
        }
    }

    fn append(&mut self, kind: ScalarKind, data: &[u8], off: usize) {
        match self {
            Self::Bool(b) => b.append_value(data[off] != 0),
            Self::I8(b) => b.append_value(data[off] as i8),
            Self::I16(b) => b.append_value(i16::from_le_bytes(read_2(data, off))),
            Self::I32(b) => b.append_value(i32::from_le_bytes(read_4(data, off))),
            Self::I64(b) => b.append_value(i64::from_le_bytes(read_8(data, off))),
            Self::U8(b) => b.append_value(data[off]),
            Self::U16(b) => b.append_value(u16::from_le_bytes(read_2(data, off))),
            Self::U32(b) => b.append_value(u32::from_le_bytes(read_4(data, off))),
            Self::U64(b) => b.append_value(u64::from_le_bytes(read_8(data, off))),
            Self::F32(b) => b.append_value(f32::from_le_bytes(read_4(data, off))),
            Self::F64(b) => b.append_value(f64::from_le_bytes(read_8(data, off))),
            Self::Str(b) => match kind {
                ScalarKind::Char => b.append_value(String::from_utf8_lossy(&data[off..off + 1])),
                ScalarKind::CharString(len) => {
                    b.append_value(c_str(&data[off..off + len]));
                }
                _ => unreachable!("string builder for non-string kind"),
            },
        }
    }

    fn append_null(&mut self) {
        match self {
            Self::Bool(b) => b.append_null(),
            Self::I8(b) => b.append_null(),
            Self::I16(b) => b.append_null(),
            Self::I32(b) => b.append_null(),
            Self::I64(b) => b.append_null(),
            Self::U8(b) => b.append_null(),
            Self::U16(b) => b.append_null(),
            Self::U32(b) => b.append_null(),
            Self::U64(b) => b.append_null(),
            Self::F32(b) => b.append_null(),
            Self::F64(b) => b.append_null(),
            Self::Str(b) => b.append_null(),
        }
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            Self::Bool(b) => Arc::new(b.finish()),
            Self::I8(b) => Arc::new(b.finish()),
            Self::I16(b) => Arc::new(b.finish()),
            Self::I32(b) => Arc::new(b.finish()),
            Self::I64(b) => Arc::new(b.finish()),
            Self::U8(b) => Arc::new(b.finish()),
            Self::U16(b) => Arc::new(b.finish()),
            Self::U32(b) => Arc::new(b.finish()),
            Self::U64(b) => Arc::new(b.finish()),
            Self::F32(b) => Arc::new(b.finish()),
            Self::F64(b) => Arc::new(b.finish()),
            Self::Str(b) => Arc::new(b.finish()),
        }
    }
}

struct Reader {
    inner: Box<dyn Read>,
    offset: u64,
}

impl Reader {
    fn new(inner: Box<dyn Read>) -> Self {
        Self { inner, offset: 0 }
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn read_exact(&mut self, len: usize) -> Result<Option<Vec<u8>>, ParseError> {
        let mut buf = vec![0u8; len];
        let mut filled = 0;
        while filled < len {
            match self.inner.read(&mut buf[filled..]) {
                Ok(0) if filled == 0 => return Ok(None),
                Ok(0) => {
                    return Err(ParseError::Framing {
                        byte_offset: self.offset,
                        detail: "truncated ULog message".to_owned(),
                    });
                }
                Ok(n) => {
                    filled += n;
                    self.offset += n as u64;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ParseError::Io(e)),
            }
        }
        Ok(Some(buf))
    }

    fn next_msg(&mut self) -> Result<Option<(u8, Vec<u8>, u64)>, ParseError> {
        let msg_offset = self.offset;
        let Some(header) = self.read_exact(3)? else {
            return Ok(None);
        };
        let len = u16::from_le_bytes([header[0], header[1]]) as usize;
        let ty = header[2];
        let payload = self.read_exact(len)?.unwrap_or_default();
        Ok(Some((ty, payload, msg_offset)))
    }
}

fn build_layout(
    format_name: &str,
    defs: &HashMap<String, RawFormat>,
    stack: &mut Vec<String>,
) -> Result<Layout, String> {
    if stack.iter().any(|name| name == format_name) {
        return Err("recursive format definition".to_owned());
    }
    let Some(format) = defs.get(format_name) else {
        return Err("referenced format is not defined".to_owned());
    };
    stack.push(format_name.to_owned());
    let mut fields = Vec::new();
    let mut offset = 0usize;
    for field in &format.fields {
        let count = field.array_len.max(1);
        if let Some(kind) = scalar_kind(&field.ty) {
            let elem_width = kind.width();
            if field.name.starts_with("_padding") {
                offset += elem_width * count;
                continue;
            }
            if kind == ScalarKind::Char && count > 1 {
                fields.push(FlatField {
                    name: field.name.clone(),
                    offset,
                    kind: ScalarKind::CharString(count),
                });
                offset += count;
            } else {
                for idx in 0..count {
                    fields.push(FlatField {
                        name: array_name(&field.name, count, idx),
                        offset: offset + elem_width * idx,
                        kind,
                    });
                }
                offset += elem_width * count;
            }
        } else {
            let nested = build_layout(&field.ty, defs, stack)?;
            for idx in 0..count {
                let base = array_name(&field.name, count, idx);
                for nested_field in &nested.fields {
                    fields.push(FlatField {
                        name: format!("{base}.{}", nested_field.name),
                        offset: offset + nested.width * idx + nested_field.offset,
                        kind: nested_field.kind,
                    });
                }
            }
            offset += nested.width * count;
        }
    }
    stack.pop();
    Ok(Layout {
        width: offset,
        fields,
    })
}

fn parse_field_decl(decl: &str) -> Option<RawField> {
    let mut parts = decl.split_whitespace();
    let ty = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (ty, ty_array_len) = parse_array(ty);
    let (name, name_array_len) = parse_array(name);
    Some(RawField {
        ty,
        name,
        array_len: name_array_len.max(ty_array_len),
    })
}

fn parse_array(text: &str) -> (String, usize) {
    let Some(open) = text.find('[') else {
        return (text.to_owned(), 1);
    };
    let Some(close) = text[open + 1..].find(']').map(|idx| idx + open + 1) else {
        return (text.to_owned(), 1);
    };
    let len = text[open + 1..close].parse::<usize>().unwrap_or(1).max(1);
    (text[..open].to_owned(), len)
}

fn scalar_kind(ty: &str) -> Option<ScalarKind> {
    Some(match ty {
        "bool" => ScalarKind::Bool,
        "int8_t" => ScalarKind::I8,
        "int16_t" => ScalarKind::I16,
        "int32_t" => ScalarKind::I32,
        "int64_t" => ScalarKind::I64,
        "uint8_t" => ScalarKind::U8,
        "uint16_t" => ScalarKind::U16,
        "uint32_t" => ScalarKind::U32,
        "uint64_t" => ScalarKind::U64,
        "float" => ScalarKind::F32,
        "double" => ScalarKind::F64,
        "char" => ScalarKind::Char,
        _ => return None,
    })
}

fn array_name(name: &str, count: usize, idx: usize) -> String {
    if count == 1 {
        name.to_owned()
    } else {
        format!("{name}[{idx}]")
    }
}

fn dropout_gap_time(before_us: i64, next_us: i64, duration_ms: u16) -> i64 {
    if next_us <= before_us {
        return next_us;
    }
    let duration_us = i64::from(duration_ms) * 1000;
    let candidate = next_us.saturating_sub(duration_us / 2);
    if candidate > before_us && candidate <= next_us {
        candidate
    } else {
        before_us + (next_us - before_us) / 2
    }
}

fn read_keyed_value(payload: &[u8]) -> Option<(String, String, &[u8])> {
    let (&key_len, rest) = payload.split_first()?;
    let key_len = key_len as usize;
    if rest.len() < key_len {
        return None;
    }
    let key = std::str::from_utf8(&rest[..key_len]).ok()?.trim();
    let (ty, name) = key.split_once(' ')?;
    let name = name.trim();
    (!ty.is_empty() && !name.is_empty())
        .then(|| (ty.trim().to_owned(), name.to_owned(), &rest[key_len..]))
}

fn metadata_value_to_string(ty: &str, bytes: &[u8]) -> Option<String> {
    let (base_ty, array_len) = parse_array(ty);
    let kind = scalar_kind(&base_ty)?;
    let count = array_len.max(1);
    if kind == ScalarKind::Char && count > 1 {
        return (bytes.len() >= count).then(|| c_str(&bytes[..count]));
    }
    let width = kind.width();
    if bytes.len() < width * count {
        return None;
    }
    let mut values = Vec::with_capacity(count);
    for idx in 0..count {
        values.push(scalar_value_to_string(kind, bytes, idx * width));
    }
    Some(values.join(","))
}

fn scalar_value_to_string(kind: ScalarKind, bytes: &[u8], off: usize) -> String {
    match kind {
        ScalarKind::Bool => (bytes[off] != 0).to_string(),
        ScalarKind::I8 => (bytes[off] as i8).to_string(),
        ScalarKind::I16 => i16::from_le_bytes(read_2(bytes, off)).to_string(),
        ScalarKind::I32 => i32::from_le_bytes(read_4(bytes, off)).to_string(),
        ScalarKind::I64 => i64::from_le_bytes(read_8(bytes, off)).to_string(),
        ScalarKind::U8 => bytes[off].to_string(),
        ScalarKind::U16 => u16::from_le_bytes(read_2(bytes, off)).to_string(),
        ScalarKind::U32 => u32::from_le_bytes(read_4(bytes, off)).to_string(),
        ScalarKind::U64 => u64::from_le_bytes(read_8(bytes, off)).to_string(),
        ScalarKind::F32 => f32::from_le_bytes(read_4(bytes, off)).to_string(),
        ScalarKind::F64 => f64::from_le_bytes(read_8(bytes, off)).to_string(),
        ScalarKind::Char => c_str(&bytes[off..off + 1]),
        ScalarKind::CharString(len) => c_str(&bytes[off..off + len]),
    }
}

fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn read_u64(p: &[u8], off: usize) -> Option<u64> {
    (off + 8 <= p.len()).then(|| u64::from_le_bytes(read_8(p, off)))
}

fn read_2(p: &[u8], off: usize) -> [u8; 2] {
    [p[off], p[off + 1]]
}
fn read_4(p: &[u8], off: usize) -> [u8; 4] {
    [p[off], p[off + 1], p[off + 2], p[off + 3]]
}
fn read_8(p: &[u8], off: usize) -> [u8; 8] {
    [
        p[off],
        p[off + 1],
        p[off + 2],
        p[off + 3],
        p[off + 4],
        p[off + 5],
        p[off + 6],
        p[off + 7],
    ]
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use arrow::array::{Array, Float32Array, Int16Array, Int32Array, StringArray, UInt8Array};
    use delog_core::ingest::SourceKind;
    use delog_core::parse_ctl::CancelToken;

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

    fn push_msg(buf: &mut Vec<u8>, ty: u8, payload: &[u8]) {
        buf.extend((payload.len() as u16).to_le_bytes());
        buf.push(ty);
        buf.extend(payload);
    }

    fn tiny_ulog() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend(MAGIC);
        buf.push(1);
        buf.extend(0u64.to_le_bytes());

        push_msg(
            &mut buf,
            b'F',
            b"vehicle_attitude:uint64_t timestamp;float[3] q;uint8_t _padding0[4];",
        );
        push_msg(&mut buf, b'F', b"position:float x;float y;float z;");
        push_msg(
            &mut buf,
            b'F',
            b"nested:uint64_t timestamp;position pos;char[8] label;int16_t temp;",
        );

        let mut sub = Vec::new();
        sub.push(2);
        sub.extend(10u16.to_le_bytes());
        sub.extend(b"nested");
        push_msg(&mut buf, b'A', &sub);

        let mut data = Vec::new();
        data.extend(10u16.to_le_bytes());
        data.extend(1_000u64.to_le_bytes());
        data.extend(1.0f32.to_le_bytes());
        data.extend(2.0f32.to_le_bytes());
        data.extend(3.0f32.to_le_bytes());
        data.extend(b"abc\0\0\0\0\0");
        data.extend((-7i16).to_le_bytes());
        push_msg(&mut buf, b'D', &data);

        buf
    }

    fn ulog_with_invalid_timestamp() -> Vec<u8> {
        let start = 1_000_000u64;
        let mut buf = Vec::new();
        buf.extend(MAGIC);
        buf.push(1);
        buf.extend(start.to_le_bytes());

        push_msg(&mut buf, b'F', b"test:uint64_t timestamp;float value;");
        let mut sub = Vec::new();
        sub.push(0);
        sub.extend(1u16.to_le_bytes());
        sub.extend(b"test");
        push_msg(&mut buf, b'A', &sub);

        for (ts, value) in [(u64::MAX, 1.0f32), (start + 10, 2.0f32)] {
            let mut data = Vec::new();
            data.extend(1u16.to_le_bytes());
            data.extend(ts.to_le_bytes());
            data.extend(value.to_le_bytes());
            push_msg(&mut buf, b'D', &data);
        }

        buf
    }

    fn ulog_with_dropout() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend(MAGIC);
        buf.push(1);
        buf.extend(0u64.to_le_bytes());

        push_msg(&mut buf, b'F', b"test:uint64_t timestamp;float value;");
        let mut sub = Vec::new();
        sub.push(0);
        sub.extend(1u16.to_le_bytes());
        sub.extend(b"test");
        push_msg(&mut buf, b'A', &sub);

        for (ts, value) in [(1_000u64, 1.0f32), (3_000, 2.0)] {
            let mut data = Vec::new();
            data.extend(1u16.to_le_bytes());
            data.extend(ts.to_le_bytes());
            data.extend(value.to_le_bytes());
            push_msg(&mut buf, b'D', &data);
            if ts == 1_000 {
                push_msg(&mut buf, b'O', &500u16.to_le_bytes());
            }
        }

        buf
    }

    fn ulog_with_metadata() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend(MAGIC);
        buf.push(1);
        buf.extend(0u64.to_le_bytes());

        let mut param = Vec::new();
        let key = b"float MPC_XY_CRUISE";
        param.push(key.len() as u8);
        param.extend(key);
        param.extend(5.5f32.to_le_bytes());
        push_msg(&mut buf, b'P', &param);

        let mut name = Vec::new();
        let key = b"char[8] SYS_NAME";
        name.push(key.len() as u8);
        name.extend(key);
        name.extend(b"px4\0\0\0\0\0");
        push_msg(&mut buf, b'P', &name);

        let mut logged = Vec::new();
        logged.push(6);
        logged.extend(12_345u64.to_le_bytes());
        logged.extend(b"armed and ready");
        push_msg(&mut buf, b'L', &logged);

        buf
    }

    fn parse(buf: Vec<u8>) -> (ParseSummary, Collect) {
        let mut sink = Collect::default();
        let ctl = ParseCtl::new(CancelToken::new(), SourceId(0), buf.len() as u64);
        let summary = ULogParser
            .parse(Box::new(Cursor::new(buf)), &mut sink, &ctl)
            .unwrap();
        (summary, sink)
    }

    #[test]
    fn sniff_scores_ulog_magic() {
        let mut head = Vec::from(&MAGIC[..]);
        head.extend([1, 0, 0, 0]);
        assert_eq!(ULogParser.sniff(&head).score, 99);
        assert_eq!(ULogParser.sniff(b"not ulog").score, 0);
    }

    /// A self-describing log with an array-flattened topic, a two-instance
    /// topic, padding to skip and several rows per topic.
    fn golden_ulog() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend(MAGIC);
        buf.push(1);
        buf.extend(0u64.to_le_bytes());

        push_msg(
            &mut buf,
            b'F',
            b"vehicle_attitude:uint64_t timestamp;float[4] q;uint8_t _padding0[4];",
        );
        push_msg(
            &mut buf,
            b'F',
            b"sensor_gps:uint64_t timestamp;int32_t lat;int32_t lon;uint8_t fix;",
        );

        let mut subscribe = |multi_id: u8, msg_id: u16, name: &[u8]| {
            let mut sub = Vec::new();
            sub.push(multi_id);
            sub.extend(msg_id.to_le_bytes());
            sub.extend(name);
            push_msg(&mut buf, b'A', &sub);
        };
        subscribe(0, 1, b"sensor_gps");
        subscribe(1, 2, b"sensor_gps");
        subscribe(0, 3, b"vehicle_attitude");

        let mut gps_row = |msg_id: u16, t: u64, lat: i32, lon: i32, fix: u8| {
            let mut data = Vec::new();
            data.extend(msg_id.to_le_bytes());
            data.extend(t.to_le_bytes());
            data.extend(lat.to_le_bytes());
            data.extend(lon.to_le_bytes());
            data.push(fix);
            push_msg(&mut buf, b'D', &data);
        };
        gps_row(1, 1_000, 473_000_000, 85_000_000, 3);
        gps_row(2, 1_200, -330_000_000, 1_515_000_000, 5);
        gps_row(1, 1_500, 473_000_010, 85_000_020, 4);

        let mut att_row = |t: u64, q: [f32; 4]| {
            let mut data = Vec::new();
            data.extend(3u16.to_le_bytes());
            data.extend(t.to_le_bytes());
            for v in q {
                data.extend(v.to_le_bytes());
            }
            data.extend([0u8; 4]);
            push_msg(&mut buf, b'D', &data);
        };
        att_row(1_000, [1.0, 0.0, 0.0, 0.5]);
        att_row(2_000, [0.9, 0.1, 0.0, 0.6]);

        buf
    }

    /// Golden table: topics, rows and raw values for the fixture log.
    #[test]
    fn golden_topics_rows_and_values() {
        let (summary, sink) = parse(golden_ulog());

        // sensor_gps[0](2) + sensor_gps[1](1) + vehicle_attitude[0](2).
        assert_eq!(summary.topic_count, 3);
        assert_eq!(summary.row_count, 5);
        assert_eq!(summary.diagnostics, 0);
        assert!(sink.diags.is_empty());

        let batch = |topic: &str| {
            sink.batches
                .iter()
                .find(|b| b.topic() == topic)
                .unwrap_or_else(|| panic!("no batch for topic {topic}"))
        };

        let gps0 = batch("sensor_gps[0]");
        assert_eq!(gps0.timestamps.values(), &[1_000, 1_500]);
        let lat = gps0.columns[0]
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        assert_eq!(lat.values(), &[473_000_000, 473_000_010]);
        let fix = gps0.columns[2]
            .as_any()
            .downcast_ref::<UInt8Array>()
            .unwrap();
        assert_eq!(fix.values(), &[3, 4]);
        // The timestamp drives the time axis, not a column.
        assert!(gps0.schema.field_by_name("timestamp").is_none());

        let gps1 = batch("sensor_gps[1]");
        assert_eq!(gps1.timestamps.values(), &[1_200]);
        let lon = gps1.columns[1]
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        assert_eq!(lon.values(), &[1_515_000_000]);

        let att = batch("vehicle_attitude[0]");
        assert_eq!(att.timestamps.values(), &[1_000, 2_000]);
        // float[4] q flattens to q[0]..q[3]; padding is skipped.
        for idx in 0..4 {
            assert!(att.schema.field_by_name(&format!("q[{idx}]")).is_some());
        }
        assert!(att.schema.field_by_name("_padding0").is_none());
        let q0 = att.columns[0]
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        assert_eq!(q0.values(), &[1.0, 0.9]);
        let q3 = att.columns[3]
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        assert_eq!(q3.values(), &[0.5, 0.6]);
    }

    #[test]
    fn parses_header_formats_subscription_data_and_flattens_nested_fields() {
        let (summary, sink) = parse(tiny_ulog());
        assert_eq!(summary.topic_count, 1);
        assert_eq!(summary.row_count, 1);
        assert_eq!(summary.diagnostics, 0);
        assert!(sink.diags.is_empty());

        let batch = &sink.batches[0];
        assert_eq!(batch.topic(), "nested[2]");
        assert_eq!(batch.timestamps.values(), &[1_000]);
        assert!(batch.schema.field_by_name("timestamp").is_none());
        assert!(batch.schema.field_by_name("_padding0").is_none());
        assert!(batch.schema.field_by_name("pos._padding0").is_none());
        assert!(batch.schema.field_by_name("pos.x").is_some());
        assert!(batch.schema.field_by_name("pos.y").is_some());
        assert!(batch.schema.field_by_name("pos.z").is_some());

        let x = batch.columns[0]
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        assert_eq!(x.values(), &[1.0]);
        let label = batch.columns[3]
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(label.value(0), "abc");
        let temp = batch.columns[4]
            .as_any()
            .downcast_ref::<Int16Array>()
            .unwrap();
        assert_eq!(temp.values(), &[-7]);
    }

    #[test]
    fn invalid_timestamps_are_skipped_without_poisoning_the_topic_range() {
        let (summary, sink) = parse(ulog_with_invalid_timestamp());
        assert_eq!(summary.row_count, 1);
        assert_eq!(summary.diagnostics, 1);
        assert!(
            sink.diags
                .iter()
                .any(|diag| diag.code == "ulog-invalid-timestamp")
        );

        let batch = &sink.batches[0];
        assert_eq!(batch.topic(), "test[0]");
        assert_eq!(batch.timestamps.values(), &[1_000_010]);
        let values = batch.columns[0]
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        assert_eq!(values.values(), &[2.0]);
    }

    #[test]
    fn dropouts_emit_diagnostics_and_insert_null_gap_rows() {
        let (summary, sink) = parse(ulog_with_dropout());
        assert_eq!(summary.row_count, 3);
        assert_eq!(summary.diagnostics, 1);
        assert!(
            sink.diags
                .iter()
                .any(|diag| diag.code == "ulog-dropout" && diag.time_us == Some(2_000))
        );

        let batch = &sink.batches[0];
        assert_eq!(batch.timestamps.values(), &[1_000, 2_000, 3_000]);
        let values = batch.columns[0]
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        assert_eq!(values.value(0), 1.0);
        assert!(values.is_null(1));
        assert_eq!(values.value(2), 2.0);
    }

    #[test]
    fn captures_params_and_logged_messages_as_source_metadata() {
        let (summary, sink) = parse(ulog_with_metadata());
        assert_eq!(summary.topic_count, 0);
        assert_eq!(summary.row_count, 0);
        assert_eq!(summary.diagnostics, 0);
        assert!(sink.diags.is_empty());

        assert_eq!(summary.source_meta.params.len(), 2);
        assert_eq!(summary.source_meta.params[0].name, "MPC_XY_CRUISE");
        assert_eq!(summary.source_meta.params[0].ty, "float");
        assert_eq!(summary.source_meta.params[0].value, "5.5");
        assert_eq!(summary.source_meta.params[1].name, "SYS_NAME");
        assert_eq!(summary.source_meta.params[1].value, "px4");

        assert_eq!(summary.source_meta.auto_markers.len(), 1);
        let marker = &summary.source_meta.auto_markers[0];
        assert_eq!(marker.time_us, 12_345);
        assert_eq!(marker.level, 6);
        assert_eq!(marker.text, "armed and ready");
    }
}
