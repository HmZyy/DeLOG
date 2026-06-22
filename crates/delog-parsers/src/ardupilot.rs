//! ArduPilot DataFlash `.BIN` parser.
//!
//! DataFlash is self-describing: every record is `0xA3 0x95 <msgid> <payload>`.
//! `FMT` records (msgid 128) define each message's name, length and field
//! layout; `FMTU`/`UNIT`/`MULT` attach units and multipliers. We store **raw**
//! values and record the unit + multiplier in the [`TopicSchema`] —
//! the One Copy applies the multiplier later. Malformed records are skipped with
//! a byte-offset diagnostic and parsing resynchronises on the next sync pair;
//! only an unreadable stream aborts.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::sync::Arc;

use arrow::array::{
    ArrayRef, Float32Builder, Float64Builder, Int8Builder, Int16Builder, Int32Builder,
    Int64Builder, StringBuilder, UInt8Builder, UInt16Builder, UInt32Builder, UInt64Builder,
};
use arrow::datatypes::DataType;
use delog_core::diagnostics::Diag;
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch};
use delog_core::parse_ctl::ParseCtl;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::time::TimeRange;

use crate::parser::{LogParser, ParseError, ReadSeek, Sniff};

const HEAD1: u8 = 0xA3;
const HEAD2: u8 = 0x95;
const FMT_MSGID: u8 = 0x80;
/// FMT payload: Type(1) Length(1) Name(4) Format(16) Columns(64).
const FMT_PAYLOAD_LEN: usize = 1 + 1 + 4 + 16 + 64;
/// Rows the parser buffers per topic before submitting a batch.
const BATCH_ROWS: usize = 8192;

/// The registered ArduPilot DataFlash parser.
#[derive(Debug, Default)]
pub struct ArduPilotParser;

