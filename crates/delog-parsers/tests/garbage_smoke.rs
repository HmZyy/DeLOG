//! Policy: malformed input must be skipped, never panic, hang, or run away on
//! memory. Stable counterpart to the cargo-fuzz targets in `/fuzz`.

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

fn drive_all(data: &[u8]) {
    drive(&ArduPilotParser, data);
    drive(&ULogParser, data);
    drive(&TlogParser, data);

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

    cases.push(Vec::new());
    cases.push(vec![0x00]);

    for case in &cases {
        drive_all(case);
    }
}
