//! Analysis helpers over immutable snapshots (PLAN.md §17).

use std::collections::HashMap;

use arrow::datatypes::DataType;

use crate::field_view::{FieldViewError, SampleValue, value_at};
use crate::identity::FieldId;
use crate::snapshot::StoreSnapshot;
use crate::time::{effective_time_us, raw_time_us};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobalFieldStats {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub stddev: f64,
    pub count: u64,
    pub missing_count: u64,
    pub rate_hz: Option<f64>,
}

pub type FieldStats = GlobalFieldStats;

#[derive(Debug, Clone, Copy)]
struct StatsAccumulator {
    min: f64,
    max: f64,
    sum: f64,
    sum_sq: f64,
    count: u64,
    missing_count: u64,
    first_us: Option<i64>,
    last_us: Option<i64>,
}

impl StatsAccumulator {
    fn empty() -> Self {
        Self {
            min: f64::NAN,
            max: f64::NAN,
            sum: 0.0,
            sum_sq: 0.0,
            count: 0,
            missing_count: 0,
            first_us: None,
            last_us: None,
        }
    }

    fn observe(&mut self, value: SampleValue<'_>, t_us: i64) {
        let value = match value {
            SampleValue::Int(v) => v as f64,
            SampleValue::UInt(v) => v as f64,
            SampleValue::Float(v) => v,
            SampleValue::Bool(v) => u8::from(v) as f64,
            SampleValue::Null => {
                self.missing_count += 1;
                return;
            }
            SampleValue::Utf8(_) => return,
        };
        if value.is_nan() {
            self.missing_count += 1;
            return;
        }
        self.min = if self.min.is_nan() {
            value
        } else {
            self.min.min(value)
        };
        self.max = if self.max.is_nan() {
            value
        } else {
            self.max.max(value)
        };
        self.sum += value;
        self.sum_sq += value * value;
        self.count += 1;
        self.first_us = Some(self.first_us.map_or(t_us, |first| first.min(t_us)));
        self.last_us = Some(self.last_us.map_or(t_us, |last| last.max(t_us)));
    }

    fn fold_chunk(&mut self, stats: &crate::chunk::ColStats, len: usize, first: i64, last: i64) {
        let count = len as u64 - stats.nan_count;
        if count > 0 {
            self.min = if self.min.is_nan() {
                stats.min
            } else {
                self.min.min(stats.min)
            };
            self.max = if self.max.is_nan() {
                stats.max
            } else {
                self.max.max(stats.max)
            };
            self.sum += stats.sum;
            self.sum_sq += stats.sum_sq;
            self.count += count;
            self.first_us = Some(self.first_us.map_or(first, |v| v.min(first)));
            self.last_us = Some(self.last_us.map_or(last, |v| v.max(last)));
        }
        self.missing_count += stats.nan_count;
    }
}

fn finish_stats(acc: StatsAccumulator, multiplier: f64) -> FieldStats {
    if acc.count == 0 {
        return FieldStats {
            min: f64::NAN,
            max: f64::NAN,
            mean: f64::NAN,
            stddev: f64::NAN,
            count: 0,
            missing_count: acc.missing_count,
            rate_hz: None,
        };
    }
    let mean = acc.sum / acc.count as f64;
    let variance = (acc.sum_sq / acc.count as f64 - mean * mean).max(0.0);
    let (scaled_min, scaled_max) = (acc.min * multiplier, acc.max * multiplier);
    let rate_hz = match (acc.first_us, acc.last_us) {
        (Some(first), Some(last)) if last > first => {
            Some(acc.count as f64 / ((last - first) as f64 / 1e6))
        }
        _ => None,
    };
    FieldStats {
        min: scaled_min.min(scaled_max),
        max: scaled_min.max(scaled_max),
        mean: mean * multiplier,
        stddev: variance.sqrt() * multiplier.abs(),
        count: acc.count,
        missing_count: acc.missing_count,
        rate_hz,
    }
}

