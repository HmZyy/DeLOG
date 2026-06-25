//! Budget: < 10 µs per swap, soft-asserted in CI; this bench reads that.

use std::sync::Arc;
use std::time::Instant;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use criterion::{Criterion, criterion_group, criterion_main};

use delog_core::chunk::Chunk;
use delog_core::identity::{IdentityRegistry, TopicId};
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_core::store::TopicStore;

const SPINE_LEN: i64 = 256;
const CHUNK_ROWS: i64 = 512;

fn schema() -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            "T",
            [FieldSchema::new("V", DataType::Float64, Some("u"), 1.0).unwrap()],
        )
        .unwrap(),
    )
}

fn chunk(schema: &TopicSchema, start: i64) -> Arc<Chunk> {
    let times = Int64Array::from((start..start + CHUNK_ROWS).collect::<Vec<_>>());
    let values: ArrayRef = Arc::new(Float64Array::from(vec![0.0_f64; CHUNK_ROWS as usize]));
    Arc::new(Chunk::try_new(times, vec![values], schema).unwrap())
}

fn loaded() -> (IdentityRegistry, TopicId, Arc<TopicStore>) {
    let schema = schema();
    let chunks: Vec<Arc<Chunk>> = (0..SPINE_LEN)
        .map(|i| chunk(&schema, i * CHUNK_ROWS))
        .collect();
    let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), chunks).unwrap());

    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "T").unwrap();
    identity.add_field(topic, "V").unwrap();
    (identity, topic, store)
}

fn bench_snapshot_swap(c: &mut Criterion) {
    let (identity, topic, base_store) = loaded();
    let store = DataStore::from_snapshot(
        StoreSnapshot::from_registry(&identity, [(topic, Arc::clone(&base_store))], 0).unwrap(),
    );
    let next = chunk(&schema(), SPINE_LEN * CHUNK_ROWS);

    c.bench_function("snapshot_swap_append_256_chunks", |b| {
        // Manual timing: each iter appends to the same base spine, so the
        // forever-growing per-iter allocation stays out of the working set.
        b.iter_custom(|iters| {
            let start = Instant::now();
            for _ in 0..iters {
                let appended = base_store.append_chunk(Arc::clone(&next)).unwrap();
                let snapshot =
                    StoreSnapshot::from_registry(&identity, [(topic, Arc::new(appended))], 0)
                        .unwrap();
                store.publish(snapshot).unwrap();
            }
            start.elapsed()
        });
    });
}

criterion_group!(benches, bench_snapshot_swap);
criterion_main!(benches);
