//! Policy: malformed input must be skipped, never panic, hang, or run away on
//! memory. Stable counterpart to the cargo-fuzz targets in `/fuzz`.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use arrow::array::{ArrayRef, Float32Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use delog_core::diagnostics::Diag;
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
use delog_core::parse_ctl::{CancelToken, ParseCtl};
use delog_parsers::mavlink::{FrameDecoder, extract_fields};
use delog_parsers::{
    ArduPilotParser, LogParser, ParquetParser, ParseError, TimestampSelection,
    TimestampSelectionError, TimestampSelectionProvider, TimestampSelectionRequest, TlogParser,
    ULogParser,
};
use parquet::arrow::ArrowWriter;

#[derive(Default)]
struct NullSink;

impl IngestSink for NullSink {
    fn open_source(&mut self, _key: &str, _kind: SourceKind) -> SourceId {
        SourceId(0)
    }
    fn submit(&mut self, _batch: ParsedBatch) {}
    fn diagnostic(&mut self, _diag: Diag) {}
    fn progress(&mut self, _source: SourceId, _frac: f32) {}
    fn close_source(&mut self, _source: SourceId, _summary: ParseSummary) {}
}

struct CancelSelection;

impl TimestampSelectionProvider for CancelSelection {
    fn select(
        &self,
        _request: TimestampSelectionRequest,
        _ctl: &ParseCtl,
    ) -> Result<TimestampSelection, TimestampSelectionError> {
        Err(TimestampSelectionError::Cancelled)
    }
}

struct PanicSelection {
    calls: Arc<AtomicUsize>,
}

impl TimestampSelectionProvider for PanicSelection {
    fn select(
        &self,
        _request: TimestampSelectionRequest,
        _ctl: &ParseCtl,
    ) -> Result<TimestampSelection, TimestampSelectionError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        panic!("timestamp provider must not be called for marked DéLOG Parquet")
    }
}

fn drive(parser: &dyn LogParser, data: &[u8]) {
    let mut sink = NullSink;
    let ctl = ParseCtl::new(CancelToken::new(), SourceId(0), data.len() as u64);
    let _ = parser.parse(Box::new(Cursor::new(data.to_vec())), &mut sink, &ctl);
}

fn drive_all(data: &[u8]) {
    drive(&ArduPilotParser, data);
    drive(&ULogParser, data);
    drive(&TlogParser, data);
    let parquet = ParquetParser::new(Arc::new(CancelSelection));
    drive(&parquet, data);

    let mut decoder = FrameDecoder::new();
    decoder.push(data);
    while let Some(frame) = decoder.next_frame() {
        if let Some(message) = frame.message.as_ref() {
            let _ = extract_fields(message);
        }
    }
}

fn pseudo_random(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn marked_parquet(
    version: Option<&str>,
    manifest: &str,
    timestamps: Vec<Option<i64>>,
    values: Vec<Option<f32>>,
) -> Vec<u8> {
    let mut metadata = HashMap::new();
    metadata.insert("delog.format".to_owned(), "multi-topic".to_owned());
    if let Some(version) = version {
        metadata.insert("delog.version".to_owned(), version.to_owned());
    }
    metadata.insert("delog.manifest".to_owned(), manifest.to_owned());
    let schema = Arc::new(Schema::new_with_metadata(
        vec![
            Field::new("__delog_t0_time", DataType::Int64, true),
            Field::new("__delog_t0_f0", DataType::Float32, true),
        ],
        metadata,
    ));
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(timestamps)),
        Arc::new(Float32Array::from(values)),
    ];
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).unwrap();
    let mut bytes = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut bytes, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
    bytes
}

