//! Shared harness for the parser fuzz targets (PAR-13).
//!
//! Each entry point drives a decoder over arbitrary bytes with a sink that
//! drops everything: fuzzing only asserts the §6.1 error policy — malformed
//! input is skipped with diagnostics, never a panic, hang, or runaway
//! allocation. The same property is smoke-tested on stable in
//! `delog-parsers/tests/garbage_smoke.rs`; these targets add coverage-guided
//! depth in CI.

use std::io::Cursor;

use delog_core::diagnostics::Diag;
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
use delog_core::parse_ctl::{CancelToken, ParseCtl};
use delog_parsers::mavlink::{FrameDecoder, extract_fields};
use delog_parsers::{ArduPilotParser, LogParser, TlogParser, ULogParser};

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

fn drive(parser: &dyn LogParser, data: &[u8]) {
    let mut sink = NullSink;
    let ctl = ParseCtl::new(CancelToken::new(), SourceId(0), data.len() as u64);
    let _ = parser.parse(Box::new(Cursor::new(data.to_vec())), &mut sink, &ctl);
}

/// ArduPilot DataFlash `.BIN` record decoding.
pub fn fuzz_ardupilot(data: &[u8]) {
    drive(&ArduPilotParser, data);
}

/// PX4 ULog definitions + data sections.
pub fn fuzz_ulog(data: &[u8]) {
    drive(&ULogParser, data);
}

/// MAVLink framing at both layers: the raw push-based frame decoder and the
/// `.tlog` µs-envelope parser that wraps it.
pub fn fuzz_mavlink(data: &[u8]) {
    let mut decoder = FrameDecoder::new();
    decoder.push(data);
    while let Some(frame) = decoder.next_frame() {
        if let Some(message) = frame.message.as_ref() {
            let _ = extract_fields(message);
        }
    }
    drive(&TlogParser, data);
}
