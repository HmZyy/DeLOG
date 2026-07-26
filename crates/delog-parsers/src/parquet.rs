use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arrow::array::{
    Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::{DataType, Schema, TimeUnit};
use bytes::Bytes;
use delog_core::diagnostics::Diag;
use delog_core::identity::SourceMetadata;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch};
use delog_core::parse_ctl::ParseCtl;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::time::TimeRange;
use parquet::arrow::arrow_reader::{ArrowReaderMetadata, ParquetRecordBatchReaderBuilder};
use parquet::errors::ParquetError;
use parquet::file::reader::{ChunkReader, Length};

use crate::parser::{LogParser, ParseError, ReadSeek, Sniff};

mod structured;

pub const PARQUET_BATCH_ROWS: usize = 8_192;

static NEXT_TIMESTAMP_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampUnit {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampCandidate {
    pub column_index: usize,
    pub name: String,
    pub data_type: DataType,
    pub logical_unit: Option<TimestampUnit>,
}

#[derive(Debug, Clone)]
pub struct TimestampSelectionRequest {
    pub request_id: u64,
    pub file_label: String,
    pub candidates: Vec<TimestampCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestampSelection {
    pub column_index: usize,
    pub unit: TimestampUnit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampSelectionError {
    Cancelled,
}

struct ConvertedTimestamps {
    timestamps: Int64Array,
    keep: BooleanArray,
    skipped: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum TimestampConversionError {
    Unsupported(DataType),
    OutOfRange { row: usize },
    SubMicrosecond { row: usize },
    FractionalMicrosecond { row: usize },
}

fn signed_to_us(
    value: i64,
    unit: TimestampUnit,
    row: usize,
) -> Result<i64, TimestampConversionError> {
    match unit {
        TimestampUnit::Seconds => value
            .checked_mul(1_000_000)
            .ok_or(TimestampConversionError::OutOfRange { row }),
        TimestampUnit::Milliseconds => value
            .checked_mul(1_000)
            .ok_or(TimestampConversionError::OutOfRange { row }),
        TimestampUnit::Microseconds => Ok(value),
        TimestampUnit::Nanoseconds => {
            if value % 1_000 != 0 {
                return Err(TimestampConversionError::SubMicrosecond { row });
            }
            Ok(value / 1_000)
        }
    }
}

fn unsigned_to_us(
    value: u64,
    unit: TimestampUnit,
    row: usize,
) -> Result<i64, TimestampConversionError> {
    match unit {
        TimestampUnit::Nanoseconds => {
            if value % 1_000 != 0 {
                return Err(TimestampConversionError::SubMicrosecond { row });
            }
            i64::try_from(value / 1_000).map_err(|_| TimestampConversionError::OutOfRange { row })
        }
        _ => i64::try_from(value)
            .map_err(|_| TimestampConversionError::OutOfRange { row })
            .and_then(|value| signed_to_us(value, unit, row)),
    }
}

fn float_to_us(
    value: f64,
    unit: TimestampUnit,
    row: usize,
) -> Result<Option<i64>, TimestampConversionError> {
    if !value.is_finite() {
        return Ok(None);
    }
    let scale = match unit {
        TimestampUnit::Seconds => 1_000_000.0,
        TimestampUnit::Milliseconds => 1_000.0,
        TimestampUnit::Microseconds => 1.0,
        TimestampUnit::Nanoseconds => 0.001,
    };
    let scaled = value * scale;
    if !scaled.is_finite() || scaled < i64::MIN as f64 || scaled >= i64::MAX as f64 {
        return Err(TimestampConversionError::OutOfRange { row });
    }
    if scaled.fract() != 0.0 {
        return Err(TimestampConversionError::FractionalMicrosecond { row });
    }
    Ok(Some(scaled as i64))
}

fn convert_rows(
    len: usize,
    mut convert: impl FnMut(usize) -> Result<Option<i64>, TimestampConversionError>,
) -> Result<ConvertedTimestamps, TimestampConversionError> {
    let mut timestamps = Vec::with_capacity(len);
    let mut keep = Vec::with_capacity(len);
    let mut skipped = 0;

    for row in 0..len {
        match convert(row)? {
            Some(value) => {
                timestamps.push(value);
                keep.push(true);
            }
            None => {
                keep.push(false);
                skipped += 1;
            }
        }
    }

    Ok(ConvertedTimestamps {
        timestamps: Int64Array::from(timestamps),
        keep: BooleanArray::from(keep),
        skipped,
    })
}

macro_rules! convert_primitive_timestamps {
    ($array:expr, $array_type:ty, $convert:expr) => {{
        let values = $array.as_any().downcast_ref::<$array_type>().unwrap();
        convert_rows(values.len(), |row| {
            if values.is_null(row) {
                Ok(None)
            } else {
                $convert(values.value(row), row).map(Some)
            }
        })
    }};
}

macro_rules! convert_optional_primitive_timestamps {
    ($array:expr, $array_type:ty, $convert:expr) => {{
        let values = $array.as_any().downcast_ref::<$array_type>().unwrap();
        convert_rows(values.len(), |row| {
            if values.is_null(row) {
                Ok(None)
            } else {
                $convert(values.value(row), row)
            }
        })
    }};
}

fn convert_timestamps(
    array: &dyn Array,
    unit: TimestampUnit,
) -> Result<ConvertedTimestamps, TimestampConversionError> {
    match array.data_type() {
        DataType::Int8 => convert_primitive_timestamps!(array, Int8Array, |value, row| {
            signed_to_us(value as i64, unit, row)
        }),
        DataType::Int16 => convert_primitive_timestamps!(array, Int16Array, |value, row| {
            signed_to_us(value as i64, unit, row)
        }),
        DataType::Int32 => convert_primitive_timestamps!(array, Int32Array, |value, row| {
            signed_to_us(value as i64, unit, row)
        }),
        DataType::Int64 => convert_primitive_timestamps!(array, Int64Array, |value, row| {
            signed_to_us(value, unit, row)
        }),
        DataType::UInt8 => convert_primitive_timestamps!(array, UInt8Array, |value, row| {
            unsigned_to_us(value as u64, unit, row)
        }),
        DataType::UInt16 => convert_primitive_timestamps!(array, UInt16Array, |value, row| {
            unsigned_to_us(value as u64, unit, row)
        }),
        DataType::UInt32 => convert_primitive_timestamps!(array, UInt32Array, |value, row| {
            unsigned_to_us(value as u64, unit, row)
        }),
        DataType::UInt64 => convert_primitive_timestamps!(array, UInt64Array, |value, row| {
            unsigned_to_us(value, unit, row)
        }),
        DataType::Float32 => {
            convert_optional_primitive_timestamps!(array, Float32Array, |value, row| {
                float_to_us(value as f64, unit, row)
            })
        }
        DataType::Float64 => {
            convert_optional_primitive_timestamps!(array, Float64Array, |value, row| {
                float_to_us(value, unit, row)
            })
        }
        DataType::Timestamp(TimeUnit::Second, _) => {
            convert_primitive_timestamps!(array, TimestampSecondArray, |value, row| {
                signed_to_us(value, TimestampUnit::Seconds, row)
            })
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            convert_primitive_timestamps!(array, TimestampMillisecondArray, |value, row| {
                signed_to_us(value, TimestampUnit::Milliseconds, row)
            })
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            convert_primitive_timestamps!(array, TimestampMicrosecondArray, |value, row| {
                signed_to_us(value, TimestampUnit::Microseconds, row)
            })
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            convert_primitive_timestamps!(array, TimestampNanosecondArray, |value, row| {
                signed_to_us(value, TimestampUnit::Nanoseconds, row)
            })
        }
        data_type => Err(TimestampConversionError::Unsupported(data_type.clone())),
    }
}

pub trait TimestampSelectionProvider: Send + Sync {
    fn select(
        &self,
        request: TimestampSelectionRequest,
        ctl: &ParseCtl,
    ) -> Result<TimestampSelection, TimestampSelectionError>;
}

fn logical_timestamp_unit(data_type: &DataType) -> Option<TimestampUnit> {
    match data_type {
        DataType::Timestamp(TimeUnit::Second, _) => Some(TimestampUnit::Seconds),
        DataType::Timestamp(TimeUnit::Millisecond, _) => Some(TimestampUnit::Milliseconds),
        DataType::Timestamp(TimeUnit::Microsecond, _) => Some(TimestampUnit::Microseconds),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => Some(TimestampUnit::Nanoseconds),
        _ => None,
    }
}

fn timestamp_candidates(schema: &Schema) -> Vec<TimestampCandidate> {
    schema
        .fields()
        .iter()
        .enumerate()
        .filter_map(|(column_index, field)| {
            let data_type = field.data_type();
            let logical_unit = logical_timestamp_unit(data_type);
            if logical_unit.is_some()
                || matches!(
                    data_type,
                    DataType::Int8
                        | DataType::Int16
                        | DataType::Int32
                        | DataType::Int64
                        | DataType::UInt8
                        | DataType::UInt16
                        | DataType::UInt32
                        | DataType::UInt64
                        | DataType::Float32
                        | DataType::Float64
                )
            {
                Some(TimestampCandidate {
                    column_index,
                    name: field.name().to_owned(),
                    data_type: data_type.clone(),
                    logical_unit,
                })
            } else {
                None
            }
        })
        .collect()
}

fn supported_value_type(dtype: &DataType) -> bool {
    matches!(
        dtype,
        DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float32
            | DataType::Float64
            | DataType::Boolean
            | DataType::Utf8
            | DataType::LargeUtf8
    )
}

fn validate_nondecreasing(
    timestamps: &Int64Array,
    last_timestamp: &mut Option<i64>,
) -> Result<(), String> {
    let mut previous = *last_timestamp;
    for &current in timestamps.values() {
        if let Some(previous) = previous
            && current < previous
        {
            return Err(format!(
                "timestamp regression: previous timestamp {previous}, current timestamp {current}"
            ));
        }
        previous = Some(current);
    }
    *last_timestamp = previous;
    Ok(())
}

fn timestamp_conversion_detail(error: TimestampConversionError) -> String {
    match error {
        TimestampConversionError::Unsupported(dtype) => {
            format!("unsupported timestamp type {dtype}")
        }
        TimestampConversionError::OutOfRange { row } => {
            format!("timestamp at row {row} is outside the microsecond range")
        }
        TimestampConversionError::SubMicrosecond { row } => {
            format!("timestamp at row {row} has sub-microsecond precision")
        }
        TimestampConversionError::FractionalMicrosecond { row } => {
            format!("timestamp at row {row} is not an exact microsecond")
        }
    }
}

fn parse_data_error(detail: impl Into<String>, emitted_any: bool) -> ParseError {
    let detail = detail.into();
    if emitted_any {
        ParseError::Framing {
            byte_offset: 0,
            detail,
        }
    } else {
        ParseError::Setup { detail }
    }
}

fn parse_arrow_error(error: impl std::fmt::Display, emitted_any: bool) -> ParseError {
    parse_data_error(error.to_string(), emitted_any)
}

fn cancellation_error(emitted_any: bool) -> ParseError {
    if emitted_any {
        ParseError::Cancelled
    } else {
        ParseError::SetupCancelled
    }
}

fn parquet_summary(
    emitted_any: bool,
    row_count: u64,
    time_range: Option<TimeRange>,
    diagnostics: u64,
) -> ParseSummary {
    ParseSummary {
        topic_count: u64::from(emitted_any),
        row_count,
        time_range,
        diagnostics,
        source_meta: SourceMetadata::default(),
    }
}

pub struct ParquetParser {
    selection: Arc<dyn TimestampSelectionProvider>,
}

impl ParquetParser {
    pub fn new(selection: Arc<dyn TimestampSelectionProvider>) -> Self {
        Self { selection }
    }
}

impl LogParser for ParquetParser {
    fn name(&self) -> &'static str {
        "parquet"
    }

    fn sniff(&self, head: &[u8]) -> Sniff {
        if head.starts_with(b"PAR1") {
            Sniff::new(100, "Parquet magic")
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
        let chunk_reader = SeekChunkReader::try_new(src).map_err(|error| ParseError::Setup {
            detail: error.to_string(),
        })?;
        let reader_metadata = ArrowReaderMetadata::load(&chunk_reader, Default::default())
            .map_err(|error| ParseError::Setup {
                detail: error.to_string(),
            })?;
        match delog_parquet_format::decode_schema(reader_metadata.schema().as_ref()) {
            Ok(Some(manifest)) => {
                return structured::parse(chunk_reader, reader_metadata, manifest, sink, ctl);
            }
            Ok(None) => {}
            Err(error) => {
                return Err(ParseError::Setup {
                    detail: format!("invalid structured DéLOG Parquet metadata: {error}"),
                });
            }
        }
        let builder = ParquetRecordBatchReaderBuilder::new_with_metadata(
            chunk_reader.clone(),
            reader_metadata.clone(),
        );
        let schema = Arc::clone(builder.schema());
        let total_rows = builder.metadata().file_metadata().num_rows().max(0) as u64;
        let row_group_count = builder.metadata().num_row_groups();

        let candidates = timestamp_candidates(&schema);
        if candidates.is_empty() {
            return Err(ParseError::Setup {
                detail: "Parquet schema has no timestamp-capable columns".to_owned(),
            });
        }
        let request = TimestampSelectionRequest {
            request_id: NEXT_TIMESTAMP_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
            file_label: ctl.label().to_owned(),
            candidates: candidates.clone(),
        };
        let selection = self
            .selection
            .select(request, ctl)
            .map_err(|TimestampSelectionError::Cancelled| ParseError::SetupCancelled)?;
        let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.column_index == selection.column_index)
        else {
            return Err(ParseError::Setup {
                detail: format!(
                    "timestamp selection column {} is stale or ineligible",
                    selection.column_index
                ),
            });
        };
        if candidate
            .logical_unit
            .is_some_and(|unit| unit != selection.unit)
        {
            return Err(ParseError::Setup {
                detail: format!(
                    "timestamp selection unit does not match column `{}`",
                    candidate.name
                ),
            });
        }

        let excluded_time_indices = std::slice::from_ref(&selection.column_index);
        let mut value_indices = Vec::new();
        let mut field_schemas = Vec::new();
        let mut unsupported_names = Vec::new();
        for (index, field) in schema.fields().iter().enumerate() {
            if excluded_time_indices.contains(&index) {
                continue;
            }
            if !supported_value_type(field.data_type()) {
                unsupported_names.push(field.name().to_owned());
                continue;
            }
            let unit = field
                .metadata()
                .get("unit")
                .filter(|unit| !unit.is_empty())
                .cloned();
            let field_schema = FieldSchema::new(
                field.name().to_owned(),
                field.data_type().clone(),
                unit,
                1.0,
            )
            .map_err(|error| ParseError::Setup {
                detail: format!("invalid value field `{}`: {error}", field.name()),
            })?;
            value_indices.push(index);
            field_schemas.push(field_schema);
        }
        if value_indices.is_empty() {
            return Err(ParseError::Setup {
                detail: "Parquet schema has no supported value columns after timestamp projection"
                    .to_owned(),
            });
        }
        let topic_schema = Arc::new(TopicSchema::new(ctl.label(), field_schemas).map_err(
            |error| ParseError::Setup {
                detail: format!("invalid Parquet topic schema: {error}"),
            },
        )?);
        let mut diagnostics = 0;
        if !unsupported_names.is_empty() {
            diagnostics += 1;
            sink.diagnostic(
                Diag::warning(
                    "parquet-unsupported-columns",
                    format!(
                        "skipped unsupported Parquet column(s): {}",
                        unsupported_names.join(", ")
                    ),
                )
                .with_source(ctl.source()),
            );
        }

        let mut emitted_any = false;
        let mut row_count = 0u64;
        let mut skipped_rows = 0u64;
        let mut processed_rows = 0u64;
        let mut time_range: Option<TimeRange> = None;
        let mut last_timestamp = None;

        let parse_result = (|| {
            if ctl.is_cancelled() {
                return Err(cancellation_error(emitted_any));
            }
            for row_group in 0..row_group_count {
                let mut reader = ParquetRecordBatchReaderBuilder::new_with_metadata(
                    chunk_reader.clone(),
                    reader_metadata.clone(),
                )
                .with_batch_size(PARQUET_BATCH_ROWS)
                .with_row_groups(vec![row_group])
                .build()
                .map_err(|error| parse_arrow_error(error, emitted_any))?;
                for batch in &mut reader {
                    let batch = batch.map_err(|error| parse_arrow_error(error, emitted_any))?;
                    let converted = convert_timestamps(
                        batch.column(selection.column_index).as_ref(),
                        selection.unit,
                    )
                    .map_err(|error| {
                        parse_data_error(timestamp_conversion_detail(error), emitted_any)
                    })?;
                    skipped_rows += converted.skipped;
                    processed_rows += batch.num_rows() as u64;
                    if converted.timestamps.is_empty() {
                        ctl.report_fraction(sink, processed_rows as f32 / total_rows.max(1) as f32);
                        if ctl.is_cancelled() {
                            return Err(cancellation_error(emitted_any));
                        }
                        continue;
                    }
                    validate_nondecreasing(&converted.timestamps, &mut last_timestamp)
                        .map_err(|error| parse_data_error(error, emitted_any))?;
                    let columns = value_indices
                        .iter()
                        .map(|&index| {
                            arrow::compute::filter(batch.column(index).as_ref(), &converted.keep)
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| parse_arrow_error(error, emitted_any))?;

                    let first = converted.timestamps.value(0);
                    let last = converted.timestamps.value(converted.timestamps.len() - 1);
                    time_range = Some(match time_range {
                        Some(range) => range.include(last),
                        None => TimeRange::new(first, last).expect("timestamps were validated"),
                    });
                    row_count += converted.timestamps.len() as u64;
                    sink.submit(ParsedBatch::new(
                        ctl.source(),
                        Arc::clone(&topic_schema),
                        converted.timestamps,
                        columns,
                    ));
                    emitted_any = true;
                    ctl.report_fraction(sink, processed_rows as f32 / total_rows.max(1) as f32);
                    if ctl.is_cancelled() {
                        return Err(cancellation_error(emitted_any));
                    }
                }
            }
            Ok(())
        })();

        if skipped_rows > 0 {
            diagnostics += 1;
            sink.diagnostic(
                Diag::warning(
                    "parquet-skipped-timestamps",
                    format!("skipped {skipped_rows} row(s) with null or non-finite timestamps"),
                )
                .with_source(ctl.source()),
            );
        }
        let summary = parquet_summary(emitted_any, row_count, time_range, diagnostics);
        match parse_result {
            Ok(()) => {
                sink.close_source(ctl.source(), summary.clone());
                Ok(summary)
            }
            Err(error) => {
                if emitted_any {
                    sink.close_source(ctl.source(), summary);
                }
                Err(error)
            }
        }
    }
}

#[derive(Clone)]
struct SeekChunkReader {
    inner: Arc<Mutex<Box<dyn ReadSeek>>>,
    len: u64,
}

impl SeekChunkReader {
    fn try_new(mut src: Box<dyn ReadSeek>) -> io::Result<Self> {
        let len = src.seek(SeekFrom::End(0))?;
        src.seek(SeekFrom::Start(0))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(src)),
            len,
        })
    }
}

struct RangeReader {
    inner: Arc<Mutex<Box<dyn ReadSeek>>>,
    pos: u64,
}

impl Read for RangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut src = self.inner.lock().unwrap();
        src.seek(SeekFrom::Start(self.pos))?;
        let read = src.read(buf)?;
        self.pos = self.pos.saturating_add(read as u64);
        Ok(read)
    }
}

