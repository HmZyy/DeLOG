//! MCAP container parser.
//!
//! The container is decoded with the `mcap` crate; this module maps its
//! channels onto DeLOG topics. An MCAP file is a sequence of records framed by
//! the magic `\x89MCAP0\r\n`; schemas define each channel's JSON Schema, channels
//! pair a schema with a topic, and messages carry a channel id, a wall-clock
//! `log_time` and a JSON payload (usually compressed inside chunks).
//!
//! One DeLOG topic is emitted per scalar channel, named from the MCAP topic with
//! `/` replaced by `.` so the browser groups families. Fields are the schema's
//! top-level scalar properties, minus clock/descriptor metadata. `log_time`
//! (ns since the Unix epoch) becomes absolute µs, matching the tlog parser.
//!
//! Channels whose schema declares a base64 blob (embedded media) are skipped:
//! they carry no scalar signals.

mod json;

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanBuilder, Float64Builder, Int64Builder, StringBuilder};
use arrow::datatypes::DataType;
use delog_core::diagnostics::Diag;
use delog_core::identity::{SourceId, SourceMetadata};
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch};
use delog_core::parse_ctl::ParseCtl;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::time::{TimeRange, TimestampUs};
use mcap::{Channel, Message};

use crate::parser::{LogParser, ParseError, ReadSeek, Sniff};

use self::json::{FieldDtype, Scalar};

/// The 8-byte MCAP magic that opens and closes every file.
const MAGIC: &[u8; 8] = b"\x89MCAP0\r\n";

/// Rows per emitted batch, matching the other file parsers.
const BATCH_ROWS: usize = 8192;

/// First, then every Nth, occurrence of a repeating problem is diagnosed.
const DIAG_INTERVAL: u64 = 1024;

#[derive(Debug, Default)]
pub struct McapParser;

impl LogParser for McapParser {
    fn name(&self) -> &'static str {
        "mcap"
    }

    fn sniff(&self, head: &[u8]) -> Sniff {
        if head.starts_with(MAGIC) {
            Sniff::new(100, "MCAP magic")
        } else {
            Sniff::no()
        }
    }

    fn parse(
        &self,
        mut src: Box<dyn ReadSeek>,
        sink: &mut dyn IngestSink,
        ctl: &ParseCtl,
    ) -> Result<ParseSummary, ParseError> {
        // The `mcap` reader works over a byte slice, so the source is read whole.
        // The magic is checked first so a forced parse of a large non-MCAP file
        // fails without reading it all.
        let len = src.seek(SeekFrom::End(0)).map_err(ParseError::Io)?;
        src.seek(SeekFrom::Start(0)).map_err(ParseError::Io)?;
        let mut magic = [0u8; MAGIC.len()];
        if src.read_exact(&mut magic).is_err() || &magic != MAGIC {
            return Err(ParseError::Setup {
                detail: "not an MCAP file (bad magic)".into(),
            });
        }
        let mut buf = Vec::with_capacity(len as usize);
        buf.extend_from_slice(&magic);
        src.read_to_end(&mut buf).map_err(ParseError::Io)?;

        Decoder::new(sink, ctl).run(&buf)
    }
}

/// How a channel's messages are handled.
enum ChannelPlan {
    /// Scalar signals become one DeLOG topic.
    Numeric(Box<NumericTopic>),
    /// Structurally valid but nothing to emit (encoded, empty, or media).
    Skipped,
}

/// One channel's column builders and the field set extracted from each payload.
struct NumericTopic {
    schema: Arc<TopicSchema>,
    wanted: HashMap<String, usize>,
    out: Vec<Scalar>,
    cols: Vec<Col>,
    ts: Int64Builder,
    rows: usize,
}

#[derive(Clone, Copy)]
enum RowOutcome {
    Appended { drift: bool },
    BadJson,
}

impl NumericTopic {
    fn take_batch(&mut self, source: SourceId) -> ParsedBatch {
        let timestamps = self.ts.finish();
        let columns: Vec<ArrayRef> = self.cols.iter_mut().map(Col::finish).collect();
        self.rows = 0;
        ParsedBatch::new(source, Arc::clone(&self.schema), timestamps, columns)
    }