/// Exact numeric statistics for samples in the inclusive effective-time window.
/// Fully covered chunks reuse their seal-time statistics; partial chunks read
/// Arrow values in place, upholding ZC-2.
pub fn visible_field_stats(
    snapshot: &StoreSnapshot,
    field: FieldId,
    t0_us: i64,
    t1_us: i64,
) -> Result<Option<FieldStats>, FieldViewError> {
    let field_entry = snapshot
        .fields
        .get(field.index())
        .filter(|entry| entry.id == field)
        .ok_or(FieldViewError::InvalidFieldId(field))?;
    let topic = snapshot
        .topic(field_entry.topic)
        .ok_or(FieldViewError::MissingTopic(field_entry.topic))?;
    let source = snapshot
        .source(topic.entry.source)
        .ok_or(FieldViewError::MissingSource)?;
    let store = topic
        .store
        .as_deref()
        .ok_or(FieldViewError::MissingTopicStore(topic.entry.id))?;
    let col_index = store.schema.field_index(&field_entry.name).ok_or_else(|| {
        FieldViewError::FieldMissingFromSchema {
            topic: topic.entry.id,
            field: field_entry.name.clone(),
        }
    })?;
    let schema = &store.schema.fields()[col_index];
    if !is_numeric(&schema.dtype) {
        return Ok(None);
    }
    let (t0_us, t1_us) = if t0_us <= t1_us {
        (t0_us, t1_us)
    } else {
        (t1_us, t0_us)
    };
    let Some(raw_lo) = raw_time_us(t0_us, source.entry.offset_us) else {
        return Ok(Some(finish_stats(
            StatsAccumulator::empty(),
            schema.multiplier,
        )));
    };
    let Some(raw_hi) = raw_time_us(t1_us, source.entry.offset_us) else {
        return Ok(Some(finish_stats(
            StatsAccumulator::empty(),
            schema.multiplier,
        )));
    };
    let mut acc = StatsAccumulator::empty();
    for chunk in store.chunks.iter() {
        if chunk.t_max < raw_lo || chunk.t_min > raw_hi {
            continue;
        }
        if raw_lo <= chunk.t_min && chunk.t_max <= raw_hi {
            let first = effective_time_us(chunk.t_min, source.entry.offset_us).unwrap_or(t0_us);
            let last = effective_time_us(chunk.t_max, source.entry.offset_us).unwrap_or(t1_us);
            acc.fold_chunk(&chunk.stats[col_index], chunk.len(), first, last);
            continue;
        }
        let start = chunk.t.values().partition_point(|&t| t < raw_lo);
        let end = chunk.t.values().partition_point(|&t| t <= raw_hi);
        let col = chunk.cols[col_index].as_ref();
        for row in start..end {
            let Some(t_us) = effective_time_us(chunk.t.value(row), source.entry.offset_us) else {
                continue;
            };
            acc.observe(value_at(col, row), t_us);
        }
    }
    Ok(Some(finish_stats(acc, schema.multiplier)))
}

