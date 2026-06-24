//! Property test: None-mode export reproduces the exact sorted union timeline.

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use proptest::prelude::*;

use delog_core::chunk::Chunk;
use delog_core::export::{Cell, ResampleMode, RowCursor};
use delog_core::identity::IdentityRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

// ---------------------------------------------------------------------------
// Helpers mirrored from export.rs unit tests
// ---------------------------------------------------------------------------

fn num_schema() -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            "T",
            [FieldSchema::new("V", DataType::Float64, None::<String>, 1.0).unwrap()],
        )
        .unwrap(),
    )
}

/// Build a single-field snapshot from a sorted, deduped `(raw_t, Option<f64>)` list.
/// Returns the snapshot and the field id.
fn snapshot_from_opt_samples(
    samples: &[(i64, Option<f64>)],
    offset_us: i64,
) -> (StoreSnapshot, delog_core::identity::FieldId) {
    let schema = num_schema();
    let times = Int64Array::from(samples.iter().map(|(t, _)| *t).collect::<Vec<_>>());
    let values: ArrayRef = Arc::new(Float64Array::from(
        samples.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
    ));
    let chunk = Arc::new(Chunk::try_new(times, vec![values], &schema).unwrap());
    let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), vec![chunk]).unwrap());

    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    identity.set_source_offset_us(source, offset_us);
    let topic = identity.add_topic(source, "T").unwrap();
    let field = identity.add_field(topic, "V").unwrap();

    let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();
    (snapshot, field)
}

/// Drain all rows from a None-mode RowCursor.
fn collect_none(
    snap: &StoreSnapshot,
    field: delog_core::identity::FieldId,
    t_start: i64,
    t_end: i64,
) -> Vec<(i64, Cell)> {
    let mut cur = RowCursor::new(snap, &[field], t_start, t_end, ResampleMode::None).unwrap();
    let mut rows = Vec::new();
    let mut out = Vec::new();
    while let Some(t) = cur.next_row(&mut out) {
        assert_eq!(
            out.len(),
            1,
            "single-field cursor must yield exactly one cell"
        );
        rows.push((t, out[0]));
    }
    rows
}

// ---------------------------------------------------------------------------
// Property
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn none_single_field_reproduces_samples(
        raw_samples in prop::collection::vec(
            (0i64..1_000_000, prop::option::of(-1e6f64..1e6f64)),
            1..200
        ),
        offset_us in -500_000i64..500_000,
    ) {
        // Sort + dedup timestamps ascending (raw domain).
        let mut data = raw_samples;
        data.sort_by_key(|(t, _)| *t);
        data.dedup_by_key(|(t, _)| *t);

        // Query window: raw range [0, 1_000_000] shifted by offset covers all samples.
        // Use effective-time coordinates for the query window.
        let t_start = offset_us; // effective time of raw_t=0
        let t_end = 1_000_000i64 + offset_us; // effective time of raw_t=1_000_000

        let (snap, field) = snapshot_from_opt_samples(&data, offset_us);

        // Compute the expected in-range samples: those whose effective time is within [t_start, t_end].
        let expected: Vec<(i64, Cell)> = data
            .iter()
            .filter_map(|(raw_t, opt_v)| {
                let eff = raw_t.checked_add(offset_us)?;
                if eff < t_start || eff > t_end {
                    return None;
                }
                let cell = match opt_v {
                    Some(v) if !v.is_nan() => Cell::Num(*v),
                    _ => Cell::Empty,
                };
                Some((eff, cell))
            })
            .collect();

        let got = collect_none(&snap, field, t_start, t_end);

        // Row count must match exactly.
        prop_assert_eq!(
            got.len(),
            expected.len(),
            "row count mismatch: got {} rows, expected {}",
            got.len(),
            expected.len()
        );

        // Each row's timestamp and cell must match.
        for (i, (got_row, exp_row)) in got.iter().zip(expected.iter()).enumerate() {
            prop_assert_eq!(
                got_row.0, exp_row.0,
                "row {}: timestamp mismatch",
                i
            );
            match (got_row.1, exp_row.1) {
                (Cell::Num(gv), Cell::Num(ev)) => {
                    prop_assert!(
                        (gv - ev).abs() < 1e-12,
                        "row {}: Num value mismatch: got {}, expected {}",
                        i, gv, ev
                    );
                }
                (Cell::Empty, Cell::Empty) => {}
                (g, e) => {
                    prop_assert!(false, "row {}: cell variant mismatch: got {:?}, expected {:?}", i, g, e);
                }
            }
        }
    }
}
