//! Property and concurrency tests for the core data model (CORE-11).
//!
//! Three families, matching the checklist item:
//! * accessor properties — [`FieldView::sample_at`] is pinned to a naive scan;
//! * time math — effective/raw round-trips and range algebra;
//! * snapshot interleavings — concurrent publish/load never tears a read.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use proptest::prelude::*;

use delog_core::chunk::Chunk;
use delog_core::field_view::{FieldView, SampleMode, SampleValue};
use delog_core::identity::{FieldId, IdentityRegistry, TopicId};
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_core::store::TopicStore;
use delog_core::time::{
    TimeRange, effective_time_us, global_effective_range, global_range, raw_time_us,
};

// ---------------------------------------------------------------------------
// Shared builders
// ---------------------------------------------------------------------------

fn float_schema(name: &str) -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            name,
            [FieldSchema::new("V", DataType::Float64, Some("u"), 1.0).unwrap()],
        )
        .unwrap(),
    )
}

/// Build a single-field snapshot whose samples are split across chunks of at
/// most `chunk_len` rows, plus the `FieldId` to view.
fn snapshot_from_samples(
    samples: &[(i64, f64)],
    chunk_len: usize,
    offset_us: i64,
) -> (StoreSnapshot, FieldId) {
    let schema = float_schema("T");
    let chunks: Vec<Arc<Chunk>> = samples
        .chunks(chunk_len.max(1))
        .map(|window| {
            let times = Int64Array::from(window.iter().map(|(t, _)| *t).collect::<Vec<_>>());
            let values: ArrayRef = Arc::new(Float64Array::from(
                window.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
            ));
            Arc::new(Chunk::try_new(times, vec![values], &schema).unwrap())
        })
        .collect();
    let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), chunks).unwrap());

    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    identity.set_source_offset_us(source, offset_us);
    let topic = identity.add_topic(source, "T").unwrap();
    let field = identity.add_field(topic, "V").unwrap();

    let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();
    (snapshot, field)
}

/// Strictly-increasing, unique timestamps so prev/next samples are unambiguous.
fn unique_samples() -> impl Strategy<Value = Vec<(i64, f64)>> {
    prop::collection::vec((0_i64..1_000_000, -1.0e6_f64..1.0e6), 1..150).prop_map(|mut v| {
        v.sort_by_key(|(t, _)| *t);
        v.dedup_by_key(|(t, _)| *t);
        v
    })
}

