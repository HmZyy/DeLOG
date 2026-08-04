use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use delog_core::identity::SourceMetadata;
use delog_core::ingest::{IngestSink, ParseSummary};
use delog_core::parse_ctl::ParseCtl;
use delog_core::time::TimeRange;
use parquet::arrow::arrow_reader::ArrowReaderMetadata;
use parquet::errors::ParquetError;
use parquet::file::reader::{ChunkReader, Length};

use crate::parser::{LogParser, ParseError, ReadSeek, Sniff};

mod generic;
mod structured;
#[cfg(test)]
mod testing;

pub const PARQUET_BATCH_ROWS: usize = 8_192;

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

pub(super) fn parse_arrow_error(error: impl std::fmt::Display, emitted_any: bool) -> ParseError {
    parse_data_error(error.to_string(), emitted_any)
}

pub(super) fn cancellation_error(emitted_any: bool) -> ParseError {
    if emitted_any {
        ParseError::Cancelled
    } else {
        ParseError::SetupCancelled
    }
}

pub(super) fn parquet_summary(
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

pub struct ParquetParser;

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
                structured::parse(chunk_reader, reader_metadata, manifest, sink, ctl)
            }
            Ok(None) => generic::parse(chunk_reader, reader_metadata, sink, ctl),
            Err(error) => Err(ParseError::Setup {
                detail: format!("invalid structured DéLOG Parquet metadata: {error}"),
            }),
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

    use arrow::array::{Array, ArrayRef, BooleanArray, Float32Array, Int64Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
    use arrow::record_batch::RecordBatch;
    use delog_core::identity::SourceId;
    use delog_core::ingest::ParsedBatch;
    use delog_core::parse_ctl::{CancelToken, ParseCtl};
    use delog_parquet_format::{
        FORMAT_KEY, FORMAT_NAME, FORMAT_VERSION, FieldManifest, MANIFEST_KEY, Manifest,
        TopicManifest, VERSION_KEY, encode_schema,
    };

    use super::testing::{RecordingSink, drive_parquet, parquet_bytes};
    use super::*;
    use crate::parser::{LogParser, ParseError};

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
        let (result, sink) = drive_parquet(structured_parquet_bytes());

        let summary = result.unwrap();
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
    fn invalid_marked_metadata_never_falls_back_to_the_generic_path() {
        let cases = [
            ("1", "{broken", "invalid manifest JSON"),
            ("2", "{}", "unsupported format version 2"),
        ];

        for (version, manifest, expected) in cases {
            let (result, sink) = drive_parquet(marked_parquet_bytes(version, manifest));

            assert!(matches!(result, Err(ParseError::Setup { .. })));
            assert!(result.unwrap_err().to_string().contains(expected));
            assert!(sink.batches.is_empty());
            assert!(sink.closed.is_none());
        }
    }

    #[test]
    fn structured_padding_row_data_is_dropped_without_validation() {
        let schema = single_float_topic_schema("flight-a", "ATT");
        let batch = single_float_topic_batch(Arc::clone(&schema), vec![None], vec![Some(1.0)]);

        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]));

        result.expect("lenient parse succeeds");
        assert!(sink.batches.is_empty());
        let summary = sink.closed.unwrap();
        assert_eq!(summary.topic_count, 0);
        assert_eq!(summary.row_count, 0);
    }

    #[test]
    fn structured_non_monotonic_across_batches_is_accepted_without_validation() {
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

        let (result, sink) = drive_parquet(parquet_bytes(schema, &[first, second]));

        result.expect("lenient parse succeeds");
        assert_eq!(sink.batches.len(), 4);
        let summary = sink.closed.unwrap();
        assert_eq!(summary.topic_count, 2);
        assert_eq!(summary.row_count, 4);
        assert_eq!(summary.time_range, TimeRange::new(9, 101));
    }

    #[test]
    fn structured_non_monotonic_within_batch_is_accepted_without_validation() {
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

        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]));

        result.expect("lenient parse succeeds");
        assert_eq!(sink.batches.len(), 2);
        let summary = sink.closed.unwrap();
        assert_eq!(summary.topic_count, 2);
        assert_eq!(summary.row_count, 4);
        assert_eq!(summary.time_range, TimeRange::new(5, 15));
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

        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]));

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
    fn structured_topic_instances_cannot_collide_with_unique_original_names() {
        let topic_names = ["ATT", "ATT", "ATT[0]"];
        let manifest = Manifest {
            version: FORMAT_VERSION,
            topics: topic_names
                .iter()
                .enumerate()
                .map(|(index, topic)| TopicManifest {
                    id: index as u32,
                    original_source: format!("flight-{index}"),
                    original_topic: (*topic).into(),
                    timestamp_column: (index * 2) as u32,
                    fields: vec![FieldManifest {
                        column: (index * 2 + 1) as u32,
                        name: "value".into(),
                        unit: None,
                        multiplier: 1.0,
                        description: None,
                    }],
                })
                .collect(),
        };
        let physical_fields = topic_names
            .iter()
            .enumerate()
            .flat_map(|(index, _)| {
                [
                    Field::new(format!("t{index}"), DataType::Int64, true),
                    Field::new(format!("v{index}"), DataType::Float32, true),
                ]
            })
            .collect::<Vec<_>>();
        let schema = Arc::new(encode_schema(physical_fields, &manifest).unwrap());
        let columns = topic_names
            .iter()
            .enumerate()
            .flat_map(|(index, _)| {
                [
                    Arc::new(Int64Array::from(vec![Some(index as i64)])) as ArrayRef,
                    Arc::new(Float32Array::from(vec![Some(index as f32)])) as ArrayRef,
                ]
            })
            .collect::<Vec<_>>();
        let batch = RecordBatch::try_new(Arc::clone(&schema), columns).unwrap();

        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]));

        assert_eq!(result.unwrap().topic_count, 3);
        assert_eq!(
            sink.batches
                .iter()
                .map(|batch| batch.topic())
                .collect::<Vec<_>>(),
            ["ATT[1]", "ATT[2]", "ATT[0]"]
        );
        assert_eq!(
            sink.batches
                .iter()
                .map(|batch| {
                    batch
                        .schema
                        .provenance()
                        .unwrap()
                        .original_source()
                        .to_owned()
                })
                .collect::<Vec<_>>(),
            ["flight-0", "flight-1", "flight-2"]
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

        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]));

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

        let (result, sink) = drive_parquet(parquet_bytes(schema, &[batch]));

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
        let parser = ParquetParser;
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
        let parser = ParquetParser;
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
    fn structured_multi_topic_partial_batch_closes_accurate_summary() {
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
        let second = RecordBatch::try_new(
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

        let (result, sink) = drive_parquet(parquet_bytes(schema, &[first, second]));

        result.expect("lenient parse succeeds");
        assert_eq!(sink.batches.len(), 3);
        let summary = sink.closed.unwrap();
        assert_eq!(summary.topic_count, 2);
        assert_eq!(summary.row_count, 3);
        assert_eq!(summary.time_range, TimeRange::new(5, 20));
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
    fn parquet_magic_is_a_confident_match() {
        assert_eq!(ParquetParser.sniff(b"PAR1rest of the file").score, 100);
        assert_eq!(ParquetParser.sniff(b"not parquet at all").score, 0);
    }
}
