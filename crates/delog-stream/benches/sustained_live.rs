use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use delog_core::ingest::{IngestSink, ingest_channel};
use delog_core::metrics::MetricsRegistry;

fn sustained_live_batch_sink(c: &mut Criterion) {
    c.bench_function("sustained_live_channel_60x50hz", |b| {
        b.iter(|| {
            let (tx, rx) = ingest_channel();
            let metrics = Arc::new(MetricsRegistry::new());
            let mut sink = tx.live_sink(metrics);
            // This exercises the non-blocking live sink under the planned
            // 60 msg-types @ 50 Hz load shape without needing sockets.
            for _ in 0..3_000 {
                sink.diagnostic(delog_core::diagnostics::Diag::info("bench", "frame"));
            }
            drop(sink);
            while rx.try_recv().is_some() {}
        });
    });
}

criterion_group!(benches, sustained_live_batch_sink);
criterion_main!(benches);