impl Length for SeekChunkReader {
    fn len(&self) -> u64 {
        self.len
    }
}

impl ChunkReader for SeekChunkReader {
    type T = RangeReader;

    fn get_read(&self, start: u64) -> parquet::errors::Result<Self::T> {
        if start > self.len {
            return Err(ParquetError::EOF(format!(
                "offset {start} exceeds {}",
                self.len
            )));
        }
        Ok(RangeReader {
            inner: Arc::clone(&self.inner),
            pos: start,
        })
    }

    fn get_bytes(&self, start: u64, length: usize) -> parquet::errors::Result<Bytes> {
        let mut reader = self.get_read(start)?.take(length as u64);
        let mut out = Vec::with_capacity(length);
        reader.read_to_end(&mut out)?;
        if out.len() != length {
            return Err(ParquetError::EOF(format!(
                "expected {length} bytes at {start}, read {}",
                out.len()
            )));
        }
        Ok(Bytes::from(out))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Cursor, Read};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use arrow::array::{
        ArrayRef, BinaryArray, BooleanArray, Date32Array, Float32Array, Float64Array, Int8Array,
        Int16Array, Int32Array, Int64Array, LargeStringArray, ListBuilder, StringArray,
        TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
        TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
    };
    use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
    use arrow::record_batch::RecordBatch;
    use delog_core::diagnostics::Diag;
    use delog_core::identity::SourceId;
    use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
    use delog_core::parse_ctl::{CancelToken, ParseCtl};
    use delog_parquet_format::{
        FORMAT_KEY, FORMAT_NAME, FORMAT_VERSION, FieldManifest, MANIFEST_KEY, Manifest,
        TopicManifest, VERSION_KEY, encode_schema,
    };
    use parquet::arrow::ArrowWriter;

