//! Shared construction and ingestion of derived topics.

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::DataType;

use crate::identity::SourceId;
use crate::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
use crate::schema::{FieldSchema, TopicSchema};

#[derive(Debug, Clone, PartialEq)]
pub enum PendingColumn {
    F64(Vec<f64>),
    Utf8(Vec<String>),
}

impl PendingColumn {
    pub fn len(&self) -> usize {
        match self {
            Self::F64(v) => v.len(),
            Self::Utf8(v) => v.len(),
        }
    }

}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingField {
    pub name: String,
    pub values: PendingColumn,
    pub unit: Option<String>,
}

impl PendingField {
    pub fn numeric(name: impl Into<String>, values: Vec<f64>, unit: Option<String>) -> Self {
        Self {
            name: name.into(),
            values: PendingColumn::F64(values),
            unit,
        }
    }

    pub fn utf8(name: impl Into<String>, values: Vec<String>) -> Self {
        Self {
            name: name.into(),
            values: PendingColumn::Utf8(values),
            unit: None,
        }
    }
}

/// One derived topic the script is building. Every field shares `times`.
pub struct PendingTopic {
    pub name: String,
    pub times: Vec<i64>,
    pub fields: Vec<PendingField>,
}

impl PendingTopic {
    pub fn new(name: String, times: Vec<i64>) -> Self {
        Self {
            name,
            times,
            fields: Vec::new(),
        }
    }

    pub fn add_field(&mut self, field: PendingField) -> Result<(), String> {
        if field.values.len() != self.times.len() {
            return Err(format!(
                "field '{}': {} values but topic '{}' has {} timestamps",
                field.name,
                field.values.len(),
                self.name,
                self.times.len()
            ));
        }
        self.fields.push(field);
        Ok(())
    }
}

struct PreparedTopic {
    schema: Arc<TopicSchema>,
    timestamps: Int64Array,
    columns: Vec<ArrayRef>,
}

pub struct PreparedTopics {
    topics: Vec<PreparedTopic>,
}

impl PreparedTopics {
    pub fn into_batches(self, source: SourceId) -> Vec<ParsedBatch> {
        self.topics
            .into_iter()
            .map(|topic| ParsedBatch::new(source, topic.schema, topic.timestamps, topic.columns))
            .collect()
    }
}

pub fn emit_topics(
    sink: &mut dyn IngestSink,
    source_key: &str,
    topics: &[PendingTopic],
) -> Result<SourceId, String> {
    let prepared = prepare_topics(topics)?;
    Ok(emit_prepared_topics(sink, source_key, prepared))
}

pub fn emit_prepared_topics(
    sink: &mut dyn IngestSink,
    source_key: &str,
    prepared: PreparedTopics,
) -> SourceId {
    let source = sink.open_source(source_key, SourceKind::Derived);
    for batch in prepared.into_batches(source) {
        sink.submit(batch);
    }
    sink.close_source(source, ParseSummary::default());
    source
}

pub fn prepare_topics(topics: &[PendingTopic]) -> Result<PreparedTopics, String> {
    let mut out = Vec::new();
    for topic in topics {
        if topic.fields.is_empty() {
            continue;
        }
        let fields: Result<Vec<FieldSchema>, String> = topic
            .fields
            .iter()
            .map(|f| {
                let dtype = match &f.values {
                    PendingColumn::F64(_) => DataType::Float64,
                    PendingColumn::Utf8(_) => DataType::Utf8,
                };
                FieldSchema::new(f.name.clone(), dtype, f.unit.clone(), 1.0)
                    .map_err(|e| e.to_string())
            })
            .collect();
        let schema =
            Arc::new(TopicSchema::new(topic.name.clone(), fields?).map_err(|e| e.to_string())?);
        let timestamps = Int64Array::from(topic.times.clone());
        let columns: Vec<ArrayRef> = topic
            .fields
            .iter()
            .map(|f| match &f.values {
                PendingColumn::F64(values) => {
                    Arc::new(Float64Array::from(values.clone())) as ArrayRef
                }
                PendingColumn::Utf8(values) => {
                    Arc::new(StringArray::from(values.clone())) as ArrayRef
                }
            })
            .collect();
        out.push(PreparedTopic {
            schema,
            timestamps,
            columns,
        });
    }
    Ok(PreparedTopics { topics: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_topics_preserves_numeric_and_utf8_columns() {
        let mut topic = PendingTopic::new("mixed".into(), vec![10, 20]);
        topic
            .add_field(PendingField::numeric(
                "value",
                vec![1.5, 2.5],
                Some("m".into()),
            ))
            .unwrap();
        topic
            .add_field(PendingField::utf8("name", vec!["a".into(), "b".into()]))
            .unwrap();

        let batches = prepare_topics(&[topic]).unwrap().into_batches(SourceId(7));
        assert_eq!(batches[0].schema.field(0).unwrap().dtype, DataType::Float64);
        assert_eq!(batches[0].schema.field(1).unwrap().dtype, DataType::Utf8);
        assert_eq!(
            batches[0].columns[1]
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(1),
            "b"
        );
    }
}
