//! ULog parser hot-path bench: header/format/subscription decode plus
//! repeated `D` records into Arrow batches.

use std::io::Cursor;

use criterion::{Criterion, criterion_group, criterion_main};
use delog_core::diagnostics::Diag;
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
use delog_core::parse_ctl::{CancelToken, ParseCtl};
use delog_parsers::{LogParser, ULogParser};

const MAGIC: &[u8; 7] = b"ULog\x01\x12\x35";
const ROWS: u64 = 100_000;

#[derive(Default)]
struct DropSink {
    rows: usize,
}

impl IngestSink for DropSink {
    fn open_source(&mut self, _key: &str, _kind: SourceKind) -> SourceId {
        SourceId(0)
    }

    fn submit(&mut self, batch: ParsedBatch) {
        self.rows += batch.rows();
    }

    fn diagnostic(&mut self, _diag: Diag) {}
    fn progress(&mut self, _source: SourceId, _frac: f32) {}
    fn close_source(&mut self, _source: SourceId, _summary: ParseSummary) {}
}

fn push_msg(buf: &mut Vec<u8>, ty: u8, payload: &[u8]) {
    buf.extend((payload.len() as u16).to_le_bytes());
    buf.push(ty);
    buf.extend(payload);
}

fn synthetic_ulog(rows: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend(MAGIC);
    buf.push(1);
    buf.extend(0u64.to_le_bytes());

    push_msg(
        &mut buf,
        b'F',
        b"vehicle_local_position:uint64_t timestamp;float x;float y;float z;float[4] q;uint8_t _padding0[4];",
    );
    let mut sub = Vec::new();
    sub.push(0);
    sub.extend(1u16.to_le_bytes());
    sub.extend(b"vehicle_local_position");
    push_msg(&mut buf, b'A', &sub);

    for i in 0..rows {
        let mut data = Vec::with_capacity(2 + 8 + 7 * 4);
        data.extend(1u16.to_le_bytes());
        data.extend((i * 1_000).to_le_bytes());
        data.extend((i as f32).to_le_bytes());
        data.extend((i as f32 * 2.0).to_le_bytes());
        data.extend((-(i as f32)).to_le_bytes());
        for j in 0..4 {
            data.extend((j as f32).to_le_bytes());
        }
        push_msg(&mut buf, b'D', &data);
    }

    buf
}

fn bench_ulog(c: &mut Criterion) {
    let log = synthetic_ulog(ROWS);
    c.bench_function("ulog_parse_100k_rows", |b| {
        b.iter(|| {
            let mut sink = DropSink::default();
            let ctl = ParseCtl::new(CancelToken::new(), SourceId(0), log.len() as u64);
            let summary = ULogParser
                .parse(Box::new(Cursor::new(log.clone())), &mut sink, &ctl)
                .unwrap();
            assert_eq!(summary.row_count, ROWS);
            assert_eq!(sink.rows as u64, ROWS);
        });
    });
}

criterion_group!(benches, bench_ulog);
criterion_main!(benches);
