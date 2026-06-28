use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
use delog_core::schema::{FieldSchema, TopicSchema};

use crate::api::PendingTopic;

pub fn emit_topics(
    sink: &mut dyn IngestSink,
    script_name: &str,
    topics: &[PendingTopic],
) -> Result<SourceId, String> {
    let key = format!("script:{script_name}");
    let source = sink.open_source(&key, SourceKind::Derived);
    for topic in topics {
        if topic.fields.is_empty() {
            continue;
        }
        let fields: Result<Vec<FieldSchema>, String> = topic
            .fields
            .iter()
            .map(|f| {
                FieldSchema::new(f.name.clone(), DataType::Float64, f.unit.clone(), 1.0)
                    .map_err(|e| e.to_string())
            })
            .collect();
        let schema =
            Arc::new(TopicSchema::new(topic.name.clone(), fields?).map_err(|e| e.to_string())?);
        let timestamps = Int64Array::from(topic.times.clone());
        let columns: Vec<ArrayRef> = topic
            .fields
            .iter()
            .map(|f| Arc::new(Float64Array::from(f.values.clone())) as ArrayRef)
            .collect();
        sink.submit(ParsedBatch::new(source, schema, timestamps, columns));
    }
    sink.close_source(source, ParseSummary::default());
    Ok(source)
}