fn sample_f64(value: SampleValue<'_>) -> f64 {
    match value {
        SampleValue::Float(v) => v,
        other => panic!("expected float sample, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Accessor properties — pinned to a naive scan
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prev_and_next_match_a_naive_scan(
        samples in unique_samples(),
        chunk_len in 1usize..16,
        offset_us in -5_000_i64..5_000,
        query_raw in -10_i64..1_000_010,
    ) {
        let (snapshot, field) = snapshot_from_samples(&samples, chunk_len, offset_us);
        let view = FieldView::new(&snapshot, field).unwrap();
        let query_eff = query_raw + offset_us;

        // Naive references over the raw domain.
        let naive_prev = samples.iter().filter(|(t, _)| *t <= query_raw).max_by_key(|(t, _)| *t);
        let naive_next = samples.iter().filter(|(t, _)| *t >= query_raw).min_by_key(|(t, _)| *t);

        match (view.sample_at(query_eff, SampleMode::Prev), naive_prev) {
            (Some(got), Some((t, v))) => {
                prop_assert_eq!(got.raw_time_us, *t);
                prop_assert_eq!(got.effective_time_us, *t + offset_us);
                prop_assert_eq!(sample_f64(got.value), *v);
            }
            (None, None) => {}
            (got, expected) => prop_assert!(false, "prev mismatch: {:?} vs {:?}", got.map(|s| s.raw_time_us), expected),
        }

        match (view.sample_at(query_eff, SampleMode::Next), naive_next) {
            (Some(got), Some((t, v))) => {
                prop_assert_eq!(got.raw_time_us, *t);
                prop_assert_eq!(sample_f64(got.value), *v);
            }
            (None, None) => {}
            (got, expected) => prop_assert!(false, "next mismatch: {:?} vs {:?}", got.map(|s| s.raw_time_us), expected),
        }
    }

    #[test]
    fn linear_interpolation_stays_within_the_bracketing_samples(
        samples in unique_samples(),
        chunk_len in 1usize..16,
        query_raw in 0_i64..1_000_000,
    ) {
        let (snapshot, field) = snapshot_from_samples(&samples, chunk_len, 0);
        let view = FieldView::new(&snapshot, field).unwrap();

        let prev = samples.iter().filter(|(t, _)| *t <= query_raw).max_by_key(|(t, _)| *t).copied();
        let next = samples.iter().filter(|(t, _)| *t >= query_raw).min_by_key(|(t, _)| *t).copied();

        if let (Some((pt, pv)), Some((nt, nv))) = (prev, next) {
            let got = view.sample_at(query_raw, SampleMode::Linear).unwrap();
            prop_assert_eq!(got.raw_time_us, query_raw);
            let y = sample_f64(got.value);
            // Interpolated value must lie within the bracket (inclusive); on an
            // exact hit (pt == nt == query) it equals the endpoint.
            let (lo, hi) = (pv.min(nv), pv.max(nv));
            prop_assert!(y >= lo - 1e-6 && y <= hi + 1e-6, "y={y} not in [{lo},{hi}]");
            if pt == query_raw { prop_assert_eq!(y, pv); }
            if nt == query_raw { prop_assert_eq!(y, nv); }
        }
    }
}

// ---------------------------------------------------------------------------
// Time math properties
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn effective_and_raw_round_trip(raw in i64::MIN/2..i64::MAX/2, offset in -1_000_000_i64..1_000_000) {
        let eff = effective_time_us(raw, offset).unwrap();
        prop_assert_eq!(raw_time_us(eff, offset), Some(raw));
    }

    #[test]
    fn union_contains_both_and_is_commutative(
        a0 in -1_000_000_i64..1_000_000, a1 in -1_000_000_i64..1_000_000,
        b0 in -1_000_000_i64..1_000_000, b1 in -1_000_000_i64..1_000_000,
    ) {
        let a = TimeRange::new(a0.min(a1), a0.max(a1)).unwrap();
        let b = TimeRange::new(b0.min(b1), b0.max(b1)).unwrap();
        let u = a.union(b);
        prop_assert!(u.contains(a.min_us) && u.contains(a.max_us));
        prop_assert!(u.contains(b.min_us) && u.contains(b.max_us));
        prop_assert_eq!(u, b.union(a));
        prop_assert_eq!(u.min_us, a.min_us.min(b.min_us));
        prop_assert_eq!(u.max_us, a.max_us.max(b.max_us));
    }

    #[test]
    fn offset_preserves_width(lo in -1_000_000_i64..1_000_000, span in 0_i64..1_000_000, off in -1_000_000_i64..1_000_000) {
        let range = TimeRange::new(lo, lo + span).unwrap();
        let shifted = range.offset(off).unwrap();
        prop_assert_eq!(shifted.max_us - shifted.min_us, span);
    }

    #[test]
    fn global_range_contains_every_input(
        bounds in prop::collection::vec((-1_000_000_i64..1_000_000, 0_i64..100_000), 1..32)
    ) {
        let ranges: Vec<TimeRange> = bounds.iter().map(|(lo, span)| TimeRange::new(*lo, lo + span).unwrap()).collect();
        let g = global_range(ranges.iter().copied()).unwrap();
        for r in &ranges {
            prop_assert!(g.contains(r.min_us) && g.contains(r.max_us));
        }
        // With zero offsets, the effective global range equals the raw one.
        let with_offsets = ranges.iter().map(|r| (*r, 0_i64));
        prop_assert_eq!(global_effective_range(with_offsets), Some(g));
    }
}

// ---------------------------------------------------------------------------
// Snapshot interleavings — concurrent publish/load is tear-free
// ---------------------------------------------------------------------------

/// A snapshot whose only sample encodes the epoch it is meant to carry, so a
/// reader can prove the `epoch` field and the data it loaded came from the same
/// publication (no torn read).
fn epoch_marked_snapshot(identity: &IdentityRegistry, topic: TopicId, epoch: i64) -> StoreSnapshot {
    let schema = float_schema("T");
    let cols: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![epoch as f64]))];
    let chunk = Arc::new(Chunk::try_new(Int64Array::from(vec![epoch]), cols, &schema).unwrap());
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    StoreSnapshot::from_registry(identity, [(topic, store)], 0).unwrap()
}

#[test]
fn concurrent_publish_and_load_never_tears() {
    const PUBLISHES: i64 = 2_000;

    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "T").unwrap();
    identity.add_field(topic, "V").unwrap();
    let identity = Arc::new(identity);

    // Seed epoch 0 so the store starts coherent.
    let store = Arc::new(DataStore::from_snapshot(epoch_marked_snapshot(
        &identity, topic, 0,
    )));
    let stop = Arc::new(AtomicBool::new(false));

    let readers: Vec<_> = (0..4)
        .map(|_| {
            let store = Arc::clone(&store);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                let mut last_epoch = 0_u64;
                let mut loads = 0_u64;
                while !stop.load(Ordering::Relaxed) {
                    let snap = store.load();
                    // A reader's successive loads see monotonically rising epochs.
                    assert!(snap.epoch >= last_epoch, "epoch went backwards");
                    last_epoch = snap.epoch;
                    // The carried data matches the snapshot's epoch: no torn read.
                    let chunk = &snap.topic_store(topic).unwrap().chunks[0];
                    assert_eq!(chunk.t_min as u64, snap.epoch, "data/epoch mismatch");
                    loads += 1;
                }
                loads
            })
        })
        .collect();

    let writer = {
        let store = Arc::clone(&store);
        let identity = Arc::clone(&identity);
        thread::spawn(move || {
            for epoch in 1..=PUBLISHES {
                let snap = epoch_marked_snapshot(&identity, topic, epoch);
                store.publish(snap).unwrap();
            }
        })
    };

    writer.join().unwrap();
    stop.store(true, Ordering::Relaxed);
    let total_loads: u64 = readers.into_iter().map(|h| h.join().unwrap()).sum();

    assert_eq!(store.current_epoch(), PUBLISHES as u64);
    assert!(total_loads > 0, "readers never observed the store");
}