/// Fold seal-time [`crate::chunk::ColStats`] for a field. Returns `Ok(None)`
/// for non-numeric fields, because strings have no meaningful min/mean/sigma.
pub fn global_field_stats(
    snapshot: &StoreSnapshot,
    field: FieldId,
) -> Result<Option<GlobalFieldStats>, FieldViewError> {
    let field_entry = snapshot
        .fields
        .get(field.index())
        .filter(|entry| entry.id == field)
        .ok_or(FieldViewError::InvalidFieldId(field))?;
    let topic = snapshot
        .topic(field_entry.topic)
        .ok_or(FieldViewError::MissingTopic(field_entry.topic))?;
    let store = topic
        .store
        .as_deref()
        .ok_or(FieldViewError::MissingTopicStore(topic.entry.id))?;
    let col_index = store.schema.field_index(&field_entry.name).ok_or_else(|| {
        FieldViewError::FieldMissingFromSchema {
            topic: topic.entry.id,
            field: field_entry.name.clone(),
        }
    })?;
    let field_schema = &store.schema.fields()[col_index];
    if !is_numeric(&field_schema.dtype) {
        return Ok(None);
    }

    let mut min = f64::NAN;
    let mut max = f64::NAN;
    let mut sum = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    let mut count = 0_u64;
    let mut missing_count = 0_u64;
    let mut t_min = None;
    let mut t_max = None;

    for chunk in store.chunks.iter() {
        let Some(stats) = chunk.stats.get(col_index) else {
            continue;
        };
        let valid = chunk.len() as u64 - stats.nan_count;
        if valid > 0 {
            if min.is_nan() || stats.min < min {
                min = stats.min;
            }
            if max.is_nan() || stats.max > max {
                max = stats.max;
            }
            sum += stats.sum;
            sum_sq += stats.sum_sq;
            count += valid;
            t_min = Some(t_min.map_or(chunk.t_min, |current: i64| current.min(chunk.t_min)));
            t_max = Some(t_max.map_or(chunk.t_max, |current: i64| current.max(chunk.t_max)));
        }
        missing_count += stats.nan_count;
    }

    if count == 0 {
        return Ok(Some(GlobalFieldStats {
            min: f64::NAN,
            max: f64::NAN,
            mean: f64::NAN,
            stddev: f64::NAN,
            count,
            missing_count,
            rate_hz: None,
        }));
    }

    let mean = sum / count as f64;
    let variance = (sum_sq / count as f64 - mean * mean).max(0.0);
    let rate_hz = match (t_min, t_max) {
        (Some(min_us), Some(max_us)) if max_us > min_us => {
            Some(count as f64 / ((max_us - min_us) as f64 / 1e6))
        }
        _ => None,
    };
    let (scaled_min, scaled_max) = (min * field_schema.multiplier, max * field_schema.multiplier);
    Ok(Some(GlobalFieldStats {
        min: scaled_min.min(scaled_max),
        max: scaled_min.max(scaled_max),
        mean: mean * field_schema.multiplier,
        stddev: variance.sqrt() * field_schema.multiplier.abs(),
        count,
        missing_count,
        rate_hz,
    }))
}

/// One distinct value of a field and the canonical times at which the field
/// transitions *into* it — the start of each contiguous run (ANA-11). Groups
/// are ordered by first appearance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueTransitions {
    /// Canonical display of the value ("4", "true", "AUTO").
    pub value_label: String,
    /// Effective µs timestamps of each transition into this value, ascending.
    pub transitions: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionsError {
    FieldView(FieldViewError),
    /// More distinct values than `max_distinct` — the field is likely
    /// continuous; refuse rather than generate a marker per value.
    TooManyValues(usize),
}

impl From<FieldViewError> for TransitionsError {
    fn from(e: FieldViewError) -> Self {
        Self::FieldView(e)
    }
}

/// Group a field's value-*transition* timestamps by distinct value (ANA-11).
/// A transition is a sample whose value differs from the previous non-null
/// value (the first non-null sample counts). Null/missing is a gap: it ends a
/// run and is not itself marked. Returns `TooManyValues` once the distinct
/// count exceeds `max_distinct`. Groups are ordered by first appearance.
pub fn field_value_transitions(
    snapshot: &StoreSnapshot,
    field: FieldId,
    max_distinct: usize,
) -> Result<Vec<ValueTransitions>, TransitionsError> {
    let field_entry = snapshot
        .fields
        .get(field.index())
        .filter(|entry| entry.id == field)
        .ok_or(FieldViewError::InvalidFieldId(field))?;
    let topic = snapshot
        .topic(field_entry.topic)
        .ok_or(FieldViewError::MissingTopic(field_entry.topic))?;
    let source = snapshot
        .source(topic.entry.source)
        .ok_or(FieldViewError::MissingSource)?;
    let store = topic
        .store
        .as_deref()
        .ok_or(FieldViewError::MissingTopicStore(topic.entry.id))?;
    let col_index = store.schema.field_index(&field_entry.name).ok_or_else(|| {
        FieldViewError::FieldMissingFromSchema {
            topic: topic.entry.id,
            field: field_entry.name.clone(),
        }
    })?;
    let offset = source.entry.offset_us;

    // Collect (effective_time, label) in time order. The chunk spine is already
    // time-ordered when monotonic (the common case); otherwise sort by time so
    // transition detection still sees samples chronologically.
    let mut samples: Vec<(i64, Option<String>)> = Vec::new();
    for chunk in store.chunks.iter() {
        let col = &chunk.cols[col_index];
        for row in 0..chunk.len() {
            let Some(t) = effective_time_us(chunk.t.value(row), offset) else {
                continue;
            };
            samples.push((t, value_label(value_at(col.as_ref(), row))));
        }
    }
    if !store.is_monotonic() {
        samples.sort_by_key(|(t, _)| *t);
    }

    // Walk in time order, emitting a transition each time the (non-null) value
    // differs from the previous one. Null ends the current run.
    let mut order: Vec<String> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut groups: Vec<ValueTransitions> = Vec::new();
    let mut prev: Option<String> = None;
    for (t, label) in samples {
        let Some(label) = label else {
            prev = None;
            continue;
        };
        if prev.as_deref() == Some(label.as_str()) {
            continue;
        }
        prev = Some(label.clone());
        let idx = match index.get(&label) {
            Some(&i) => i,
            None => {
                if order.len() >= max_distinct {
                    return Err(TransitionsError::TooManyValues(order.len() + 1));
                }
                let i = groups.len();
                index.insert(label.clone(), i);
                order.push(label.clone());
                groups.push(ValueTransitions {
                    value_label: label,
                    transitions: Vec::new(),
                });
                i
            }
        };
        groups[idx].transitions.push(t);
    }
    Ok(groups)
}

