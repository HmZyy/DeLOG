use arrow::array::{
    Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::DataType;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnchorKind {
    First,
    Last,
    FirstChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AlignmentError {
    FieldUnavailable,
    NoFiniteSamples,
    NoChange,
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SyncSample {
    pub row: usize,
    pub raw_time_us: i64,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SampleNeighborhood {
    pub previous: Option<SyncSample>,
    pub current: SyncSample,
    pub next: Option<SyncSample>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ComparableValue {
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    Boolean(bool),
}

#[derive(Debug, Clone, Copy)]
struct DecodedSample {
    public: SyncSample,
    comparable: ComparableValue,
}

struct ResolvedField<'a> {
    store: &'a TopicStore,
    column: usize,
    multiplier: f64,
}

fn resolve_field(
    snapshot: &StoreSnapshot,
    field: FieldId,
) -> Result<ResolvedField<'_>, AlignmentError> {
    let entry = snapshot
        .fields
        .get(field.index())
        .filter(|entry| entry.id == field && !entry.removed)
        .ok_or(AlignmentError::FieldUnavailable)?;
    let topic = snapshot
        .topic(entry.topic)
        .filter(|topic| !topic.entry.removed)
        .ok_or(AlignmentError::FieldUnavailable)?;
    let store = topic
        .store
        .as_deref()
        .ok_or(AlignmentError::FieldUnavailable)?;
    let column = store
        .schema
        .field_index(&entry.name)
        .ok_or(AlignmentError::FieldUnavailable)?;
    let multiplier = store
        .schema
        .field(column)
        .ok_or(AlignmentError::FieldUnavailable)?
        .multiplier;
    Ok(ResolvedField {
        store,
        column,
        multiplier,
    })
}

fn decode(
    array: &dyn Array,
    row: usize,
    global_row: usize,
    raw_time_us: i64,
    multiplier: f64,
) -> Option<DecodedSample> {
    if array.is_null(row) {
        return None;
    }

    macro_rules! signed {
        ($array:ty) => {{
            let value = array.as_any().downcast_ref::<$array>()?.value(row) as i64;
            (ComparableValue::Signed(value), value as f64 * multiplier)
        }};
    }
    macro_rules! unsigned {
        ($array:ty) => {{
            let value = array.as_any().downcast_ref::<$array>()?.value(row) as u64;
            (ComparableValue::Unsigned(value), value as f64 * multiplier)
        }};
    }
    macro_rules! float {
        ($array:ty) => {{
            let value = array.as_any().downcast_ref::<$array>()?.value(row) as f64;
            if !value.is_finite() {
                return None;
            }
            (ComparableValue::Float(value), value * multiplier)
        }};
    }

    let (comparable, value) = match array.data_type() {
        DataType::Int8 => signed!(Int8Array),
        DataType::Int16 => signed!(Int16Array),
        DataType::Int32 => signed!(Int32Array),
        DataType::Int64 => signed!(Int64Array),
        DataType::UInt8 => unsigned!(UInt8Array),
        DataType::UInt16 => unsigned!(UInt16Array),
        DataType::UInt32 => unsigned!(UInt32Array),
        DataType::UInt64 => unsigned!(UInt64Array),
        DataType::Float32 => float!(Float32Array),
        DataType::Float64 => float!(Float64Array),
        DataType::Boolean => {
            let value = array.as_any().downcast_ref::<BooleanArray>()?.value(row);
            (
                ComparableValue::Boolean(value),
                u8::from(value) as f64 * multiplier,
            )
        }
        _ => return None,
    };
    if !value.is_finite() {
        return None;
    }

    Some(DecodedSample {
        public: SyncSample {
            row: global_row,
            raw_time_us,
            value,
        },
        comparable,
    })
}

pub(crate) fn anchor(
    snapshot: &StoreSnapshot,
    field: FieldId,
    kind: AnchorKind,
) -> Result<SyncSample, AlignmentError> {
    let resolved = resolve_field(snapshot, field)?;
    let mut global_row = 0;
    let mut last = None;
    let mut previous = None;

    for chunk in resolved.store.chunks.iter() {
        let array = chunk.cols[resolved.column].as_ref();
        for local_row in 0..chunk.len() {
            let decoded = decode(
                array,
                local_row,
                global_row,
                chunk.t.value(local_row),
                resolved.multiplier,
            );
            global_row += 1;
            let Some(decoded) = decoded else {
                continue;
            };

            match kind {
                AnchorKind::First => return Ok(decoded.public),
                AnchorKind::Last => last = Some(decoded.public),
                AnchorKind::FirstChange => {
                    if previous.is_some_and(|value| value != decoded.comparable) {
                        return Ok(decoded.public);
                    }
                    previous = Some(decoded.comparable);
                }
            }
        }
    }

    match kind {
        AnchorKind::First | AnchorKind::Last => last.ok_or(AlignmentError::NoFiniteSamples),
        AnchorKind::FirstChange if previous.is_some() => Err(AlignmentError::NoChange),
        AnchorKind::FirstChange => Err(AlignmentError::NoFiniteSamples),
    }
}

pub(crate) fn sample_neighborhood(
    snapshot: &StoreSnapshot,
    field: FieldId,
    row: usize,
) -> Result<SampleNeighborhood, AlignmentError> {
    sample_neighborhood_impl(snapshot, field, row, &mut 0)
}

fn decode_public_at(
    resolved: &ResolvedField<'_>,
    chunk_index: usize,
    local_row: usize,
    global_row: usize,
    visited_rows: &mut usize,
) -> Option<SyncSample> {
    *visited_rows += 1;
    let chunk = &resolved.store.chunks[chunk_index];
    decode(
        chunk.cols[resolved.column].as_ref(),
        local_row,
        global_row,
        chunk.t.value(local_row),
        resolved.multiplier,
    )
    .map(|sample| sample.public)
}

fn sample_neighborhood_impl(
    snapshot: &StoreSnapshot,
    field: FieldId,
    row: usize,
    visited_rows: &mut usize,
) -> Result<SampleNeighborhood, AlignmentError> {
    let resolved = resolve_field(snapshot, field)?;
    let mut chunk_start = 0usize;
    let (chunk_index, local_row) = resolved
        .store
        .chunks
        .iter()
        .enumerate()
        .find_map(|(index, chunk)| {
            let chunk_end = chunk_start.checked_add(chunk.len())?;
            if row < chunk_end {
                Some((index, row - chunk_start))
            } else {
                chunk_start = chunk_end;
                None
            }
        })
        .ok_or(AlignmentError::NoFiniteSamples)?;
    let current = decode_public_at(&resolved, chunk_index, local_row, row, visited_rows)
        .ok_or(AlignmentError::NoFiniteSamples)?;

    let mut previous = None;
    for candidate_local in (0..local_row).rev() {
        if let Some(sample) = decode_public_at(
            &resolved,
            chunk_index,
            candidate_local,
            chunk_start + candidate_local,
            visited_rows,
        ) {
            previous = Some(sample);
            break;
        }
    }
    if previous.is_none() {
        let mut previous_chunk_start = chunk_start;
        for previous_chunk_index in (0..chunk_index).rev() {
            let chunk = &resolved.store.chunks[previous_chunk_index];
            previous_chunk_start -= chunk.len();
            for candidate_local in (0..chunk.len()).rev() {
                if let Some(sample) = decode_public_at(
                    &resolved,
                    previous_chunk_index,
                    candidate_local,
                    previous_chunk_start + candidate_local,
                    visited_rows,
                ) {
                    previous = Some(sample);
                    break;
                }
            }
            if previous.is_some() {
                break;
            }
        }
    }

    let current_chunk = &resolved.store.chunks[chunk_index];
    let mut next = None;
    for candidate_local in local_row + 1..current_chunk.len() {
        if let Some(sample) = decode_public_at(
            &resolved,
            chunk_index,
            candidate_local,
            chunk_start + candidate_local,
            visited_rows,
        ) {
            next = Some(sample);
            break;
        }
    }
    if next.is_none() {
        let mut next_chunk_start = chunk_start + current_chunk.len();
        for next_chunk_index in chunk_index + 1..resolved.store.chunks.len() {
            let chunk = &resolved.store.chunks[next_chunk_index];
            for candidate_local in 0..chunk.len() {
                if let Some(sample) = decode_public_at(
                    &resolved,
                    next_chunk_index,
                    candidate_local,
                    next_chunk_start + candidate_local,
                    visited_rows,
                ) {
                    next = Some(sample);
                    break;
                }
            }
            if next.is_some() {
                break;
            }
            next_chunk_start += chunk.len();
        }
    }

    Ok(SampleNeighborhood {
        previous,
        current,
        next,
    })
}

#[cfg(test)]
fn sample_neighborhood_with_visited_rows(
    snapshot: &StoreSnapshot,
    field: FieldId,
    row: usize,
) -> Result<(SampleNeighborhood, usize), AlignmentError> {
    let mut visited_rows = 0;
    let neighborhood = sample_neighborhood_impl(snapshot, field, row, &mut visited_rows)?;
    Ok((neighborhood, visited_rows))
}

pub(crate) fn target_offset_us(
    reference_raw_us: i64,
    reference_offset_us: i64,
    target_raw_us: i64,
) -> Result<i64, AlignmentError> {
    reference_raw_us
        .checked_add(reference_offset_us)
        .and_then(|effective| effective.checked_sub(target_raw_us))
        .ok_or(AlignmentError::Overflow)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, BooleanArray, Float64Array, Int64Array, UInt64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::{IdentityRegistry, SourceKind};
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;

    use super::*;

    fn numeric_fixture_with_multiplier(
        times: Vec<i64>,
        values: Vec<Option<f64>>,
        multiplier: f64,
    ) -> (StoreSnapshot, FieldId) {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source_with_kind("source", SourceKind::File);
        let topic = ids.add_topic(source, "DATA").unwrap();
        let field = ids.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "DATA",
                [
                    FieldSchema::new("value", DataType::Float64, None::<String>, multiplier)
                        .unwrap(),
                ],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(values))];
        let chunk = Arc::new(Chunk::try_new(Int64Array::from(times), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        (
            StoreSnapshot::from_registry(&ids, [(topic, store)], 0).unwrap(),
            field,
        )
    }

    fn numeric_fixture(times: Vec<i64>, values: Vec<Option<f64>>) -> (StoreSnapshot, FieldId) {
        numeric_fixture_with_multiplier(times, values, 2.0)
    }

    #[test]
    fn first_last_and_first_change_skip_null_and_non_finite_samples() {
        let (snapshot, field) = numeric_fixture(
            vec![10, 20, 30, 40, 50, 60],
            vec![
                None,
                Some(f64::NAN),
                Some(4.0),
                Some(4.0),
                Some(7.0),
                Some(f64::INFINITY),
            ],
        );
        assert_eq!(
            anchor(&snapshot, field, AnchorKind::First)
                .unwrap()
                .raw_time_us,
            30
        );
        assert_eq!(
            anchor(&snapshot, field, AnchorKind::Last)
                .unwrap()
                .raw_time_us,
            50
        );
        let changed = anchor(&snapshot, field, AnchorKind::FirstChange).unwrap();
        assert_eq!((changed.raw_time_us, changed.value), (50, 14.0));
    }

    #[test]
    fn float_multiplier_overflow_is_excluded_from_first_last_anchors() {
        let (snapshot, field) =
            numeric_fixture_with_multiplier(vec![10, 20], vec![Some(f64::MAX), Some(1.0)], 2.0);
        assert_eq!(
            anchor(&snapshot, field, AnchorKind::First)
                .unwrap()
                .raw_time_us,
            20
        );
        assert_eq!(
            anchor(&snapshot, field, AnchorKind::Last)
                .unwrap()
                .raw_time_us,
            20
        );
    }

    #[test]
    fn integer_multiplier_overflow_is_excluded_while_later_zero_remains() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source_with_kind("source", SourceKind::File);
        let topic = ids.add_topic(source, "DATA").unwrap();
        let field = ids.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "DATA",
                [FieldSchema::new("value", DataType::Int64, None::<String>, f64::MAX).unwrap()],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![Arc::new(Int64Array::from(vec![i64::MAX, 0]))];
        let chunk =
            Arc::new(Chunk::try_new(Int64Array::from(vec![10, 20]), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&ids, [(topic, store)], 0).unwrap();

        assert_eq!(
            anchor(&snapshot, field, AnchorKind::First)
                .unwrap()
                .raw_time_us,
            20
        );
        assert_eq!(
            anchor(&snapshot, field, AnchorKind::Last)
                .unwrap()
                .raw_time_us,
            20
        );
    }

    #[test]
    fn first_change_reports_constant_signal() {
        let (snapshot, field) = numeric_fixture(vec![10, 20], vec![Some(3.0), Some(3.0)]);
        assert_eq!(
            anchor(&snapshot, field, AnchorKind::FirstChange),
            Err(AlignmentError::NoChange)
        );
    }

    #[test]
    fn boolean_first_change_uses_the_latter_sample() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source_with_kind("source", SourceKind::File);
        let topic = ids.add_topic(source, "FLAGS").unwrap();
        let field = ids.add_field(topic, "armed").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "FLAGS",
                [FieldSchema::new("armed", DataType::Boolean, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![Arc::new(BooleanArray::from(vec![false, false, true]))];
        let chunk =
            Arc::new(Chunk::try_new(Int64Array::from(vec![1, 2, 3]), cols, &schema).unwrap());
        let snapshot = StoreSnapshot::from_registry(
            &ids,
            [(
                topic,
                Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
            )],
            0,
        )
        .unwrap();
        assert_eq!(
            anchor(&snapshot, field, AnchorKind::FirstChange)
                .unwrap()
                .raw_time_us,
            3
        );
    }

    #[test]
    fn neighborhood_returns_exact_finite_neighbors_across_null_rows() {
        let (snapshot, field) = numeric_fixture(
            vec![10, 20, 30, 40, 50],
            vec![Some(1.0), None, Some(2.0), None, Some(3.0)],
        );
        let n = sample_neighborhood(&snapshot, field, 2).unwrap();
        assert_eq!(n.previous.unwrap().raw_time_us, 10);
        assert_eq!(n.current.raw_time_us, 30);
        assert_eq!(n.next.unwrap().raw_time_us, 50);
        assert_eq!(
            sample_neighborhood(&snapshot, field, 1),
            Err(AlignmentError::NoFiniteSamples)
        );
    }

    #[test]
    fn target_offset_includes_reference_preview_offset_and_checks_both_operations() {
        assert_eq!(target_offset_us(1_000, 250, 400), Ok(850));
        assert_eq!(
            target_offset_us(i64::MAX, 1, 0),
            Err(AlignmentError::Overflow)
        );
        assert_eq!(
            target_offset_us(i64::MIN, 0, 1),
            Err(AlignmentError::Overflow)
        );
    }

    #[test]
    fn late_neighborhood_lookup_does_not_decode_the_full_prefix() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source_with_kind("source", SourceKind::File);
        let topic = ids.add_topic(source, "DATA").unwrap();
        let field = ids.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "DATA",
                [FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunks = (0..3).map(|chunk_index| {
            let start = chunk_index * 100;
            let times: Vec<_> = (start..start + 100).map(|row| row as i64).collect();
            let values: Vec<_> = (start..start + 100).map(|row| row as f64).collect();
            let cols: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(values))];
            Arc::new(Chunk::try_new(Int64Array::from(times), cols, &schema).unwrap())
        });
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), chunks).unwrap());
        let snapshot = StoreSnapshot::from_registry(&ids, [(topic, store)], 0).unwrap();

        let (neighborhood, visited_rows) =
            sample_neighborhood_with_visited_rows(&snapshot, field, 250).unwrap();

        assert_eq!(neighborhood.current.row, 250);
        assert_eq!(neighborhood.previous.unwrap().row, 249);
        assert_eq!(neighborhood.next.unwrap().row, 251);
        assert_eq!(
            visited_rows, 3,
            "only current and adjacent finite rows are decoded"
        );
    }

    #[test]
    fn first_change_compares_large_signed_integers_before_display_conversion() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source_with_kind("source", SourceKind::File);
        let topic = ids.add_topic(source, "DATA").unwrap();
        let field = ids.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "DATA",
                [FieldSchema::new("value", DataType::Int64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let values = vec![9_007_199_254_740_992_i64, 9_007_199_254_740_993_i64];
        assert_eq!(values[0] as f64, values[1] as f64);
        let cols: Vec<ArrayRef> = vec![Arc::new(Int64Array::from(values))];
        let chunk =
            Arc::new(Chunk::try_new(Int64Array::from(vec![10, 20]), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&ids, [(topic, store)], 0).unwrap();

        assert_eq!(
            anchor(&snapshot, field, AnchorKind::FirstChange)
                .unwrap()
                .raw_time_us,
            20
        );
    }

    #[test]
    fn first_change_compares_large_unsigned_integers_before_display_conversion() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source_with_kind("source", SourceKind::File);
        let topic = ids.add_topic(source, "DATA").unwrap();
        let field = ids.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "DATA",
                [FieldSchema::new("value", DataType::UInt64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let values = vec![9_007_199_254_740_992_u64, 9_007_199_254_740_993_u64];
        assert_eq!(values[0] as f64, values[1] as f64);
        let cols: Vec<ArrayRef> = vec![Arc::new(UInt64Array::from(values))];
        let chunk =
            Arc::new(Chunk::try_new(Int64Array::from(vec![10, 20]), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&ids, [(topic, store)], 0).unwrap();

        assert_eq!(
            anchor(&snapshot, field, AnchorKind::FirstChange)
                .unwrap()
                .raw_time_us,
            20
        );
    }
}