    /// Decode one payload and append a row. Returns [`RowOutcome::BadJson`]
    /// (without touching the builders) when the payload is not a JSON object.
    fn append(&mut self, ts_us: TimestampUs, data: &[u8]) -> RowOutcome {
        if json::read_fields(data, &self.wanted, &mut self.out).is_err() {
            return RowOutcome::BadJson;
        }
        self.ts.append_value(ts_us);
        let mut drift = false;
        for (col, scalar) in self.cols.iter_mut().zip(&self.out) {
            if matches!(col.append(scalar), Appended::Drift) {
                drift = true;
            }
        }
        self.rows += 1;
        RowOutcome::Appended { drift }
    }
}

/// One channel's column builders and the field set extracted from each payload.
struct Decoder<'a> {
    sink: &'a mut dyn IngestSink,
    ctl: &'a ParseCtl,
    source: SourceId,
    plans: HashMap<u16, ChannelPlan>,
    topic_names: HashSet<String>,
    emitted_topics: HashSet<u16>,
    unknown_channels: HashSet<u16>,
    row_count: u64,
    time_range: Option<TimeRange>,
    diagnostics: u64,
    stream_errors: u64,
    bad_json: u64,
    drift: u64,
}

impl<'a> Decoder<'a> {
    fn new(sink: &'a mut dyn IngestSink, ctl: &'a ParseCtl) -> Self {
        Self {
            sink,
            ctl,
            source: ctl.source(),
            plans: HashMap::new(),
            topic_names: HashSet::new(),
            emitted_topics: HashSet::new(),
            unknown_channels: HashSet::new(),
            row_count: 0,
            time_range: None,
            diagnostics: 0,
            stream_errors: 0,
            bad_json: 0,
            drift: 0,
        }
    }

    fn run(mut self, buf: &[u8]) -> Result<ParseSummary, ParseError> {
        let total = mcap::Summary::read(buf)
            .ok()
            .flatten()
            .and_then(|summary| summary.stats)
            .map(|stats| stats.message_count)
            .filter(|count| *count > 0);

        let stream = mcap::MessageStream::new(buf).map_err(|error| ParseError::Setup {
            detail: error.to_string(),
        })?;

        let mut index = 0u64;
        for message in stream {
            if self.ctl.cancelled_at(index) {
                self.finish();
                let summary = self.summary();
                self.sink.close_source(self.source, summary);
                return Err(ParseError::Cancelled);
            }
            match message {
                Ok(message) => self.ingest(&message),
                Err(error) => self.note_stream_error(&error),
            }
            index += 1;
            if let Some(total) = total {
                self.ctl
                    .report_fraction(self.sink, index as f32 / total as f32);
            }
        }

        self.finish();
        let summary = self.summary();
        self.sink.close_source(self.source, summary.clone());
        Ok(summary)
    }

    fn ingest(&mut self, message: &Message<'_>) {
        let channel_id = message.channel.id;
        if !self.plans.contains_key(&channel_id) {
            let plan = self.build_plan(&message.channel);
            self.plans.insert(channel_id, plan);
        }
        // ns since the Unix epoch -> canonical µs. `u64::MAX / 1000` still fits
        // an i64, so the cast cannot wrap.
        let ts_us = (message.log_time / 1000) as i64;
        let data = message.data.as_ref();

        enum Kind {
            Numeric,
            Skipped,
            Unknown,
        }
        let kind = match self.plans.get(&channel_id) {
            Some(ChannelPlan::Numeric(_)) => Kind::Numeric,
            Some(ChannelPlan::Skipped) => Kind::Skipped,
            None => Kind::Unknown,
        };
        match kind {
            Kind::Numeric => self.ingest_numeric(channel_id, ts_us, data),
            Kind::Skipped => {}
            Kind::Unknown => self.note_unknown_channel(channel_id),
        }
    }