/// Canonical display label for a discrete value, or `None` for null (a gap).
fn value_label(value: SampleValue<'_>) -> Option<String> {
    match value {
        SampleValue::Int(v) => Some(v.to_string()),
        SampleValue::UInt(v) => Some(v.to_string()),
        SampleValue::Bool(b) => Some(b.to_string()),
        SampleValue::Utf8(s) => Some(s.to_string()),
        SampleValue::Float(v) => Some(v.to_string()),
        SampleValue::Null => None,
    }
}

fn is_numeric(dtype: &DataType) -> bool {
    matches!(
        dtype,
        DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float32
            | DataType::Float64
            | DataType::Boolean
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
    use proptest::prelude::*;

    use super::*;
    use crate::chunk::Chunk;
    use crate::identity::IdentityRegistry;
    use crate::schema::{FieldSchema, TopicSchema};
    use crate::store::TopicStore;

    #[test]
    fn folds_global_stats_from_chunk_stats() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "A").unwrap();
        let value = identity.add_field(topic, "v").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "A",
                [FieldSchema::new("v", DataType::Float64, None::<String>, 0.5).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![0, 1_000_000, 2_000_000]),
                vec![Arc::new(Float64Array::from(vec![Some(1.0), Some(2.0), None])) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();

        let stats = global_field_stats(&snapshot, value).unwrap().unwrap();
        assert_eq!(stats.min, 0.5);
        assert_eq!(stats.max, 1.0);
        assert_eq!(stats.mean, 0.75);
        assert_eq!(stats.stddev, 0.25);
        assert_eq!(stats.count, 2);
        assert_eq!(stats.missing_count, 1);
        assert_eq!(stats.rate_hz, Some(1.0));
    }

    #[test]
    fn visible_stats_include_window_endpoints_and_apply_multiplier() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        identity.set_source_offset_us(source, 100).unwrap();
        let topic = identity.add_topic(source, "A").unwrap();
        let value = identity.add_field(topic, "v").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "A",
                [FieldSchema::new("v", DataType::Float64, Some("m"), -2.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![0, 10, 20, 30, 40]),
                vec![Arc::new(Float64Array::from(vec![
                    Some(1.0),
                    Some(2.0),
                    Some(f64::NAN),
                    None,
                    Some(4.0),
                ])) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 7).unwrap();

        let stats = visible_field_stats(&snapshot, value, 110, 130)
            .unwrap()
            .unwrap();
        assert_eq!(stats.min, -4.0);
        assert_eq!(stats.max, -4.0);
        assert_eq!(stats.mean, -4.0);
        assert_eq!(stats.stddev, 0.0);
        assert_eq!(stats.count, 1);
        assert_eq!(stats.missing_count, 2);
        assert_eq!(stats.rate_hz, None);

        let empty = visible_field_stats(&snapshot, value, 1_000, 2_000)
            .unwrap()
            .unwrap();
        assert_eq!(empty.count, 0);
        assert_eq!(empty.missing_count, 0);
        assert!(empty.min.is_nan());
    }

    /// Build a single-`Int64`-field snapshot ("M.mode") from times + values.
    fn int_field_snapshot(times: &[i64], vals: Vec<Option<i64>>) -> (StoreSnapshot, FieldId) {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "M").unwrap();
        let field = identity.add_field(topic, "mode").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "M",
                [FieldSchema::new("mode", DataType::Int64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(times.to_vec()),
                vec![Arc::new(Int64Array::from(vals)) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();
        (snapshot, field)
    }

    #[test]
    fn transitions_mark_each_run_start_in_first_seen_order() {
        // mode: 0 0 4 4 0 4 at 0..5 s.
        let (snapshot, field) = int_field_snapshot(
            &[0, 1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000],
            vec![Some(0), Some(0), Some(4), Some(4), Some(0), Some(4)],
        );
        let groups = field_value_transitions(&snapshot, field, 64).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].value_label, "0");
        assert_eq!(groups[0].transitions, vec![0, 4_000_000]);
        assert_eq!(groups[1].value_label, "4");
        assert_eq!(groups[1].transitions, vec![2_000_000, 5_000_000]);
    }

    #[test]
    fn null_ends_a_run_and_re_entry_is_a_transition() {
        // mode: 4 null 4 at 0,1,2 s — null is a gap, not a value.
        let (snapshot, field) =
            int_field_snapshot(&[0, 1_000_000, 2_000_000], vec![Some(4), None, Some(4)]);
        let groups = field_value_transitions(&snapshot, field, 64).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].value_label, "4");
        assert_eq!(groups[0].transitions, vec![0, 2_000_000]);
    }

    #[test]
    fn too_many_distinct_values_errors() {
        let (snapshot, field) = int_field_snapshot(
            &[0, 1_000_000, 2_000_000, 3_000_000],
            vec![Some(0), Some(1), Some(2), Some(3)],
        );
        let err = field_value_transitions(&snapshot, field, 3).unwrap_err();
        assert_eq!(err, TransitionsError::TooManyValues(4));
    }

    #[test]
    fn strings_have_no_numeric_stats() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "A").unwrap();
        let text = identity.add_field(topic, "text").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "A",
                [FieldSchema::new("text", DataType::Utf8, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![0]),
                vec![Arc::new(StringArray::from(vec!["ok"])) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();

        assert!(global_field_stats(&snapshot, text).unwrap().is_none());
    }

    proptest! {
        #[test]
        fn visible_stats_matches_naive_scan(
            values in prop::collection::vec(-1_000_i64..=1_000, 1..128),
            a in 0_usize..128,
            b in 0_usize..128,
        ) {
            let times: Vec<i64> = (0..values.len()).map(|i| i as i64 * 10).collect();
            let vals = values.iter().copied().map(Some).collect();
            let (snapshot, field) = int_field_snapshot(&times, vals);
            let lo = a.min(b).min(values.len() - 1);
            let hi = a.max(b).min(values.len() - 1);
            let expected = &values[lo..=hi];
            let stats = visible_field_stats(&snapshot, field, times[lo], times[hi])
                .unwrap()
                .unwrap();
            let sum: f64 = expected.iter().map(|&v| v as f64).sum();
            let mean = sum / expected.len() as f64;
            let variance = expected
                .iter()
                .map(|&v| {
                    let d = v as f64 - mean;
                    d * d
                })
                .sum::<f64>()
                / expected.len() as f64;

            prop_assert_eq!(stats.count, expected.len() as u64);
            prop_assert_eq!(stats.min, *expected.iter().min().unwrap() as f64);
            prop_assert_eq!(stats.max, *expected.iter().max().unwrap() as f64);
            prop_assert!((stats.mean - mean).abs() < 1e-9);
            prop_assert!((stats.stddev - variance.sqrt()).abs() < 1e-7);
        }
    }
}
