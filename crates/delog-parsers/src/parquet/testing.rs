use std::io::Cursor;

use arrow::datatypes::SchemaRef;
use arrow::record_batch::RecordBatch;
use delog_core::diagnostics::Diag;
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
use delog_core::parse_ctl::{CancelToken, ParseCtl};
use parquet::arrow::ArrowWriter;

use super::ParquetParser;
use crate::parser::{LogParser, ParseError};

pub(super) fn parquet_bytes(schema: SchemaRef, batches: &[RecordBatch]) -> Vec<u8> {
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
pub(super) struct RecordingSink {
    pub batches: Vec<ParsedBatch>,
    pub diagnostics: Vec<Diag>,
    pub progress: Vec<f32>,
    pub closed: Option<ParseSummary>,
    pub cancel_after_first: Option<CancelToken>,
    pub cancel_on_progress: Option<CancelToken>,
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

pub(super) fn drive_parquet(bytes: Vec<u8>) -> (Result<ParseSummary, ParseError>, RecordingSink) {
    let ctl =
        ParseCtl::new(CancelToken::new(), SourceId(4), bytes.len() as u64).with_label("generic");
    let mut sink = RecordingSink::default();
    let result = ParquetParser.parse(Box::new(Cursor::new(bytes)), &mut sink, &ctl);
    (result, sink)
}
