use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Array, Int64Array};
use arrow::datatypes::DataType;
use criterion::{Criterion, criterion_group, criterion_main};
use delog_cache::{MinMaxPyramid, TraceCache};
use delog_core::chunk::Chunk;
use delog_core::identity::{FieldId, IdentityRegistry};
use delog_core::metrics::MetricsRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

const ROWS: i64 = 1_000_000;
const CHUNK: i64 = 65_536;

fn schema() -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            "S",
            [FieldSchema::new("V", DataType::Float32, Some("u"), 1.0).unwrap()],
        )
        .unwrap(),
    )
}

fn snapshot(rows: i64) -> (StoreSnapshot, FieldId) {
    let schema = schema();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < rows {
        let end = (start + CHUNK).min(rows);
        let times = Int64Array::from((start..end).collect::<Vec<_>>());
        let vals: ArrayRef = Arc::new(Float32Array::from(
            (start..end)
                .map(|i| (i as f32 * 0.001).sin())
                .collect::<Vec<_>>(),
        ));
        chunks.push(Arc::new(
            Chunk::try_new(times, vec![vals], &schema).unwrap(),
        ));
        start = end;
    }
    let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), chunks).unwrap());

    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "S").unwrap();
    let field = identity.add_field(topic, "V").unwrap();
    (
        StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap(),
        field,
    )
}

fn bench_cache(c: &mut Criterion) {
    let metrics = MetricsRegistry::new();
    let ys: Vec<f32> = (0..ROWS).map(|i| (i as f32 * 0.001).sin()).collect();
    let (snap, field) = snapshot(ROWS);
    let cache = TraceCache::build(&snap, field, 0, 0, &metrics).unwrap();

    c.bench_function("cache_build_1M", |b| {
        b.iter(|| TraceCache::build(&snap, field, 0, 0, &metrics).unwrap());
    });

    c.bench_function("cache_yquery_1M", |b| {
        b.iter(|| cache.pyramid.query(&cache.xy, 100_000, 900_000));
    });

    c.bench_function("pyramid_build_1M", |b| {
        b.iter(|| MinMaxPyramid::build(&ys));
    });

    let (snap_small, field_s) = snapshot(ROWS - 512);
    let (snap_full, _) = snapshot(ROWS);
    c.bench_function("cache_append_512", |b| {
        b.iter_batched(
            || TraceCache::build(&snap_small, field_s, 0, 0, &metrics).unwrap(),
            |mut warm| {
                warm.append(&snap_full, field_s, &metrics);
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, bench_cache);
criterion_main!(benches);