impl LogParser for ArduPilotParser {
    fn name(&self) -> &'static str {
        "ardupilot-bin"
    }

    fn sniff(&self, head: &[u8]) -> Sniff {
        match head {
            // Sync pair + FMT msgid + FMT-defining-FMT (type 128) — unmistakable.
            [HEAD1, HEAD2, FMT_MSGID, FMT_MSGID, ..] => Sniff::new(99, "DataFlash FMT header"),
            [HEAD1, HEAD2, ..] => Sniff::new(90, "DataFlash sync pair"),
            _ => Sniff::no(),
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

/// One field within a registered message format, with its payload byte offset.
#[derive(Debug, Clone)]
struct RawField {
    name: String,
    offset: usize,
    chr: u8,
}

/// A message layout decoded from an `FMT` record.
#[derive(Debug, Clone)]
struct MsgFormat {
    name: String,
    payload_len: usize,
    fields: Vec<RawField>,
    /// False when the field sizes did not sum to `payload_len`; we can still
    /// skip records of this type but not decode them.
    decodable: bool,
}

impl MsgFormat {
    fn field(&self, name: &str) -> Option<&RawField> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// One emitted column: where to read it and as what.
#[derive(Debug, Clone)]
struct Emit {
    offset: usize,
    chr: u8,
}

/// Where the timestamp lives in a record's payload.
#[derive(Debug, Clone, Copy)]
enum TimeSource {
    /// `TimeUS` (u64 microseconds).
    Micros(usize),
    /// `TimeMS` (u32 milliseconds) — older logs.
    Millis(usize),
}

/// Precomputed decode plan for one msgid (shared across its instances).
#[derive(Debug, Clone)]
struct Plan {
    base_name: String,
    time: TimeSource,
    instance: Option<(usize, u8)>,
    emits: Vec<Emit>,
    schema: Arc<TopicSchema>,
}

/// Accumulates rows for one topic (one msgid, one instance).
struct TopicAccum {
    schema: Arc<TopicSchema>,
    ts: Int64Builder,
    cols: Vec<ColBuilder>,
    rows: usize,
}

struct Decoder<'a> {
    sink: &'a mut dyn IngestSink,
    ctl: &'a ParseCtl,
    source: SourceId,
    formats: HashMap<u8, MsgFormat>,
    plans: HashMap<u8, Option<Plan>>,
    units: HashMap<u8, String>,
    mults: HashMap<u8, f64>,
    fmtu: HashMap<u8, (Vec<u8>, Vec<u8>)>,
    topics: HashMap<String, TopicAccum>,
    row_count: u64,
    diagnostics: u64,
    time_range: Option<TimeRange>,
}

impl<'a> Decoder<'a> {
    fn new(sink: &'a mut dyn IngestSink, ctl: &'a ParseCtl) -> Self {
        let source = ctl.source();
        Self {
            sink,
            ctl,
            source,
            formats: HashMap::new(),
            plans: HashMap::new(),
            units: HashMap::new(),
            mults: HashMap::new(),
            fmtu: HashMap::new(),
            topics: HashMap::new(),
            row_count: 0,
            diagnostics: 0,
            time_range: None,
        }
    }

    fn run(mut self, src: Box<dyn ReadSeek>) -> Result<ParseSummary, ParseError> {
        let mut reader = FrameReader::new(Box::new(BufReader::new(src)));
        let mut record_index: u64 = 0;

        loop {
            match reader.next_sync()? {
                SyncResult::Eof => break,
                SyncResult::Skipped(bytes) => {
                    self.diagnostic(
                        Diag::warning(
                            "bin-resync",
                            format!("skipped {bytes} byte(s) of corruption before resync"),
                        )
                        .at_byte(reader.offset()),
                    );
                }
                SyncResult::Found => {}
            }

            let Some(msgid) = reader.read_u8()? else {
                break;
            };
            let record_offset = reader.offset();

            if msgid == FMT_MSGID {
                if !self.read_fmt(&mut reader)? {
                    break;
                }
            } else if !self.read_message(&mut reader, msgid, record_offset)? {
                break;
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

    /// Read and register an `FMT` record (fixed layout).
    fn read_fmt(&mut self, reader: &mut FrameReader) -> Result<bool, ParseError> {
        let Some(payload) = reader.read_payload(FMT_PAYLOAD_LEN)? else {
            return Ok(false);
        };
        let length = payload[1] as usize;
        let name = c_str(&payload[2..6]);
        let format = &payload[6..22];
        let columns = c_str(&payload[22..86]);

        let payload_len = length.saturating_sub(3);
        let labels: Vec<&str> = columns.split(',').filter(|s| !s.is_empty()).collect();

        let mut fields = Vec::new();
        let mut offset = 0usize;
        let mut decodable = true;
        for (i, &chr) in format.iter().take_while(|&&c| c != 0).enumerate() {
            let Some(size) = type_size(chr) else {
                decodable = false;
                break;
            };
            let field_name = labels.get(i).copied().unwrap_or("?").to_owned();
            fields.push(RawField {
                name: field_name,
                offset,
                chr,
            });
            offset += size;
        }
        if offset != payload_len {
            decodable = false;
        }

        // The FMT record's own `Type` field is the msgid it describes.
        let described = payload[0];
        self.formats.insert(
            described,
            MsgFormat {
                name,
                payload_len,
                fields,
                decodable,
            },
        );
        Ok(true)
    }

    /// Read a non-FMT record: metadata (`FMTU`/`UNIT`/`MULT`) updates tables;
    /// everything else becomes topic rows. Returns `false` on EOF.
    fn read_message(
        &mut self,
        reader: &mut FrameReader,
        msgid: u8,
        record_offset: u64,
    ) -> Result<bool, ParseError> {
        let Some(format) = self.formats.get(&msgid) else {
            // Unknown msgid: we cannot know its length, so resync.
            self.diagnostic(
                Diag::warning("bin-unknown-msgid", format!("unknown message id {msgid}"))
                    .at_byte(record_offset),
            );
            return Ok(true);
        };
        let payload_len = format.payload_len;
        let name = format.name.clone();

        let Some(payload) = reader.read_payload(payload_len)? else {
            return Ok(false);
        };

        match name.as_str() {
            "FMTU" => self.decode_fmtu(msgid, &payload),
            "UNIT" => self.read_unit(msgid, &payload),
            "MULT" => self.read_mult(msgid, &payload),
            _ => self.decode_data(msgid, &payload),
        }
        Ok(true)
    }

    fn read_unit(&mut self, msgid: u8, payload: &[u8]) {
        let fmt = &self.formats[&msgid];
        if let (Some(id), Some(label)) = (fmt.field("Id"), fmt.field("Label")) {
            let id_char = payload[id.offset];
            let label = c_str(&payload[label.offset..label.offset + 64]);
            self.units.insert(id_char, label);
        }
    }

    fn read_mult(&mut self, msgid: u8, payload: &[u8]) {
        let fmt = &self.formats[&msgid];
        if let (Some(id), Some(mult)) = (fmt.field("Id"), fmt.field("Mult")) {
            let id_char = payload[id.offset];
            let value = f64::from_le_bytes(read_8(payload, mult.offset));
            self.mults.insert(id_char, value);
        }
    }

    /// Build (once) and apply a decode plan, routing rows to the right topic.
    fn decode_data(&mut self, msgid: u8, payload: &[u8]) {
        if !self.plans.contains_key(&msgid) {
            let plan = self.build_plan(msgid);
            self.plans.insert(msgid, plan);
        }
        let Some(plan) = self.plans.get(&msgid).and_then(Option::as_ref).cloned() else {
            return;
        };

        let Some(time_us) = read_time(&plan.time, payload) else {
            return;
        };

        let topic_name = match plan.instance {
            Some((off, chr)) => {
                let inst = read_instance(chr, payload, off);
                format!("{}[{inst}]", plan.base_name)
            }
            None => plan.base_name.clone(),
        };

        // Each instance topic gets its own schema *named* `MOT[N]` so the topic
        // name is the schema name (the one-name invariant); the base
        // schema is reused unchanged for non-instance topics.
        let accum = if let Some(accum) = self.topics.get_mut(&topic_name) {
            accum
        } else {
            let schema = if topic_name == plan.base_name {
                Arc::clone(&plan.schema)
            } else {
                Arc::new(
                    TopicSchema::new(&topic_name, plan.schema.fields().to_vec())
                        .expect("renamed instance schema is valid"),
                )
            };
            let accum = TopicAccum {
                schema,
                ts: Int64Builder::new(),
                cols: plan
                    .emits
                    .iter()
                    .map(|e| ColBuilder::for_chr(e.chr))
                    .collect(),
                rows: 0,
            };
            self.topics.entry(topic_name).or_insert(accum)
        };

        accum.ts.append_value(time_us);
        for (col, emit) in accum.cols.iter_mut().zip(&plan.emits) {
            col.append(emit.chr, payload, emit.offset);
        }
        accum.rows += 1;
        self.row_count += 1;
        self.time_range = Some(match self.time_range {
            Some(r) => r.include(time_us),
            None => TimeRange::point(time_us),
        });

        if accum.rows >= BATCH_ROWS {
            let batch = accum.take_batch(self.source);
            self.sink.submit(batch);
        }
    }

    fn build_plan(&mut self, msgid: u8) -> Option<Plan> {
        let format = self.formats.get(&msgid)?;
        if !format.decodable {
            self.diagnostic(Diag::warning(
                "bin-undecodable-format",
                format!(
                    "message `{}` has an inconsistent format; skipped",
                    format.name
                ),
            ));
            return None;
        }

        // Locate the timestamp.
        let time = if let Some(f) = format.field("TimeUS") {
            TimeSource::Micros(f.offset)
        } else if let Some(f) = format.field("TimeMS") {
            TimeSource::Millis(f.offset)
        } else {
            self.diagnostic(Diag::info(
                "bin-no-timestamp",
                format!(
                    "message `{}` has no TimeUS/TimeMS; not plotted",
                    format.name
                ),
            ));
            return None;
        };

        // Optional instance discriminator.
        let instance = format
            .fields
            .iter()
            .find(|f| (f.name == "I" || f.name == "Instance") && is_integer_chr(f.chr))
            .map(|f| (f.offset, f.chr));
        let instance_name = instance.map(|_| ()).and(
            format
                .fields
                .iter()
                .find(|f| f.name == "I" || f.name == "Instance")
                .map(|f| f.name.clone()),
        );

        let mut emits = Vec::new();
        let mut field_schemas = Vec::new();
        for (idx, field) in format.fields.iter().enumerate() {
            if field.name == "TimeUS" || field.name == "TimeMS" {
                continue;
            }
            if instance_name.as_deref() == Some(field.name.as_str()) {
                continue;
            }
            let Some(dtype) = scalar_dtype(field.chr) else {
                continue; // arrays ('a') and unknowns are not emitted
            };
            let (mult, unit) = self.resolve_unit_mult(msgid, idx, field.chr);
            match FieldSchema::new(&field.name, dtype, unit, mult) {
                Ok(fs) => {
                    field_schemas.push(fs);
                    emits.push(Emit {
                        offset: field.offset,
                        chr: field.chr,
                    });
                }
                Err(_) => continue,
            }
        }

        let base_name = format.name.clone();
        let schema = TopicSchema::new(&base_name, field_schemas).ok()?;
        Some(Plan {
            base_name,
            time,
            instance,
            emits,
            schema: Arc::new(schema),
        })
    }

    /// Resolve a field's multiplier and unit. Pre-scaled type chars
    /// (`c/C/e/E/L`) carry their own scale; other fields take the FMTU/MULT
    /// multiplier when one is defined.
    fn resolve_unit_mult(&self, msgid: u8, field_index: usize, chr: u8) -> (f64, Option<String>) {
        let (default_mult, default_unit) = default_mult_unit(chr);

        let fmtu = self.fmtu.get(&msgid);
        let unit = fmtu
            .and_then(|(unit_ids, _)| unit_ids.get(field_index).copied())
            .filter(|&c| c != b'-' && c != 0)
            .and_then(|c| self.units.get(&c).cloned())
            .or_else(|| default_unit.map(str::to_owned));

        let mult = if matches!(chr, b'c' | b'C' | b'e' | b'E' | b'L') {
            default_mult
        } else {
            fmtu.and_then(|(_, mult_ids)| mult_ids.get(field_index).copied())
                .filter(|&c| c != b'-' && c != 0)
                .and_then(|c| self.mults.get(&c).copied())
                .filter(|m| m.is_finite() && *m != 0.0)
                .unwrap_or(default_mult)
        };

        (mult, unit)
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
            ..ParseSummary::default()
        }
    }
}

impl TopicAccum {
    fn take_batch(&mut self, source: SourceId) -> ParsedBatch {
        let timestamps = self.ts.finish();
        let columns: Vec<ArrayRef> = self.cols.iter_mut().map(ColBuilder::finish).collect();
        self.rows = 0;
        ParsedBatch::new(source, Arc::clone(&self.schema), timestamps, columns)
    }
}

impl Decoder<'_> {
    fn decode_fmtu(&mut self, msgid: u8, payload: &[u8]) {
        let fmt = &self.formats[&msgid];
        let (Some(target), Some(units), Some(mults)) = (
            fmt.field("FmtType"),
            fmt.field("UnitIds"),
            fmt.field("MultIds"),
        ) else {
            return;
        };
        let target_id = payload[target.offset];
        let unit_ids = trimmed_bytes(&payload[units.offset..units.offset + 16]);
        let mult_ids = trimmed_bytes(&payload[mults.offset..mults.offset + 16]);
        self.fmtu.insert(target_id, (unit_ids, mult_ids));
    }
}

/// Column builder for one emitted field; the variant matches the type char.
enum ColBuilder {
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
    fn for_chr(chr: u8) -> Self {
        match chr {
            b'b' => Self::I8(Int8Builder::new()),
            b'h' | b'c' => Self::I16(Int16Builder::new()),
            b'i' | b'e' | b'L' => Self::I32(Int32Builder::new()),
            b'q' => Self::I64(Int64Builder::new()),
            b'B' | b'M' => Self::U8(UInt8Builder::new()),
            b'H' | b'C' => Self::U16(UInt16Builder::new()),
            b'I' | b'E' => Self::U32(UInt32Builder::new()),
            b'Q' => Self::U64(UInt64Builder::new()),
            b'f' => Self::F32(Float32Builder::new()),
            b'd' => Self::F64(Float64Builder::new()),
            b'n' | b'N' | b'Z' => Self::Str(StringBuilder::new()),
            other => unreachable!("for_chr on unsupported char {other}"),
        }
    }

    fn append(&mut self, chr: u8, p: &[u8], off: usize) {
        match self {
            Self::I8(b) => b.append_value(p[off] as i8),
            Self::I16(b) => b.append_value(i16::from_le_bytes(read_2(p, off))),
            Self::I32(b) => b.append_value(i32::from_le_bytes(read_4(p, off))),
            Self::I64(b) => b.append_value(i64::from_le_bytes(read_8(p, off))),
            Self::U8(b) => b.append_value(p[off]),
            Self::U16(b) => b.append_value(u16::from_le_bytes(read_2(p, off))),
            Self::U32(b) => b.append_value(u32::from_le_bytes(read_4(p, off))),
            Self::U64(b) => b.append_value(u64::from_le_bytes(read_8(p, off))),
            Self::F32(b) => b.append_value(f32::from_le_bytes(read_4(p, off))),
            Self::F64(b) => b.append_value(f64::from_le_bytes(read_8(p, off))),
            Self::Str(b) => {
                let len = str_len(chr);
                b.append_value(c_str(&p[off..off + len]));
            }
        }
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
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

/// Streaming framed reader with resync and a one-byte pushback.
struct FrameReader {
    inner: Box<dyn Read>,
    offset: u64,
    pending: Option<u8>,
}

enum SyncResult {
    Found,
    Skipped(u64),
    Eof,
}

impl FrameReader {
    fn new(inner: Box<dyn Read>) -> Self {
        Self {
            inner,
            offset: 0,
            pending: None,
        }
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn read_u8(&mut self) -> Result<Option<u8>, ParseError> {
        if let Some(b) = self.pending.take() {
            self.offset += 1;
            return Ok(Some(b));
        }
        let mut buf = [0u8; 1];
        match self.inner.read(&mut buf) {
            Ok(0) => Ok(None),
            Ok(_) => {
                self.offset += 1;
                Ok(Some(buf[0]))
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => self.read_u8(),
            Err(e) => Err(ParseError::Io(e)),
        }
    }

    /// Scan to the next `HEAD1 HEAD2` pair.
    fn next_sync(&mut self) -> Result<SyncResult, ParseError> {
        let mut skipped = 0u64;
        loop {
            let Some(b) = self.read_u8()? else {
                return Ok(if skipped == 0 {
                    SyncResult::Eof
                } else {
                    SyncResult::Skipped(skipped)
                });
            };
            if b != HEAD1 {
                skipped += 1;
                continue;
            }
            match self.read_u8()? {
                Some(HEAD2) => {
                    return Ok(if skipped == 0 {
                        SyncResult::Found
                    } else {
                        SyncResult::Skipped(skipped)
                    });
                }
                Some(other) => {
                    // The first byte was a false HEAD1; reconsider `other`.
                    skipped += 1;
                    self.pending = Some(other);
                    self.offset -= 1;
                }
                None => {
                    return Ok(SyncResult::Skipped(skipped + 1));
                }
            }
        }
    }

    /// Read exactly `len` payload bytes, or `None` on a truncated tail.
    fn read_payload(&mut self, len: usize) -> Result<Option<Vec<u8>>, ParseError> {
        let mut buf = vec![0u8; len];
        let mut filled = 0;
        if let Some(b) = self.pending.take() {
            buf[0] = b;
            filled = 1;
            self.offset += 1;
        }
        while filled < len {
            match self.inner.read(&mut buf[filled..]) {
                Ok(0) => return Ok(None),
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
}

fn read_time(time: &TimeSource, payload: &[u8]) -> Option<i64> {
    match *time {
        TimeSource::Micros(off) => {
            (off + 8 <= payload.len()).then(|| u64::from_le_bytes(read_8(payload, off)) as i64)
        }
        TimeSource::Millis(off) => (off + 4 <= payload.len())
            .then(|| u32::from_le_bytes(read_4(payload, off)) as i64 * 1000),
    }
}

fn read_instance(chr: u8, payload: &[u8], off: usize) -> i64 {
    match chr {
        b'b' => payload[off] as i8 as i64,
        b'B' | b'M' => payload[off] as i64,
        b'h' => i16::from_le_bytes(read_2(payload, off)) as i64,
        b'H' => u16::from_le_bytes(read_2(payload, off)) as i64,
        b'i' => i32::from_le_bytes(read_4(payload, off)) as i64,
        b'I' => u32::from_le_bytes(read_4(payload, off)) as i64,
        b'q' => i64::from_le_bytes(read_8(payload, off)),
        b'Q' => u64::from_le_bytes(read_8(payload, off)) as i64,
        _ => 0,
    }
}

/// Byte width of a DataFlash format character, or `None` if unknown.
fn type_size(chr: u8) -> Option<usize> {
    Some(match chr {
        b'b' | b'B' | b'M' => 1,
        b'h' | b'H' | b'c' | b'C' => 2,
        b'i' | b'I' | b'f' | b'e' | b'E' | b'L' | b'n' => 4,
        b'd' | b'q' | b'Q' => 8,
        b'N' => 16,
        b'Z' => 64,
        b'a' => 64, // int16[32] array — consumed but not emitted
        _ => return None,
    })
}

/// Arrow dtype for an emitted scalar/text field, or `None` for arrays/unknowns.
fn scalar_dtype(chr: u8) -> Option<DataType> {
    Some(match chr {
        b'b' => DataType::Int8,
        b'B' | b'M' => DataType::UInt8,
        b'h' | b'c' => DataType::Int16,
        b'H' | b'C' => DataType::UInt16,
        b'i' | b'e' | b'L' => DataType::Int32,
        b'I' | b'E' => DataType::UInt32,
        b'q' => DataType::Int64,
        b'Q' => DataType::UInt64,
        b'f' => DataType::Float32,
        b'd' => DataType::Float64,
        b'n' | b'N' | b'Z' => DataType::Utf8,
        _ => return None, // 'a' arrays, unknowns
    })
}

fn default_mult_unit(chr: u8) -> (f64, Option<&'static str>) {
    match chr {
        b'c' | b'C' | b'e' | b'E' => (0.01, None),
        b'L' => (1e-7, Some("deg")),
        _ => (1.0, None),
    }
}

fn is_integer_chr(chr: u8) -> bool {
    matches!(
        chr,
        b'b' | b'B' | b'M' | b'h' | b'H' | b'i' | b'I' | b'q' | b'Q'
    )
}

fn str_len(chr: u8) -> usize {
    match chr {
        b'n' => 4,
        b'N' => 16,
        b'Z' => 64,
        _ => 0,
    }
}

/// Decode a NUL-padded ASCII field into a trimmed lossy string.
fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// NUL-trimmed byte ids (FMTU unit/mult id strings).
fn trimmed_bytes(bytes: &[u8]) -> Vec<u8> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end].to_vec()
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

    use arrow::array::{Float32Array, Int16Array};
    use delog_core::ingest::SourceKind;
    use delog_core::parse_ctl::CancelToken;

    use super::*;

    /// Collects submitted batches synchronously for assertions.
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

    /// Append an FMT record describing message `type_id`.
    fn push_fmt(buf: &mut Vec<u8>, type_id: u8, name: &str, format: &str, columns: &str) {
        let payload_len: usize = format.bytes().map(|c| type_size(c).unwrap()).sum();
        buf.extend([HEAD1, HEAD2, FMT_MSGID, type_id, (payload_len + 3) as u8]);
        push_padded(buf, name.as_bytes(), 4);
        push_padded(buf, format.as_bytes(), 16);
        push_padded(buf, columns.as_bytes(), 64);
    }

    fn push_padded(buf: &mut Vec<u8>, bytes: &[u8], width: usize) {
        buf.extend(bytes);
        buf.extend(std::iter::repeat_n(0u8, width - bytes.len()));
    }

    fn push_rec(buf: &mut Vec<u8>, msgid: u8, payload: &[u8]) {
        buf.extend([HEAD1, HEAD2, msgid]);
        buf.extend(payload);
    }

    fn parse(buf: Vec<u8>) -> (ParseSummary, Collect) {
        let mut sink = Collect::default();
        let ctl = ParseCtl::new(CancelToken::new(), SourceId(0), buf.len() as u64);
        let summary = ArduPilotParser
            .parse(Box::new(Cursor::new(buf)), &mut sink, &ctl)
            .unwrap();
        (summary, sink)
    }

    fn batch<'a>(sink: &'a Collect, topic: &str) -> &'a ParsedBatch {
        sink.batches
            .iter()
            .find(|b| b.topic() == topic)
            .unwrap_or_else(|| panic!("no batch for topic {topic}"))
    }

    /// A self-describing log with a scalar, a fixed-point field, strings, and an
    /// instance-split message.
    fn golden_log() -> Vec<u8> {
        let mut buf = Vec::new();
        // TEST: TimeUS(Q), A(f float), B(c int16 ×0.01).
        push_fmt(&mut buf, 200, "TEST", "Qfc", "TimeUS,A,B");
        // MOT: TimeUS(Q), I(B instance), Thr(f).
        push_fmt(&mut buf, 201, "MOT", "QBf", "TimeUS,I,Thr");

        let mut test_rec = |t: u64, a: f32, b: i16| {
            let mut p = Vec::new();
            p.extend(t.to_le_bytes());
            p.extend(a.to_le_bytes());
            p.extend(b.to_le_bytes());
            push_rec(&mut buf, 200, &p);
        };
        test_rec(1_000, 1.5, 250);
        test_rec(2_000, 2.5, 300);

        let mut mot_rec = |t: u64, inst: u8, thr: f32| {
            let mut p = Vec::new();
            p.extend(t.to_le_bytes());
            p.push(inst);
            p.extend(thr.to_le_bytes());
            push_rec(&mut buf, 201, &p);
        };
        mot_rec(1_000, 0, 10.0);
        mot_rec(1_000, 1, 20.0);
        mot_rec(2_000, 0, 11.0);
        buf
    }

    #[test]
    fn golden_topics_rows_and_values() {
        let (summary, sink) = parse(golden_log());

        // TEST(2) + MOT[0](2) + MOT[1](1).
        assert_eq!(summary.topic_count, 3);
        assert_eq!(summary.row_count, 5);
        assert_eq!(summary.diagnostics, 0);

        let test = batch(&sink, "TEST");
        assert_eq!(test.timestamps.values(), &[1_000, 2_000]);
        // Field A (f32) stored verbatim; field B (c) kept raw with ×0.01 schema.
        let a = test.columns[0]
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        assert_eq!(a.values(), &[1.5, 2.5]);
        let b = test.columns[1]
            .as_any()
            .downcast_ref::<Int16Array>()
            .unwrap();
        assert_eq!(b.values(), &[250, 300]);
        assert_eq!(test.schema.field_by_name("B").unwrap().multiplier, 0.01);
        // TimeUS and the instance field are not columns.
        assert!(test.schema.field_by_name("TimeUS").is_none());

        // Instance split into separate topics; the discriminator is not a column.
        let mot0 = batch(&sink, "MOT[0]");
        assert_eq!(mot0.timestamps.values(), &[1_000, 2_000]);
        assert!(mot0.schema.field_by_name("I").is_none());
        let mot1 = batch(&sink, "MOT[1]");
        assert_eq!(mot1.timestamps.values(), &[1_000]);
    }

    #[test]
    fn sniff_scores_the_dataflash_header() {
        assert_eq!(
            ArduPilotParser
                .sniff(&[HEAD1, HEAD2, FMT_MSGID, FMT_MSGID])
                .score,
            99
        );
        assert_eq!(ArduPilotParser.sniff(&[HEAD1, HEAD2, 200]).score, 90);
        assert_eq!(ArduPilotParser.sniff(b"not a log").score, 0);
    }

    #[test]
    fn garbage_between_records_resyncs_with_a_diagnostic() {
        // Junk with no sync pair, spliced between the FMT and a valid record.
        let mut corrupt = Vec::new();
        push_fmt(&mut corrupt, 200, "TEST", "Qfc", "TimeUS,A,B");
        corrupt.extend([0xDE, 0xAD, 0xBE, 0xEF, 0x00]);
        let mut p = Vec::new();
        p.extend(3_000u64.to_le_bytes());
        p.extend(7.0f32.to_le_bytes());
        p.extend(700i16.to_le_bytes());
        push_rec(&mut corrupt, 200, &p);

        let (summary, sink) = parse(corrupt);
        assert!(sink.diags.iter().any(|d| d.code == "bin-resync"));
        // The valid record after the garbage still parsed.
        let test = batch(&sink, "TEST");
        assert_eq!(test.timestamps.values(), &[3_000]);
        assert!(summary.diagnostics >= 1);
    }
}