    use super::*;
    use crate::parser::{LogParser, ParseError};

    fn parquet_bytes(schema: SchemaRef, batches: &[RecordBatch]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut writer = ArrowWriter::try_new(&mut out, schema, None).unwrap();
        for batch in batches {
            writer.write(batch).unwrap();
            writer.flush().unwrap();
        }
        writer.close().unwrap();
        out
    }

    #[derive(Default)]
    struct RecordingSink {
        batches: Vec<ParsedBatch>,
        diagnostics: Vec<Diag>,
        progress: Vec<f32>,
        closed: Option<ParseSummary>,
        cancel_after_first: Option<CancelToken>,
        cancel_on_progress: Option<CancelToken>,
    }

    impl IngestSink for RecordingSink {
        fn open_source(&mut self, _key: &str, _kind: SourceKind) -> SourceId {
            SourceId(4)
        }

        fn submit(&mut self, batch: ParsedBatch) {
            self.batches.push(batch);
            if self.batches.len() == 1
                && let Some(token) = &self.cancel_after_first
            {
                token.cancel();
            }
        }

        fn diagnostic(&mut self, diag: Diag) {
            self.diagnostics.push(diag);
        }

        fn progress(&mut self, _source: SourceId, frac: f32) {
            self.progress.push(frac);
            if let Some(token) = &self.cancel_on_progress {
                token.cancel();
            }
        }

        fn close_source(&mut self, _source: SourceId, summary: ParseSummary) {
            self.closed = Some(summary);
        }
    }

    struct FixedProvider {
        response: Option<Result<TimestampSelection, TimestampSelectionError>>,
        calls: AtomicUsize,
    }

    impl FixedProvider {
        fn ok(selection: TimestampSelection) -> Self {
            Self {
                response: Some(Ok(selection)),
                calls: AtomicUsize::new(0),
            }
        }

        fn cancelled() -> Self {
            Self {
                response: Some(Err(TimestampSelectionError::Cancelled)),
                calls: AtomicUsize::new(0),
            }
        }