    fn build_plan(&mut self, channel: &Channel<'_>) -> ChannelPlan {
        let topic = channel.topic.clone();
        if channel.message_encoding != "json" {
            self.diag(Diag::info(
                "mcap-unsupported-encoding",
                format!(
                    "channel `{topic}` uses message encoding `{}`; skipped",
                    channel.message_encoding
                ),
            ));
            return ChannelPlan::Skipped;
        }
        let Some(schema) = channel.schema.as_ref() else {
            self.diag(Diag::warning(
                "mcap-missing-schema",
                format!("channel `{topic}` has no schema; skipped"),
            ));
            return ChannelPlan::Skipped;
        };
        if schema.encoding != "jsonschema" {
            self.diag(Diag::info(
                "mcap-unsupported-schema",
                format!(
                    "channel `{topic}` uses schema encoding `{}`; skipped",
                    schema.encoding
                ),
            ));
            return ChannelPlan::Skipped;
        }

        let plan = match json::plan_schema(&schema.data) {
            Ok(plan) => plan,
            Err(error) => {
                self.diag(Diag::warning(
                    "mcap-bad-schema",
                    format!("channel `{topic}`: {error}; skipped"),
                ));
                return ChannelPlan::Skipped;
            }
        };

        if plan.is_media {
            self.diag(Diag::info(
                "mcap-media-skipped",
                format!("channel `{topic}` carries media (base64); skipped"),
            ));
            return ChannelPlan::Skipped;
        }

        if plan.fields.is_empty() {
            self.diag(Diag::info(
                "mcap-no-fields",
                format!("channel `{topic}` exposes no scalar fields; skipped"),
            ));
            return ChannelPlan::Skipped;
        }

        let name = self.unique_name(&sanitize_topic(&topic));
        let mut field_schemas = Vec::with_capacity(plan.fields.len());
        for spec in &plan.fields {
            let schema =
                FieldSchema::new(spec.key.clone(), dtype_of(spec.dtype), None::<String>, 1.0)
                    .expect("scalar dtypes and non-empty names are always valid");
            field_schemas.push(schema);
        }
        let schema = match TopicSchema::new(name, field_schemas) {
            Ok(schema) => Arc::new(schema),
            Err(error) => {
                self.diag(Diag::warning(
                    "mcap-bad-topic",
                    format!("channel `{topic}`: {error}; skipped"),
                ));
                return ChannelPlan::Skipped;
            }
        };

        let wanted: HashMap<String, usize> = plan
            .fields
            .iter()
            .enumerate()
            .map(|(index, spec)| (spec.key.clone(), index))
            .collect();
        let cols = plan
            .fields
            .iter()
            .map(|spec| Col::for_dtype(spec.dtype))
            .collect();
        let out = vec![Scalar::Missing; plan.fields.len()];
        ChannelPlan::Numeric(Box::new(NumericTopic {
            schema,
            wanted,
            out,
            cols,
            ts: Int64Builder::new(),
            rows: 0,
        }))
    }

    fn ingest_numeric(&mut self, channel_id: u16, ts_us: TimestampUs, data: &[u8]) {
        let source = self.source;
        let mut outcome = None;
        let mut full = None;
        if let Some(ChannelPlan::Numeric(topic)) = self.plans.get_mut(&channel_id) {
            match topic.append(ts_us, data) {
                RowOutcome::BadJson => outcome = Some(RowOutcome::BadJson),
                RowOutcome::Appended { drift } => {
                    outcome = Some(RowOutcome::Appended { drift });
                    if topic.rows >= BATCH_ROWS {
                        full = Some(topic.take_batch(source));
                    }
                }
            }
        }

        match outcome {
            Some(RowOutcome::BadJson) => self.note_bad_json(),
            Some(RowOutcome::Appended { drift }) => {
                self.emitted_topics.insert(channel_id);
                self.row_count += 1;
                self.time_range = Some(match self.time_range {
                    Some(range) => range.include(ts_us),
                    None => TimeRange::point(ts_us),
                });
                if drift {
                    self.note_drift();
                }
            }
            None => {}
        }
        if let Some(batch) = full {
            self.sink.submit(batch);
        }
    }

