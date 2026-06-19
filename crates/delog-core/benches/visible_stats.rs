use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use delog_core::analysis::visible_field_stats;
use delog_core::chunk::Chunk;
use delog_core::identity::{FieldId, IdentityRegistry};
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

fn snapshot(rows: usize, chunk_rows: usize) -> (StoreSnapshot, FieldId) {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("bench");
    let topic = identity.add_topic(source, "A").unwrap();
    let field = identity.add_field(topic, "v").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "A",
            [FieldSchema::new("v", DataType::Float64, None::<String>, 1.0).unwrap()],
        )
        .unwrap(),
    );
    let chunks = (0..rows)
        .step_by(chunk_rows)
        .map(|start| {
            let end = (start + chunk_rows).min(rows);
            let times = Int64Array::from_iter_values((start..end).map(|i| i as i64));
            let values = Float64Array::from_iter_values((start..end).map(|i| (i % 1000) as f64));
            Arc::new(Chunk::try_new(times, vec![Arc::new(values) as ArrayRef], &schema).unwrap())
        })
        .collect::<Vec<_>>();
    let store = Arc::new(TopicStore::from_chunks(schema, chunks).unwrap());
    (
        StoreSnapshot::from_registry(&identity, [(topic, store)], 1).unwrap(),
        field,
    )
}

fn bench_visible_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("visible_stats");
    for (name, rows, chunk_rows, lo, hi) in [
        ("small_window", 100_000, 65_536, 40_000, 41_000),
        ("fragmented", 100_000, 512, 10_123, 90_456),
        ("multi_million", 2_000_000, 65_536, 0, 1_999_999),
    ] {
        let (snapshot, field) = snapshot(rows, chunk_rows);
        group.bench_with_input(BenchmarkId::new(name, rows), &snapshot, |b, snapshot| {
            b.iter(|| visible_field_stats(snapshot, field, lo, hi).unwrap())
        });
    }
    group.finish();
}

criterion_group!(benches, bench_visible_stats);
criterion_main!(benches);