        fn panic_if_called() -> Self {
            Self {
                response: None,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl TimestampSelectionProvider for FixedProvider {
        fn select(
            &self,
            _request: TimestampSelectionRequest,
            _ctl: &ParseCtl,
        ) -> Result<TimestampSelection, TimestampSelectionError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.response
                .expect("provider must not be called for a DéLOG export")
        }
    }

    fn drive_parquet(
        bytes: Vec<u8>,
        provider: Arc<dyn TimestampSelectionProvider>,
    ) -> (Result<ParseSummary, ParseError>, RecordingSink) {
        let parser = ParquetParser::new(provider);
        let ctl = ParseCtl::new(CancelToken::new(), SourceId(4), bytes.len() as u64)
            .with_label("generic");
        let mut sink = RecordingSink::default();
        let result = parser.parse(Box::new(Cursor::new(bytes)), &mut sink, &ctl);
        (result, sink)
    }

    fn legacy_flat_batch(timestamps: Vec<i64>, values: Vec<f64>) -> (SchemaRef, RecordBatch) {
        let seconds = timestamps
            .iter()
            .map(|&value| value as f64 / 1_000_000.0)
            .collect::<Vec<_>>();
        let schema = Arc::new(Schema::new(vec![
            Field::new("t_us", DataType::Int64, false),
            Field::new("t_s", DataType::Float64, false),
            Field::new("value", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(timestamps)),
                Arc::new(Float64Array::from(seconds)),
                Arc::new(Float64Array::from(values)),
            ],
        )
        .unwrap();
        (schema, batch)
    }

    fn structured_fixture() -> (SchemaRef, RecordBatch) {
        let manifest = Manifest {
            version: FORMAT_VERSION,
            topics: vec![
                TopicManifest {
                    id: 0,
                    original_source: "flight-a".into(),
                    original_topic: "ATT".into(),
                    timestamp_column: 0,
                    fields: vec![FieldManifest {
                        column: 1,
                        name: "Roll".into(),
                        unit: Some("rad".into()),
                        multiplier: 1.0,
                        description: Some("airframe roll".into()),
                    }],
                },
                TopicManifest {
                    id: 1,
                    original_source: "flight-b".into(),
                    original_topic: "STATUS".into(),
                    timestamp_column: 2,
                    fields: vec![
                        FieldManifest {
                            column: 3,
                            name: "armed".into(),
                            unit: None,
                            multiplier: 1.0,
                            description: None,
                        },
                        FieldManifest {
                            column: 4,
                            name: "mode".into(),
                            unit: None,
                            multiplier: 1.0,
                            description: None,
                        },
                    ],
                },
            ],
        };
        let schema = Arc::new(
            encode_schema(
                vec![
                    Field::new("__delog_t0_time", DataType::Int64, true),
                    Field::new("__delog_t0_f0", DataType::Float32, true),
                    Field::new("__delog_t1_time", DataType::Int64, true),
                    Field::new("__delog_t1_f0", DataType::Boolean, true),
                    Field::new("__delog_t1_f1", DataType::Utf8, true),
                ],
                &manifest,
            )
            .unwrap(),
        );
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![Some(10), Some(30), None])),
                Arc::new(Float32Array::from(vec![Some(1.5), None, None])),
                Arc::new(Int64Array::from(vec![Some(5), Some(15), Some(25)])),
                Arc::new(BooleanArray::from(vec![
                    Some(true),
                    Some(false),
                    Some(true),
                ])),
                Arc::new(StringArray::from(vec![
                    Some("MANUAL"),
                    Some("AUTO"),
                    Some("RTL"),
                ])),
            ],
        )
        .unwrap();
        (schema, batch)
    }

    fn structured_parquet_bytes() -> Vec<u8> {
        let (schema, batch) = structured_fixture();
        parquet_bytes(schema, &[batch])
    }

    fn single_float_topic_schema(original_source: &str, original_topic: &str) -> SchemaRef {
        Arc::new(
            encode_schema(
                vec![
                    Field::new("__delog_t0_time", DataType::Int64, true),
                    Field::new("__delog_t0_f0", DataType::Float32, true),
                ],
                &Manifest {
                    version: FORMAT_VERSION,
                    topics: vec![TopicManifest {
                        id: 0,
                        original_source: original_source.into(),
                        original_topic: original_topic.into(),
                        timestamp_column: 0,
                        fields: vec![FieldManifest {
                            column: 1,
                            name: "value".into(),
                            unit: None,
                            multiplier: 1.0,
                            description: None,
                        }],
                    }],
                },
            )
            .unwrap(),
        )
    }

    fn single_float_topic_batch(
        schema: SchemaRef,
        timestamps: Vec<Option<i64>>,
        values: Vec<Option<f32>>,
    ) -> RecordBatch {
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(timestamps)),
                Arc::new(Float32Array::from(values)),
            ],
        )
        .unwrap()
    }

    fn marked_parquet_bytes(version: &str, manifest: &str) -> Vec<u8> {
        let metadata = HashMap::from([
            (FORMAT_KEY.to_owned(), FORMAT_NAME.to_owned()),
            (VERSION_KEY.to_owned(), version.to_owned()),
            (MANIFEST_KEY.to_owned(), manifest.to_owned()),
        ]);
        let schema = Arc::new(Schema::new_with_metadata(
            vec![
                Field::new("__delog_t0_time", DataType::Int64, true),
                Field::new("__delog_t0_f0", DataType::Float32, true),
            ],
            metadata,
        ));
        let batch = single_float_topic_batch(Arc::clone(&schema), vec![Some(1)], vec![Some(1.0)]);
        parquet_bytes(schema, &[batch])
    }

    #[test]
    fn structured_file_reconstructs_independent_topics() {
        let provider = Arc::new(FixedProvider::panic_if_called());
        let calls = Arc::clone(&provider);

        let (result, sink) = drive_parquet(structured_parquet_bytes(), provider);

        let summary = result.unwrap();
        assert_eq!(calls.calls.load(Ordering::Relaxed), 0);
        assert_eq!(summary.topic_count, 2);
        assert_eq!(summary.row_count, 5);

        let att = sink
            .batches
            .iter()
            .filter(|batch| batch.topic() == "ATT")
            .collect::<Vec<_>>();
        assert_eq!(att.len(), 1);
        assert_eq!(att[0].timestamps.values(), &[10, 30]);
        assert_eq!(att[0].schema.fields()[0].dtype, DataType::Float32);
        assert_eq!(
            att[0].schema.provenance().unwrap().original_source(),
            "flight-a"
        );
        assert_eq!(att[0].schema.provenance().unwrap().original_topic(), "ATT");
        assert_eq!(att[0].schema.fields()[0].unit.as_deref(), Some("rad"));
        assert_eq!(att[0].schema.fields()[0].multiplier, 1.0);
        assert_eq!(
            att[0].schema.fields()[0].description.as_deref(),
            Some("airframe roll")
        );
        let roll = att[0].columns[0]
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        assert_eq!(roll.len(), 2);
        assert!(roll.is_valid(0));
        assert!(roll.is_null(1));

        let status = sink
            .batches
            .iter()
            .find(|batch| batch.topic() == "STATUS")
            .unwrap();
        assert_eq!(status.timestamps.values(), &[5, 15, 25]);
        assert_eq!(status.schema.fields()[1].dtype, DataType::Utf8);
    }

    #[test]
    fn invalid_marked_metadata_never_falls_back_to_generic_picker() {
        let cases = [
            ("1", "{broken", "invalid manifest JSON"),
            ("2", "{}", "unsupported format version 2"),
        ];

        for (version, manifest, expected) in cases {
            let provider = Arc::new(FixedProvider::panic_if_called());
            let calls = Arc::clone(&provider);

            let (result, sink) = drive_parquet(marked_parquet_bytes(version, manifest), provider);

            assert!(matches!(result, Err(ParseError::Setup { .. })));
            assert!(result.unwrap_err().to_string().contains(expected));
            assert_eq!(calls.calls.load(Ordering::Relaxed), 0);
            assert!(sink.batches.is_empty());
            assert!(sink.closed.is_none());
        }
    }

    #[test]
    fn structured_padding_rejects_non_null_topic_data() {
        let schema = single_float_topic_schema("flight-a", "ATT");
        let batch = single_float_topic_batch(Arc::clone(&schema), vec![None], vec![Some(1.0)]);
        let provider = Arc::new(FixedProvider::panic_if_called());

        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);

        assert!(matches!(result, Err(ParseError::Setup { .. })));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("non-null data in padding row 0")
        );
        assert!(sink.batches.is_empty());
        assert!(sink.closed.is_none());
    }

    #[test]
    fn structured_regression_is_tracked_per_topic_across_batches() {
        let manifest = Manifest {
            version: FORMAT_VERSION,
            topics: vec![
                TopicManifest {
                    id: 0,
                    original_source: "flight-a".into(),
                    original_topic: "A".into(),
                    timestamp_column: 0,
                    fields: vec![FieldManifest {
                        column: 1,
                        name: "value".into(),
                        unit: None,
                        multiplier: 1.0,
                        description: None,
                    }],
                },
                TopicManifest {
                    id: 1,
                    original_source: "flight-b".into(),
                    original_topic: "B".into(),
                    timestamp_column: 2,
                    fields: vec![FieldManifest {
                        column: 3,
                        name: "value".into(),
                        unit: None,
                        multiplier: 1.0,
                        description: None,
                    }],
                },
            ],
        };
        let schema = Arc::new(
            encode_schema(
                vec![
                    Field::new("a_time", DataType::Int64, true),
                    Field::new("a_value", DataType::Float32, true),
                    Field::new("b_time", DataType::Int64, true),
                    Field::new("b_value", DataType::Float32, true),
                ],
                &manifest,
            )
            .unwrap(),
        );
        let batch = |a_time, b_time| {
            RecordBatch::try_new(
                Arc::clone(&schema),
                vec![
                    Arc::new(Int64Array::from(vec![Some(a_time)])),
                    Arc::new(Float32Array::from(vec![Some(1.0)])),
                    Arc::new(Int64Array::from(vec![Some(b_time)])),
                    Arc::new(Float32Array::from(vec![Some(2.0)])),
                ],
            )
            .unwrap()
        };
        let first = batch(10, 100);
        let second = batch(9, 101);

        let (result, sink) = drive_parquet(
            parquet_bytes(schema, &[first, second]),
            Arc::new(FixedProvider::panic_if_called()),
        );

        assert!(matches!(result, Err(ParseError::Framing { .. })));
        let error = result.unwrap_err().to_string();
        assert!(error.contains("topic `A`"));
        assert!(error.contains("previous timestamp 10"));
        assert!(error.contains("current timestamp 9"));
        assert_eq!(sink.batches.len(), 2);
        assert_eq!(sink.closed.as_ref().unwrap().topic_count, 2);
        assert_eq!(sink.closed.as_ref().unwrap().row_count, 2);
    }

    #[test]
    fn structured_regression_within_batch_identifies_the_bad_topic() {
        let (schema, _) = structured_fixture();
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![Some(10), Some(9)])),
                Arc::new(Float32Array::from(vec![Some(1.0), Some(2.0)])),
                Arc::new(Int64Array::from(vec![Some(5), Some(15)])),
                Arc::new(BooleanArray::from(vec![Some(true), Some(false)])),
                Arc::new(StringArray::from(vec![Some("MANUAL"), Some("AUTO")])),
            ],
        )
        .unwrap();

        let (result, sink) = drive_parquet(
            parquet_bytes(schema, &[batch]),
            Arc::new(FixedProvider::panic_if_called()),
        );

        assert!(matches!(result, Err(ParseError::Setup { .. })));
        let error = result.unwrap_err().to_string();
        assert!(error.contains("topic `ATT`"));
        assert!(error.contains("previous timestamp 10"));
        assert!(error.contains("current timestamp 9"));
        assert!(sink.batches.is_empty());
        assert!(sink.closed.is_none());
    }

    #[test]
    fn structured_same_named_topics_get_stable_instances_and_provenance() {
        let manifest = Manifest {
            version: FORMAT_VERSION,
            topics: vec![
                TopicManifest {
                    id: 9,
                    original_source: "flight-a".into(),
                    original_topic: "ATT".into(),
                    timestamp_column: 0,
                    fields: vec![FieldManifest {
                        column: 1,
                        name: "Roll".into(),
                        unit: None,
                        multiplier: 1.0,
                        description: None,
                    }],
                },
                TopicManifest {
                    id: 3,
                    original_source: "flight-b".into(),
                    original_topic: "ATT".into(),
                    timestamp_column: 2,
                    fields: vec![FieldManifest {
                        column: 3,
                        name: "Roll".into(),
                        unit: None,
                        multiplier: 1.0,
                        description: None,
                    }],
                },
            ],
        };
        let schema = Arc::new(
            encode_schema(
                vec![
                    Field::new("a_time", DataType::Int64, true),
                    Field::new("a_roll", DataType::Float32, true),
                    Field::new("b_time", DataType::Int64, true),
                    Field::new("b_roll", DataType::Float32, true),
                ],
                &manifest,
            )
            .unwrap(),
        );
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Float32Array::from(vec![Some(1.0)])),
                Arc::new(Int64Array::from(vec![Some(2)])),
                Arc::new(Float32Array::from(vec![Some(2.0)])),
            ],
        )
        .unwrap();

        let (result, sink) = drive_parquet(
            parquet_bytes(schema, &[batch]),
            Arc::new(FixedProvider::panic_if_called()),
        );

        assert_eq!(result.unwrap().topic_count, 2);
        assert_eq!(
            sink.batches
                .iter()
                .map(|batch| batch.topic())
                .collect::<Vec<_>>(),
            ["ATT[0]", "ATT[1]"]
        );
        assert_eq!(
            sink.batches[0]
                .schema
                .provenance()
                .unwrap()
                .original_source(),
            "flight-a"
        );
        assert_eq!(
            sink.batches[1]
                .schema
                .provenance()
                .unwrap()
                .original_source(),
            "flight-b"
        );
    }

    #[test]
    fn structured_zero_row_manifest_topic_is_not_emitted() {
        let (schema, _) = structured_fixture();
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![None, None])),
                Arc::new(Float32Array::from(vec![None, None])),
                Arc::new(Int64Array::from(vec![Some(5), Some(15)])),
                Arc::new(BooleanArray::from(vec![Some(true), Some(false)])),
                Arc::new(StringArray::from(vec![Some("MANUAL"), Some("AUTO")])),
            ],
        )
        .unwrap();

        let (result, sink) = drive_parquet(
            parquet_bytes(schema, &[batch]),
            Arc::new(FixedProvider::panic_if_called()),
        );

        let summary = result.unwrap();
        assert_eq!(summary.topic_count, 1);
        assert_eq!(summary.row_count, 2);
        assert_eq!(
            sink.batches
                .iter()
                .map(|batch| batch.topic())
                .collect::<Vec<_>>(),
            ["STATUS"]
        );
    }

    #[test]
    fn structured_emitted_batches_are_bounded_to_8192_rows() {
        let schema = single_float_topic_schema("flight-a", "VALUE");
        let timestamps = (0..=PARQUET_BATCH_ROWS as i64)
            .map(Some)
            .collect::<Vec<_>>();
        let values = (0..=PARQUET_BATCH_ROWS)
            .map(|value| Some(value as f32))
            .collect::<Vec<_>>();
        let batch = single_float_topic_batch(Arc::clone(&schema), timestamps, values);

        let (result, sink) = drive_parquet(
            parquet_bytes(schema, &[batch]),
            Arc::new(FixedProvider::panic_if_called()),
        );

        assert_eq!(result.unwrap().row_count, 8_193);
        assert_eq!(
            sink.batches
                .iter()
                .map(ParsedBatch::rows)
                .collect::<Vec<_>>(),
            [8_192, 1]
        );
        assert!(
            sink.batches
                .iter()
                .all(|batch| batch.rows() <= PARQUET_BATCH_ROWS)
        );
        assert_eq!(sink.progress.len(), 2);
        assert!(sink.progress[0] < 1.0);
        assert_eq!(sink.progress[1], 1.0);
    }

    #[test]
    fn structured_cancellation_before_submission_is_setup_cancelled() {
        let bytes = structured_parquet_bytes();
        let token = CancelToken::new();
        token.cancel();
        let ctl = ParseCtl::new(token, SourceId(4), bytes.len() as u64).with_label("structured");
        let parser = ParquetParser::new(Arc::new(FixedProvider::panic_if_called()));
        let mut sink = RecordingSink::default();

        let result = parser.parse(Box::new(Cursor::new(bytes)), &mut sink, &ctl);

        assert!(matches!(result, Err(ParseError::SetupCancelled)));
        assert!(sink.batches.is_empty());
        assert!(sink.closed.is_none());
    }

    #[test]
    fn structured_cancellation_after_first_topic_closes_partial_summary() {
        let bytes = structured_parquet_bytes();
        let token = CancelToken::new();
        let ctl =
            ParseCtl::new(token.clone(), SourceId(4), bytes.len() as u64).with_label("structured");
        let parser = ParquetParser::new(Arc::new(FixedProvider::panic_if_called()));
        let mut sink = RecordingSink {
            cancel_after_first: Some(token),
            ..RecordingSink::default()
        };

        let result = parser.parse(Box::new(Cursor::new(bytes)), &mut sink, &ctl);

        assert!(matches!(result, Err(ParseError::Cancelled)));
        assert_eq!(sink.batches.len(), 1);
        let summary = sink.closed.unwrap();
        assert_eq!(summary.topic_count, 1);
        assert_eq!(summary.row_count, 2);
        assert_eq!(summary.time_range, TimeRange::new(10, 30));
    }

    #[test]
    fn structured_error_closes_accurate_multi_topic_partial_summary() {
        let (schema, _) = structured_fixture();
        let first = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![Some(10)])),
                Arc::new(Float32Array::from(vec![Some(1.0)])),
                Arc::new(Int64Array::from(vec![Some(5)])),
                Arc::new(BooleanArray::from(vec![Some(true)])),
                Arc::new(StringArray::from(vec![Some("MANUAL")])),
            ],
        )
        .unwrap();
        let invalid_second = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![Some(20)])),
                Arc::new(Float32Array::from(vec![Some(2.0)])),
                Arc::new(Int64Array::from(vec![None])),
                Arc::new(BooleanArray::from(vec![Some(false)])),
                Arc::new(StringArray::from(vec![None::<&str>])),
            ],
        )
        .unwrap();

        let (result, sink) = drive_parquet(
            parquet_bytes(schema, &[first, invalid_second]),
            Arc::new(FixedProvider::panic_if_called()),
        );

        assert!(matches!(result, Err(ParseError::Framing { .. })));
        assert_eq!(sink.batches.len(), 3);
        let summary = sink.closed.unwrap();
        assert_eq!(summary.topic_count, 2);
        assert_eq!(summary.row_count, 3);
        assert_eq!(summary.time_range, TimeRange::new(5, 20));
    }

    #[test]
    fn legacy_flat_schema_uses_provider_and_retains_seconds_column() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("t_us", DataType::Int64, false),
            Field::new("t_s", DataType::Float64, false),
            Field::new("roll", DataType::Float64, true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![10, 20])),
                Arc::new(Float64Array::from(vec![0.0, 0.00001])),
                Arc::new(Float64Array::from(vec![Some(1.0), None])),
            ],
        )
        .unwrap();
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Microseconds,
        }));
        let calls = Arc::clone(&provider);
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        let summary = result.unwrap();
        assert_eq!(summary.row_count, 2);
        assert_eq!(calls.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            sink.batches[0]
                .schema
                .fields()
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["t_s", "roll"]
        );
    }

    #[test]
    fn parquet_magic_is_a_confident_match() {
        let provider = Arc::new(FixedProvider::panic_if_called());
        let parser = ParquetParser::new(provider);
        assert_eq!(parser.sniff(b"PAR1rest").score, 100);
        assert_eq!(parser.sniff(b"not parquet").score, 0);
    }

    #[test]
    fn generic_schema_uses_selected_numeric_column_and_unit() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("value", DataType::Float32, true),
            Field::new("time_ms", DataType::Int64, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Float32Array::from(vec![1.0, 2.0])),
                Arc::new(Int64Array::from(vec![1, 2])),
            ],
        )
        .unwrap();
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 1,
            unit: TimestampUnit::Milliseconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        assert_eq!(result.unwrap().row_count, 2);
        assert_eq!(sink.batches[0].timestamps.values(), &[1_000, 2_000]);
        assert_eq!(sink.batches[0].schema.name(), "generic");
        assert_eq!(sink.batches[0].schema.fields()[0].name, "value");
    }

    #[test]
    fn rejects_regression_across_record_batches() {
        let (schema, first) = legacy_flat_batch(vec![20], vec![1.0]);
        let (_, second) = legacy_flat_batch(vec![10], vec![2.0]);
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Microseconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[first, second]), provider);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("previous timestamp 20"));
        assert!(error.contains("current timestamp 10"));
        assert_eq!(sink.batches.len(), 1);
        assert_eq!(sink.batches[0].timestamps.values(), &[20]);
        assert_eq!(sink.closed.as_ref().unwrap().row_count, 1);
    }

    #[test]
    fn preserves_all_supported_primitive_types_and_nulls() {
        let fields = vec![
            Field::new("time", DataType::Int64, false),
            Field::new("i8", DataType::Int8, true),
            Field::new("i16", DataType::Int16, true),
            Field::new("i32", DataType::Int32, true),
            Field::new("i64", DataType::Int64, true),
            Field::new("u8", DataType::UInt8, true),
            Field::new("u16", DataType::UInt16, true),
            Field::new("u32", DataType::UInt32, true),
            Field::new("u64", DataType::UInt64, true),
            Field::new("f32", DataType::Float32, true),
            Field::new("f64", DataType::Float64, true),
            Field::new("bool", DataType::Boolean, true),
            Field::new("utf8", DataType::Utf8, true),
            Field::new("large_utf8", DataType::LargeUtf8, true),
        ];
        let value_columns: Vec<ArrayRef> = vec![
            Arc::new(Int8Array::from(vec![Some(-1), None])),
            Arc::new(Int16Array::from(vec![Some(-2), None])),
            Arc::new(Int32Array::from(vec![Some(-3), None])),
            Arc::new(Int64Array::from(vec![Some(-4), None])),
            Arc::new(UInt8Array::from(vec![Some(1), None])),
            Arc::new(UInt16Array::from(vec![Some(2), None])),
            Arc::new(UInt32Array::from(vec![Some(3), None])),
            Arc::new(UInt64Array::from(vec![Some(4), None])),
            Arc::new(Float32Array::from(vec![Some(1.5), None])),
            Arc::new(Float64Array::from(vec![Some(2.5), None])),
            Arc::new(BooleanArray::from(vec![Some(true), None])),
            Arc::new(StringArray::from(vec![Some("a"), None])),
            Arc::new(LargeStringArray::from(vec![Some("b"), None])),
        ];
        let mut columns: Vec<ArrayRef> = vec![Arc::new(Int64Array::from(vec![1, 2]))];
        columns.extend(value_columns.iter().cloned());
        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new(Arc::clone(&schema), columns).unwrap();
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Microseconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        result.unwrap();
        let emitted = &sink.batches[0];
        assert_eq!(
            emitted
                .schema
                .fields()
                .iter()
                .map(|field| field.dtype.clone())
                .collect::<Vec<_>>(),
            value_columns
                .iter()
                .map(|column| column.data_type().clone())
                .collect::<Vec<_>>()
        );
        for (actual, expected) in emitted.columns.iter().zip(&value_columns) {
            assert_eq!(actual.nulls(), expected.nulls());
        }
    }

    #[test]
    fn preserves_non_empty_unit_metadata() {
        let mut radians = HashMap::new();
        radians.insert("unit".to_owned(), "rad".to_owned());
        let mut empty = HashMap::new();
        empty.insert("unit".to_owned(), String::new());
        let schema = Arc::new(Schema::new(vec![
            Field::new("time", DataType::Int64, false),
            Field::new("angle", DataType::Float32, true).with_metadata(radians),
            Field::new("empty", DataType::Float32, true).with_metadata(empty),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Float32Array::from(vec![1.0])),
                Arc::new(Float32Array::from(vec![2.0])),
            ],
        )
        .unwrap();
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Microseconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        result.unwrap();
        assert_eq!(
            sink.batches[0].schema.fields()[0].unit.as_deref(),
            Some("rad")
        );
        assert_eq!(sink.batches[0].schema.fields()[1].unit, None);
    }

    #[test]
    fn warns_once_for_unsupported_columns() {
        let list_field = Arc::new(Field::new_list_field(DataType::Int32, true));
        let schema = Arc::new(Schema::new(vec![
            Field::new("time", DataType::Int64, false),
            Field::new("value", DataType::Float32, true),
            Field::new("payload", DataType::Binary, true),
            Field::new("items", DataType::List(list_field), true),
            Field::new("day", DataType::Date32, true),
        ]));
        let mut list = ListBuilder::new(arrow::array::Int32Builder::new());
        list.values().append_value(1);
        list.append(true);
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Float32Array::from(vec![1.0])),
                Arc::new(BinaryArray::from(vec![Some(&b"x"[..])])),
                Arc::new(list.finish()),
                Arc::new(Date32Array::from(vec![Some(1)])),
            ],
        )
        .unwrap();
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Microseconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        result.unwrap();
        assert_eq!(sink.batches[0].schema.fields().len(), 1);
        let diagnostics = sink
            .diagnostics
            .iter()
            .filter(|diag| diag.code == "parquet-unsupported-columns")
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), 1);
        for name in ["payload", "items", "day"] {
            assert!(diagnostics[0].message.contains(name));
        }
    }

    #[test]
    fn fails_setup_when_no_value_columns_remain() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("time", DataType::Int64, false),
            Field::new("payload", DataType::Binary, true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(BinaryArray::from(vec![Some(&b"x"[..])])),
            ],
        )
        .unwrap();
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Microseconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        assert!(matches!(result, Err(ParseError::Setup { .. })));
        assert!(sink.batches.is_empty());
        assert!(sink.closed.is_none());
    }

    #[test]
    fn fails_setup_when_no_timestamp_candidate_exists() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("text", DataType::Utf8, true),
            Field::new("flag", DataType::Boolean, true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(StringArray::from(vec![Some("x")])),
                Arc::new(BooleanArray::from(vec![Some(true)])),
            ],
        )
        .unwrap();
        let provider = Arc::new(FixedProvider::panic_if_called());
        let calls = Arc::clone(&provider);
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        assert!(matches!(result, Err(ParseError::Setup { .. })));
        assert_eq!(calls.calls.load(Ordering::Relaxed), 0);
        assert!(sink.batches.is_empty());
    }

    #[test]
    fn rejects_stale_or_ineligible_provider_selection() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("time", DataType::Int64, false),
            Field::new("text", DataType::Utf8, true),
            Field::new("value", DataType::Float32, true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(StringArray::from(vec![Some("x")])),
                Arc::new(Float32Array::from(vec![1.0])),
            ],
        )
        .unwrap();
        let out_of_range = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 99,
            unit: TimestampUnit::Microseconds,
        }));
        let (result, sink) = drive_parquet(
            parquet_bytes(Arc::clone(&schema), std::slice::from_ref(&batch)),
            out_of_range,
        );
        assert!(matches!(result, Err(ParseError::Setup { .. })));
        assert!(sink.batches.is_empty());

        let ineligible = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 1,
            unit: TimestampUnit::Microseconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), ineligible);
        assert!(matches!(result, Err(ParseError::Setup { .. })));
        assert!(sink.batches.is_empty());
    }

    #[test]
    fn skips_null_and_non_finite_rows_with_one_warning() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("time", DataType::Float64, true),
            Field::new("value", DataType::Float32, true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Float64Array::from(vec![
                    Some(1.0),
                    None,
                    Some(f64::NAN),
                    Some(2.0),
                ])),
                Arc::new(Float32Array::from(vec![1.0, 2.0, 3.0, 4.0])),
            ],
        )
        .unwrap();
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Seconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        result.unwrap();
        assert_eq!(sink.batches[0].timestamps.values(), &[1_000_000, 2_000_000]);
        let diagnostics = sink
            .diagnostics
            .iter()
            .filter(|diag| diag.code == "parquet-skipped-timestamps")
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("skipped 2 row(s)"));
    }

    #[test]
    fn all_invalid_timestamp_rows_complete_without_a_batch() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("time", DataType::Float64, true),
            Field::new("value", DataType::Float32, true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Float64Array::from(vec![None, Some(f64::NAN)])),
                Arc::new(Float32Array::from(vec![1.0, 2.0])),
            ],
        )
        .unwrap();
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Seconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        let summary = result.unwrap();
        assert_eq!(summary.row_count, 0);
        assert_eq!(summary.topic_count, 0);
        assert!(sink.batches.is_empty());
        assert_eq!(sink.closed.as_ref().unwrap(), &summary);
        assert_eq!(
            sink.diagnostics
                .iter()
                .filter(|diag| diag.code == "parquet-skipped-timestamps")
                .count(),
            1
        );
    }

    #[test]
    fn generic_t_s_is_retained_when_another_column_is_selected() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("clock", DataType::Int64, false),
            Field::new("t_s", DataType::Float64, false),
            Field::new("value", DataType::Float32, true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Float64Array::from(vec![0.1])),
                Arc::new(Float32Array::from(vec![2.0])),
            ],
        )
        .unwrap();
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Microseconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        result.unwrap();
        let fields = sink.batches[0]
            .schema
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(fields, ["t_s", "value"]);
    }

    #[test]
    fn reports_row_based_progress_and_summary_counts() {
        let (schema, first) = legacy_flat_batch(vec![10, 20], vec![1.0, 2.0]);
        let (_, second) = legacy_flat_batch(vec![30, 40], vec![3.0, 4.0]);
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Microseconds,
        }));
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[first, second]), provider);
        let summary = result.unwrap();
        assert_eq!(sink.progress.last(), Some(&1.0));
        assert_eq!(summary.topic_count, 1);
        assert_eq!(summary.row_count, 4);
        assert_eq!(summary.time_range.unwrap().min_us, 10);
        assert_eq!(summary.time_range.unwrap().max_us, 40);
        assert_eq!(summary.diagnostics, 0);
        assert_eq!(sink.diagnostics.len() as u64, summary.diagnostics);
        assert_eq!(sink.closed.as_ref().unwrap(), &summary);
    }

    #[test]
    fn provider_cancellation_maps_to_setup_cancelled() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("time", DataType::Int64, false),
            Field::new("value", DataType::Float32, true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Float32Array::from(vec![1.0])),
            ],
        )
        .unwrap();
        let provider = Arc::new(FixedProvider::cancelled());
        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]), provider);
        assert!(matches!(result, Err(ParseError::SetupCancelled)));
        assert!(sink.batches.is_empty());
    }

    #[test]
    fn pre_cancelled_parse_maps_to_setup_cancelled_before_submission() {
        let (schema, batch) = legacy_flat_batch(vec![1], vec![1.0]);
        let bytes = parquet_bytes(schema, &[batch]);
        let token = CancelToken::new();
        token.cancel();
        let ctl = ParseCtl::new(token, SourceId(4), bytes.len() as u64).with_label("generic");
        let parser = ParquetParser::new(Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Microseconds,
        })));
        let mut sink = RecordingSink::default();

        let result = parser.parse(Box::new(Cursor::new(bytes)), &mut sink, &ctl);

        assert!(matches!(result, Err(ParseError::SetupCancelled)));
        assert!(sink.batches.is_empty());
        assert!(sink.closed.is_none());
    }

    #[test]
    fn cancellation_after_all_invalid_batch_maps_to_setup_cancelled() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("time", DataType::Float64, true),
            Field::new("value", DataType::Float32, true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Float64Array::from(vec![None, Some(f64::NAN)])),
                Arc::new(Float32Array::from(vec![1.0, 2.0])),
            ],
        )
        .unwrap();
        let bytes = parquet_bytes(schema, &[batch]);
        let token = CancelToken::new();
        let ctl =
            ParseCtl::new(token.clone(), SourceId(4), bytes.len() as u64).with_label("generic");
        let parser = ParquetParser::new(Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Seconds,
        })));
        let mut sink = RecordingSink {
            cancel_on_progress: Some(token),
            ..RecordingSink::default()
        };

        let result = parser.parse(Box::new(Cursor::new(bytes)), &mut sink, &ctl);

        assert!(matches!(result, Err(ParseError::SetupCancelled)));
        assert!(sink.batches.is_empty());
        assert!(sink.closed.is_none());
    }

    #[test]
    fn cancellation_after_submission_preserves_partial_batches() {
        let timestamps = (0..PARQUET_BATCH_ROWS as i64).collect::<Vec<_>>();
        let values = vec![1.0; PARQUET_BATCH_ROWS];
        let (schema, first) = legacy_flat_batch(timestamps, values);
        let (_, second) = legacy_flat_batch(vec![PARQUET_BATCH_ROWS as i64], vec![2.0]);
        let bytes = parquet_bytes(schema, &[first, second]);
        let token = CancelToken::new();
        let ctl =
            ParseCtl::new(token.clone(), SourceId(4), bytes.len() as u64).with_label("generic");
        let provider = Arc::new(FixedProvider::ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Microseconds,
        }));
        let parser = ParquetParser::new(provider);
        let mut sink = RecordingSink {
            cancel_after_first: Some(token),
            ..RecordingSink::default()
        };
        let result = parser.parse(Box::new(Cursor::new(bytes)), &mut sink, &ctl);
        assert!(matches!(result, Err(ParseError::Cancelled)));
        assert_eq!(sink.batches.len(), 1);
        assert_eq!(
            sink.closed.as_ref().unwrap().row_count,
            PARQUET_BATCH_ROWS as u64
        );
    }

    #[test]
    fn seek_chunk_reader_serves_independent_ranges() {
        let reader =
            SeekChunkReader::try_new(Box::new(Cursor::new(b"0123456789".to_vec()))).unwrap();
        assert_eq!(&reader.get_bytes(2, 4).unwrap()[..], b"2345");
        let mut tail = reader.get_read(7).unwrap();
        let mut out = String::new();
        tail.read_to_string(&mut out).unwrap();
        assert_eq!(out, "789");
    }

    #[test]
    fn schema_inspection_finds_only_timestamp_capable_fields() {
        let schema = Schema::new(vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("counter", DataType::UInt32, false),
            Field::new(
                "stamp",
                DataType::Timestamp(TimeUnit::Nanosecond, None),
                false,
            ),
            Field::new(
                "nested",
                DataType::List(Arc::new(Field::new_list_field(DataType::Int32, true))),
                true,
            ),
        ]);
        let candidates = timestamp_candidates(&schema);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "counter");
        assert_eq!(candidates[0].logical_unit, None);
        assert_eq!(candidates[1].logical_unit, Some(TimestampUnit::Nanoseconds));
    }

    #[test]
    fn schema_inspection_excludes_float16_timestamp_candidates() {
        let schema = Schema::new(vec![Field::new("half", DataType::Float16, false)]);

        assert!(timestamp_candidates(&schema).is_empty());
    }

    #[test]
    fn integer_units_convert_exactly_to_microseconds() {
        let cases: Vec<(ArrayRef, TimestampUnit, Vec<i64>)> = vec![
            (
                Arc::new(Int64Array::from(vec![1, -2])),
                TimestampUnit::Seconds,
                vec![1_000_000, -2_000_000],
            ),
            (
                Arc::new(UInt32Array::from(vec![1, 2])),
                TimestampUnit::Milliseconds,
                vec![1_000, 2_000],
            ),
            (
                Arc::new(Int32Array::from(vec![1, 2])),
                TimestampUnit::Microseconds,
                vec![1, 2],
            ),
            (
                Arc::new(Int64Array::from(vec![1_000, -2_000])),
                TimestampUnit::Nanoseconds,
                vec![1, -2],
            ),
        ];
        for (array, unit, expected) in cases {
            let converted = convert_timestamps(array.as_ref(), unit).unwrap();
            assert_eq!(converted.timestamps.values(), expected.as_slice());
        }
    }

    #[test]
    fn arrow_timestamp_storage_uses_its_declared_unit() {
        let cases: Vec<(ArrayRef, TimestampUnit, Vec<i64>)> = vec![
            (
                Arc::new(TimestampSecondArray::from(vec![1_i64])),
                TimestampUnit::Seconds,
                vec![1_000_000],
            ),
            (
                Arc::new(TimestampMillisecondArray::from(vec![1_i64])),
                TimestampUnit::Milliseconds,
                vec![1_000],
            ),
            (
                Arc::new(TimestampMicrosecondArray::from(vec![1_i64])),
                TimestampUnit::Microseconds,
                vec![1],
            ),
            (
                Arc::new(TimestampNanosecondArray::from(vec![1_000_i64])),
                TimestampUnit::Nanoseconds,
                vec![1],
            ),
        ];
        for (array, unit, expected) in cases {
            let converted = convert_timestamps(array.as_ref(), unit).unwrap();
            assert_eq!(converted.timestamps.values(), expected.as_slice());
        }
    }

    #[test]
    fn null_and_non_finite_float_timestamps_are_skipped() {
        let array = Float64Array::from(vec![Some(1.0), None, Some(f64::NAN), Some(2.0)]);
        let converted = convert_timestamps(&array, TimestampUnit::Seconds).unwrap();
        assert_eq!(converted.timestamps.values(), &[1_000_000, 2_000_000]);
        assert_eq!(converted.skipped, 2);
        assert_eq!(
            converted.keep.iter().collect::<Vec<_>>(),
            vec![Some(true), Some(false), Some(false), Some(true)]
        );
    }

    #[test]
    fn precision_loss_and_overflow_are_errors() {
        assert!(matches!(
            convert_timestamps(&Int64Array::from(vec![1]), TimestampUnit::Nanoseconds),
            Err(TimestampConversionError::SubMicrosecond { row: 0 })
        ));
        assert!(matches!(
            convert_timestamps(
                &Float64Array::from(vec![0.000_000_5]),
                TimestampUnit::Seconds
            ),
            Err(TimestampConversionError::FractionalMicrosecond { row: 0 })
        ));
        assert!(matches!(
            convert_timestamps(
                &UInt64Array::from(vec![u64::MAX]),
                TimestampUnit::Microseconds
            ),
            Err(TimestampConversionError::OutOfRange { row: 0 })
        ));
    }

    #[test]
    fn float_timestamp_at_positive_i64_boundary_is_out_of_range() {
        assert!(matches!(
            convert_timestamps(
                &Float64Array::from(vec![i64::MAX as f64]),
                TimestampUnit::Microseconds
            ),
            Err(TimestampConversionError::OutOfRange { row: 0 })
        ));
    }

    #[test]
    fn keep_mask_filters_value_columns_without_changing_type() {
        let values: ArrayRef = Arc::new(UInt16Array::from(vec![10, 20, 30, 40]));
        let keep = BooleanArray::from(vec![true, false, false, true]);
        let filtered = arrow::compute::filter(values.as_ref(), &keep).unwrap();
        assert_eq!(filtered.data_type(), &DataType::UInt16);
        assert_eq!(
            filtered
                .as_any()
                .downcast_ref::<UInt16Array>()
                .unwrap()
                .values(),
            &[10, 40]
        );
    }
}
