//! Analysis helpers over immutable snapshots (PLAN.md §17).

use arrow::datatypes::DataType;

use crate::field_view::FieldViewError;
use crate::identity::FieldId;
use crate::snapshot::StoreSnapshot;

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
    let dtype = &store.schema.fields()[col_index].dtype;
    if !is_numeric(dtype) {
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
    Ok(Some(GlobalFieldStats {
        min,
        max,
        mean,
        stddev: variance.sqrt(),
        count,
        missing_count,
        rate_hz,
    }))
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
                [FieldSchema::new("v", DataType::Float64, None::<String>, 1.0).unwrap()],
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
        assert_eq!(stats.min, 1.0);
        assert_eq!(stats.max, 2.0);
        assert_eq!(stats.mean, 1.5);
        assert_eq!(stats.stddev, 0.5);
        assert_eq!(stats.count, 2);
        assert_eq!(stats.missing_count, 1);
        assert_eq!(stats.rate_hz, Some(1.0));
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
}