    fn note_stream_error(&mut self, error: &mcap::McapError) {
        self.stream_errors += 1;
        if self.stream_errors == 1 || self.stream_errors.is_multiple_of(DIAG_INTERVAL) {
            self.diag(Diag::warning(
                "mcap-stream",
                format!("skipped {} bad message(s): {error}", self.stream_errors),
            ));
        }
    }

    fn note_unknown_channel(&mut self, channel_id: u16) {
        if self.unknown_channels.insert(channel_id) {
            self.diag(Diag::warning(
                "mcap-unknown-channel",
                format!("message on unknown channel {channel_id}; skipped"),
            ));
        }
    }

    fn note_bad_json(&mut self) {
        self.bad_json += 1;
        if self.bad_json == 1 || self.bad_json.is_multiple_of(DIAG_INTERVAL) {
            self.diag(Diag::warning(
                "mcap-bad-payload",
                format!(
                    "skipped {} payload(s) that were not JSON objects",
                    self.bad_json
                ),
            ));
        }
    }

    fn note_drift(&mut self) {
        self.drift += 1;
        if self.drift == 1 || self.drift.is_multiple_of(DIAG_INTERVAL) {
            self.diag(Diag::warning(
                "mcap-type-drift",
                format!(
                    "{} field value(s) did not match the channel schema",
                    self.drift
                ),
            ));
        }
    }

    /// Flush partial batches.
    fn finish(&mut self) {
        let source = self.source;
        let ids: Vec<u16> = self
            .plans
            .iter()
            .filter_map(|(&id, plan)| match plan {
                ChannelPlan::Numeric(topic) if topic.rows > 0 => Some(id),
                _ => None,
            })
            .collect();
        for id in ids {
            if let Some(ChannelPlan::Numeric(topic)) = self.plans.get_mut(&id) {
                let batch = topic.take_batch(source);
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
            topic_count: self.emitted_topics.len() as u64,
            row_count: self.row_count,
            time_range: self.time_range,
            diagnostics: self.diagnostics,
            source_meta: SourceMetadata::default(),
        }
    }

    /// Names are unique per source; a collision (e.g. `A/B` and `A.B` both
    /// sanitizing to `A.B`) gets a `#N` suffix so batches cannot merge.
    fn unique_name(&mut self, base: &str) -> String {
        if self.topic_names.insert(base.to_owned()) {
            return base.to_owned();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base}#{n}");
            if self.topic_names.insert(candidate.clone()) {
                return candidate;
            }
            n += 1;
        }
    }
}

/// A typed Arrow column builder mirroring the schema field order.
enum Col {
    Int(Int64Builder),
    Float(Float64Builder),
    Bool(BooleanBuilder),
    Str(StringBuilder),
}

enum Appended {
    Value,
    Null,
    /// The payload's JSON type did not match the channel's declared type.
    Drift,
}

impl Col {
    fn for_dtype(dtype: FieldDtype) -> Self {
        match dtype {
            FieldDtype::Int => Self::Int(Int64Builder::new()),
            FieldDtype::Float => Self::Float(Float64Builder::new()),
            FieldDtype::Bool => Self::Bool(BooleanBuilder::new()),
            FieldDtype::Str => Self::Str(StringBuilder::new()),
        }
    }

