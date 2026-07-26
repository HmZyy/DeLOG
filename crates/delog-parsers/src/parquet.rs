use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use arrow::array::{
    Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::{DataType, Schema, TimeUnit};
use bytes::Bytes;
use delog_core::parse_ctl::ParseCtl;
use parquet::errors::ParquetError;
use parquet::file::reader::{ChunkReader, Length};

use crate::parser::ReadSeek;

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
    if !scaled.is_finite() || scaled < i64::MIN as f64 || scaled > i64::MAX as f64 {
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

fn delog_timestamp_selection(schema: &Schema) -> Option<TimestampSelection> {
    let [timestamp, seconds, ..] = schema.fields().as_ref() else {
        return None;
    };

    (timestamp.name() == "t_us"
        && timestamp.data_type() == &DataType::Int64
        && !timestamp.is_nullable()
        && seconds.name() == "t_s"
        && seconds.data_type() == &DataType::Float64
        && !seconds.is_nullable())
    .then_some(TimestampSelection {
        column_index: 0,
        unit: TimestampUnit::Microseconds,
    })
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
    use std::io::{Cursor, Read};
    use std::sync::Arc;

    use arrow::array::{
        ArrayRef, BooleanArray, Float64Array, Int32Array, Int64Array, TimestampMicrosecondArray,
        TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt16Array,
        UInt32Array, UInt64Array,
    };
    use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

    use super::*;

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
    fn delog_signature_requires_exact_leading_fields() {
        let schema = Schema::new(vec![
            Field::new("t_us", DataType::Int64, false),
            Field::new("t_s", DataType::Float64, false),
            Field::new("roll", DataType::Float64, true),
        ]);
        assert_eq!(delog_timestamp_selection(&schema).unwrap().column_index, 0);

        let nullable = Schema::new(vec![
            Field::new("t_us", DataType::Int64, true),
            Field::new("t_s", DataType::Float64, false),
        ]);
        assert!(delog_timestamp_selection(&nullable).is_none());
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
