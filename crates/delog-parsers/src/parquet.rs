use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

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
                        | DataType::Float16
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
}