    /// Append one value, coercing int/float where lossless. A type mismatch
    /// appends a null and reports drift rather than dropping the whole row.
    fn append(&mut self, scalar: &Scalar) -> Appended {
        match self {
            Self::Int(builder) => match scalar {
                Scalar::Int(value) => {
                    builder.append_value(*value);
                    Appended::Value
                }
                Scalar::Float(value)
                    if value.is_finite()
                        && value.fract() == 0.0
                        && *value >= i64::MIN as f64
                        && *value <= i64::MAX as f64 =>
                {
                    builder.append_value(*value as i64);
                    Appended::Value
                }
                Scalar::Missing => {
                    builder.append_null();
                    Appended::Null
                }
                _ => {
                    builder.append_null();
                    Appended::Drift
                }
            },
            Self::Float(builder) => match scalar {
                Scalar::Int(value) => {
                    builder.append_value(*value as f64);
                    Appended::Value
                }
                Scalar::Float(value) => {
                    builder.append_value(*value);
                    Appended::Value
                }
                Scalar::Missing => {
                    builder.append_null();
                    Appended::Null
                }
                _ => {
                    builder.append_null();
                    Appended::Drift
                }
            },
            Self::Bool(builder) => match scalar {
                Scalar::Bool(value) => {
                    builder.append_value(*value);
                    Appended::Value
                }
                Scalar::Missing => {
                    builder.append_null();
                    Appended::Null
                }
                _ => {
                    builder.append_null();
                    Appended::Drift
                }
            },
            Self::Str(builder) => match scalar {
                Scalar::Str(value) => {
                    builder.append_value(value);
                    Appended::Value
                }
                Scalar::Missing => {
                    builder.append_null();
                    Appended::Null
                }
                _ => {
                    builder.append_null();
                    Appended::Drift
                }
            },
        }
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            Self::Int(builder) => Arc::new(builder.finish()),
            Self::Float(builder) => Arc::new(builder.finish()),
            Self::Bool(builder) => Arc::new(builder.finish()),
            Self::Str(builder) => Arc::new(builder.finish()),
        }
    }
}

fn dtype_of(dtype: FieldDtype) -> DataType {
    match dtype {
        FieldDtype::Int => DataType::Int64,
        FieldDtype::Float => DataType::Float64,
        FieldDtype::Bool => DataType::Boolean,
        FieldDtype::Str => DataType::Utf8,
    }
}

fn sanitize_topic(raw: &str) -> String {
    let trimmed = raw.strip_prefix('/').unwrap_or(raw);
    let replaced = trimmed.replace('/', ".");
    if replaced.is_empty() {
        "mcap".to_owned()
    } else {
        replaced
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Cursor;

    use arrow::array::{Array, Int64Array, StringArray};
    use delog_core::ingest::SourceKind;
    use delog_core::parse_ctl::CancelToken;
    use mcap::records::MessageHeader;

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

    fn build(build: impl FnOnce(&mut mcap::Writer<Cursor<Vec<u8>>>)) -> Vec<u8> {
        let mut writer = mcap::Writer::new(Cursor::new(Vec::new())).expect("writer");
        build(&mut writer);
        writer.finish().expect("finish");
        writer.into_inner().into_inner()
    }

    fn write_message(
        writer: &mut mcap::Writer<Cursor<Vec<u8>>>,
        channel: u16,
        sequence: u32,
        log_time: u64,
        payload: &str,
    ) {
        writer
            .write_to_known_channel(
                &MessageHeader {
                    channel_id: channel,
                    sequence,
                    log_time,
                    publish_time: log_time,
                },
                payload.as_bytes(),
            )
            .expect("write");
    }

    fn parse(bytes: Vec<u8>) -> (ParseSummary, Collect) {
        let mut sink = Collect::default();
        let ctl = ParseCtl::new(CancelToken::new(), SourceId(0), bytes.len() as u64);
        let summary = McapParser
            .parse(Box::new(Cursor::new(bytes)), &mut sink, &ctl)
            .expect("mcap parse");
        (summary, sink)
    }

    fn ns(us: i64) -> u64 {
        (us as u64) * 1000
    }

    #[test]
    fn magic_sniff_is_confident() {
        assert_eq!(McapParser.sniff(MAGIC).score, 100);
        assert_eq!(McapParser.sniff(b"ULog\x01\x12").score, 0);
    }

    #[test]
    fn a_non_mcap_file_is_a_setup_error() {
        let mut sink = Collect::default();
        let ctl = ParseCtl::new(CancelToken::new(), SourceId(0), 4);
        let result = McapParser.parse(Box::new(Cursor::new(b"nope".to_vec())), &mut sink, &ctl);
        assert!(matches!(result, Err(ParseError::Setup { .. })));
    }
}