fn assert_marked_setup_failure(bytes: Vec<u8>, case: &str) {
    let calls = Arc::new(AtomicUsize::new(0));
    let parser = ParquetParser::new(Arc::new(PanicSelection {
        calls: Arc::clone(&calls),
    }));
    let ctl = ParseCtl::new(CancelToken::new(), SourceId(0), bytes.len() as u64)
        .with_label(format!("{case}.parquet"));
    let mut sink = NullSink;

    let result = parser.parse(Box::new(Cursor::new(bytes)), &mut sink, &ctl);

    assert!(
        matches!(result, Err(ParseError::Setup { .. })),
        "{case} should report setup failure, got {result:?}"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "{case} fell back to generic timestamp selection"
    );
}

#[test]
fn pseudo_random_inputs_never_panic_or_hang() {
    for seed in 0..4000u64 {
        let len = (seed as usize * 7) % 600;
        drive_all(&pseudo_random(seed, len));
    }
}

#[test]
fn truncated_and_oversized_headers_are_handled() {
    let mut cases: Vec<Vec<u8>> = Vec::new();

    cases.push(vec![0xA3, 0x95]);
    cases.push([&[0xA3, 0x95, 0x80][..], &pseudo_random(1, 200)].concat());

    let mut ulog = b"ULog\x01\x12\x35\x01".to_vec();
    ulog.extend(0u64.to_le_bytes()); // start timestamp
    ulog.extend([0xFF, 0xFF]); // message length = 65535, no payload follows
    ulog.push(b'F');
    cases.push(ulog);

    for magic in [0xFD_u8, 0xFE] {
        let mut tlog = 1_700_000_000_000_000u64.to_be_bytes().to_vec();
        tlog.push(magic);
        tlog.push(0xFF); // payload length 255, no body follows
        tlog.extend(pseudo_random(magic as u64, 8));
        cases.push(tlog);
    }

    cases.push(vec![0xFD; 4096]);
    cases.push(vec![0xFE; 4096]);
    cases.push(b"PAR1".to_vec());

    cases.push(Vec::new());
    cases.push(vec![0x00]);

    for case in &cases {
        drive_all(case);
    }
}

#[test]
fn malformed_marked_parquet_never_falls_back_to_generic_selection() {
    let valid_manifest = r#"{"version":1,"topics":[{"id":0,"original_source":"flight","original_topic":"ATT","timestamp_column":0,"fields":[{"column":1,"name":"Roll","unit":null,"multiplier":1.0,"description":null}]}]}"#;
    let cases = [
        (
            "malformed manifest",
            marked_parquet(Some("1"), "{broken", vec![Some(1)], vec![Some(2.0)]),
        ),
        (
            "missing version",
            marked_parquet(None, valid_manifest, vec![Some(1)], vec![Some(2.0)]),
        ),
        (
            "unsupported version",
            marked_parquet(
                Some("2"),
                r#"{"version":2,"topics":[]}"#,
                vec![Some(1)],
                vec![Some(2.0)],
            ),
        ),
        (
            "manifest column outside schema",
            marked_parquet(
                Some("1"),
                r#"{"version":1,"topics":[{"id":0,"original_source":"flight","original_topic":"ATT","timestamp_column":0,"fields":[{"column":2,"name":"Roll","unit":null,"multiplier":1.0,"description":null}]}]}"#,
                vec![Some(1)],
                vec![Some(2.0)],
            ),
        ),
    ];

    for (case, bytes) in cases {
        assert_marked_setup_failure(bytes, case);
    }
}

#[test]
fn marked_parquet_rejects_non_null_data_in_padding_rows_without_fallback() {
    let manifest = r#"{"version":1,"topics":[{"id":0,"original_source":"flight","original_topic":"ATT","timestamp_column":0,"fields":[{"column":1,"name":"Roll","unit":null,"multiplier":1.0,"description":null}]}]}"#;
    let bytes = marked_parquet(
        Some("1"),
        manifest,
        vec![Some(1), None],
        vec![Some(2.0), Some(3.0)],
    );

    assert_marked_setup_failure(bytes, "non-null padding");
}
