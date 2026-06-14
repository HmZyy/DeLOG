//! Live Python transform support: same-topic batch transforms for scripts.

use std::collections::HashMap;

use delog_core::field_view::array_row_as_f64;
use delog_core::ingest::ParsedBatch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTransformSpec {
    pub script_name: String,
    pub generation: u64,
    pub topic: String,
    pub fields: Vec<String>,
    pub output_topic: String,
}

impl LiveTransformSpec {
    pub fn new(
        script_name: String,
        generation: u64,
        topic: String,
        fields: Vec<String>,
        output_topic: String,
    ) -> Result<Self, String> {
        if topic.is_empty() {
            return Err("live_transform topic must not be empty".into());
        }
        if fields.is_empty() {
            return Err("live_transform fields must not be empty".into());
        }
        if output_topic.is_empty() {
            return Err("live_transform output_topic must not be empty".into());
        }
        Ok(Self {
            script_name,
            generation,
            topic,
            fields,
            output_topic,
        })
    }

    pub fn matches(&self, batch: &ParsedBatch) -> bool {
        batch.topic() == self.topic
            && self
                .fields
                .iter()
                .all(|field| batch.schema.field_index(field).is_some())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveTransformBatch {
    pub times: Vec<i64>,
    pub values: HashMap<String, Vec<f64>>,
}

impl LiveTransformBatch {
    // ZC-EXCEPTION: script live materialization copies the incoming live batch
    // into contiguous f64 buffers for CPython/numpy; off render hot path.
    pub fn from_parsed(spec: &LiveTransformSpec, batch: &ParsedBatch) -> Result<Self, String> {
        if !spec.matches(batch) {
            return Err(format!(
                "batch topic '{}' does not satisfy live transform '{}'",
                batch.topic(),
                spec.output_topic
            ));
        }
        let times: Vec<i64> = (0..batch.timestamps.len())
            .map(|row| batch.timestamps.value(row))
            .collect();
        let mut values = HashMap::new();
        for field in &spec.fields {
            let idx = batch
                .schema
                .field_index(field)
                .ok_or_else(|| format!("field '{field}' missing from {}", batch.topic()))?;
            let col = batch.columns[idx].as_ref();
            let vals = (0..batch.timestamps.len())
                .map(|row| array_row_as_f64(col, row))
                .collect();
            values.insert(field.clone(), vals);
        }
        Ok(Self { times, values })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float32Array, Int16Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::identity::SourceId;
    use delog_core::ingest::ParsedBatch;
    use delog_core::schema::{FieldSchema, TopicSchema};

    use super::*;

    fn nav_batch() -> ParsedBatch {
        let schema = Arc::new(
            TopicSchema::new(
                "NAV_CONTROLLER_OUTPUT",
                [
                    FieldSchema::new("nav_roll", DataType::Float32, Some("deg"), 1.0).unwrap(),
                    FieldSchema::new("nav_pitch", DataType::Float32, Some("deg"), 1.0).unwrap(),
                    FieldSchema::new("nav_bearing", DataType::Int16, Some("deg"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let columns: Vec<ArrayRef> = vec![
            Arc::new(Float32Array::from(vec![0.0, 90.0])),
            Arc::new(Float32Array::from(vec![45.0, -45.0])),
            Arc::new(Int16Array::from(vec![180, -90])),
        ];
        ParsedBatch::new(
            SourceId(7),
            schema,
            Int64Array::from(vec![100, 200]),
            columns,
        )
    }

    #[test]
    fn spec_matches_topic_and_required_fields() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAV_CONTROLLER_OUTPUT".into(),
            vec!["nav_roll".into(), "nav_bearing".into()],
            "NAV_RAD".into(),
        )
        .unwrap();

        assert!(spec.matches(&nav_batch()));
    }

    #[test]
    fn spec_rejects_missing_required_fields() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAV_CONTROLLER_OUTPUT".into(),
            vec!["missing".into()],
            "NAV_RAD".into(),
        )
        .unwrap();

        assert!(!spec.matches(&nav_batch()));
    }

    #[test]
    fn materialize_batch_widens_numeric_fields_to_f64() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAV_CONTROLLER_OUTPUT".into(),
            vec!["nav_roll".into(), "nav_bearing".into()],
            "NAV_RAD".into(),
        )
        .unwrap();

        let materialized = LiveTransformBatch::from_parsed(&spec, &nav_batch()).unwrap();

        assert_eq!(materialized.times, vec![100, 200]);
        assert_eq!(materialized.values["nav_roll"], vec![0.0, 90.0]);
        assert_eq!(materialized.values["nav_bearing"], vec![180.0, -90.0]);
    }
}
