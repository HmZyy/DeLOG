use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::DataType;
use delog_core::derived::{PendingColumn, PendingField};
use delog_core::field_view::{array_row_as_f64, array_row_as_str};
use delog_core::identity::SourceId;
use delog_core::ingest::ParsedBatch;
use delog_core::schema::{FieldSchema, TopicSchema};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTransformSpec {
    pub script_name: String,
    pub func_name: String,
    pub generation: u64,
    pub topic: String,
    pub fields: Vec<String>,
    pub output_topic: Option<String>,
}

impl LiveTransformSpec {
    pub fn new(
        script_name: String,
        generation: u64,
        topic: String,
        fields: Vec<String>,
        output_topic: Option<String>,
    ) -> Result<Self> {
        if topic.is_empty() {
            return Err(Error::invalid_input(
                "live_transform topic must not be empty",
            ));
        }
        if fields.is_empty() {
            return Err(Error::invalid_input(
                "live_transform fields must not be empty",
            ));
        }
        if output_topic.as_deref() == Some("") {
            return Err(Error::invalid_input(
                "live_transform output_topic must not be empty",
            ));
        }
        Ok(Self {
            script_name,
            func_name: String::new(),
            generation,
            topic,
            fields,
            output_topic,
        })
    }

    pub fn label(&self) -> String {
        format!("{}.{}", self.script_name, self.func_name)
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
pub enum LiveColumn {
    F64(Vec<f64>),
    Str(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveTransformBatch {
    pub times: Vec<i64>,
    pub values: HashMap<String, LiveColumn>,
}

impl LiveTransformBatch {
    pub fn from_parsed(spec: &LiveTransformSpec, batch: &ParsedBatch) -> Result<Self> {
        if !spec.matches(batch) {
            return Err(Error::invalid_input(format!(
                "batch topic '{}' does not satisfy live transform '{}'",
                batch.topic(),
                spec.label()
            )));
        }
        let times = (0..batch.timestamps.len())
            .map(|row| batch.timestamps.value(row))
            .collect();
        let mut values = HashMap::new();
        for field in &spec.fields {
            let index = batch.schema.field_index(field).ok_or_else(|| {
                Error::not_found(format!("field '{field}' missing from {}", batch.topic()))
            })?;
            let column = batch.columns[index].as_ref();
            let live_column = match column.data_type() {
                DataType::Utf8 | DataType::LargeUtf8 => LiveColumn::Str(
                    (0..batch.timestamps.len())
                        .map(|row| array_row_as_str(column, row).unwrap_or_default().to_owned())
                        .collect(),
                ),
                _ => LiveColumn::F64(
                    (0..batch.timestamps.len())
                        .map(|row| array_row_as_f64(column, row))
                        .collect(),
                ),
            };
            values.insert(field.clone(), live_column);
        }
        Ok(Self { times, values })
    }
}

pub struct LiveTransformResult {
    pub topic: String,
    pub times: Vec<i64>,
    pub fields: Vec<PendingField>,
}

pub fn result_to_batch(source: SourceId, result: LiveTransformResult) -> Result<ParsedBatch> {
    let fields = result
        .fields
        .iter()
        .map(|field| {
            let dtype = match &field.values {
                PendingColumn::F64(_) => DataType::Float64,
                PendingColumn::Utf8(_) => DataType::Utf8,
            };
            FieldSchema::new(field.name.clone(), dtype, field.unit.clone(), 1.0)
                .map_err(|error| Error::invalid_input(error.to_string()))
        })
        .collect::<Result<Vec<_>>>()?;
    let schema = Arc::new(
        TopicSchema::new(result.topic, fields)
            .map_err(|error| Error::invalid_input(error.to_string()))?,
    );
    let timestamps = Int64Array::from(result.times);
    let columns: Vec<ArrayRef> = result
        .fields
        .into_iter()
        .map(|field| match field.values {
            PendingColumn::F64(values) => Arc::new(Float64Array::from(values)) as ArrayRef,
            PendingColumn::Utf8(values) => Arc::new(StringArray::from(values)) as ArrayRef,
        })
        .collect();
    Ok(ParsedBatch::new(source, schema, timestamps, columns))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float32Array, Int16Array, Int64Array, StringArray};
    use arrow::datatypes::DataType;
    use delog_core::derived::{PendingColumn, PendingField};
    use delog_core::identity::SourceId;
    use delog_core::ingest::ParsedBatch;
    use delog_core::schema::{FieldSchema, TopicSchema};

    use super::{
        LiveColumn, LiveTransformBatch, LiveTransformResult, LiveTransformSpec, result_to_batch,
    };

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

    fn named_batch() -> ParsedBatch {
        let schema = Arc::new(
            TopicSchema::new(
                "NAMED_VALUE_FLOAT",
                [
                    FieldSchema::new("name", DataType::Utf8, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("value", DataType::Float32, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(vec![
                Some("airspd"),
                None,
                Some("clbrate"),
            ])),
            Arc::new(Float32Array::from(vec![1.5, 2.5, 3.5])),
        ];
        ParsedBatch::new(
            SourceId(7),
            schema,
            Int64Array::from(vec![100, 200, 300]),
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
            Some("NAV_RAD".into()),
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
            Some("NAV_RAD".into()),
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
            Some("NAV_RAD".into()),
        )
        .unwrap();

        let materialized = LiveTransformBatch::from_parsed(&spec, &nav_batch()).unwrap();

        assert_eq!(materialized.times, vec![100, 200]);
        assert_eq!(
            materialized.values["nav_roll"],
            LiveColumn::F64(vec![0.0, 90.0])
        );
        assert_eq!(
            materialized.values["nav_bearing"],
            LiveColumn::F64(vec![180.0, -90.0])
        );
    }

    #[test]
    fn spec_accepts_dynamic_mode_and_labels_by_function() {
        let mut spec = LiveTransformSpec::new(
            "named_values".into(),
            1,
            "NAMED_VALUE_FLOAT".into(),
            vec!["name".into(), "value".into()],
            None,
        )
        .unwrap();
        spec.func_name = "split_floats".into();
        assert_eq!(spec.output_topic, None);
        assert_eq!(spec.label(), "named_values.split_floats");

        assert!(
            LiveTransformSpec::new(
                "s".into(),
                1,
                "T".into(),
                vec!["v".into()],
                Some(String::new())
            )
            .is_err()
        );
    }

    #[test]
    fn materialize_batch_extracts_string_fields_with_empty_for_null() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAMED_VALUE_FLOAT".into(),
            vec!["name".into(), "value".into()],
            Some("OUT".into()),
        )
        .unwrap();

        let materialized = LiveTransformBatch::from_parsed(&spec, &named_batch()).unwrap();

        assert_eq!(
            materialized.values["name"],
            LiveColumn::Str(vec!["airspd".into(), "".into(), "clbrate".into()])
        );
        assert_eq!(
            materialized.values["value"],
            LiveColumn::F64(vec![1.5, 2.5, 3.5])
        );
    }

    #[test]
    fn result_conversion_preserves_field_order_types_and_units() {
        let batch = result_to_batch(
            SourceId(9),
            LiveTransformResult {
                topic: "OUT".into(),
                times: vec![100, 200],
                fields: vec![
                    PendingField::numeric("value", vec![1.0, 2.0], Some("m".into())),
                    PendingField {
                        name: "label".into(),
                        values: PendingColumn::Utf8(vec!["a".into(), "b".into()]),
                        unit: None,
                    },
                ],
            },
        )
        .unwrap();

        assert_eq!(batch.source, SourceId(9));
        assert_eq!(batch.topic(), "OUT");
        assert_eq!(batch.schema.fields()[0].name, "value");
        assert_eq!(batch.schema.fields()[0].dtype, DataType::Float64);
        assert_eq!(batch.schema.fields()[0].unit.as_deref(), Some("m"));
        assert_eq!(batch.schema.fields()[1].name, "label");
        assert_eq!(batch.schema.fields()[1].dtype, DataType::Utf8);
        assert_eq!(batch.timestamps.values(), &[100, 200]);
    }
}
