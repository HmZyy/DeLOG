use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
use delog_core::schema::{FieldSchema, TopicSchema};

use crate::api::PendingTopic;

struct PreparedTopic {
    schema: Arc<TopicSchema>,
    timestamps: Int64Array,
    columns: Vec<ArrayRef>,
}

pub struct PreparedTopics {
    topics: Vec<PreparedTopic>,
}

pub fn emit_topics(
    sink: &mut dyn IngestSink,
    script_name: &str,
    topics: &[PendingTopic],
) -> Result<SourceId, String> {
    let prepared = prepare_topics(topics)?;
    Ok(emit_prepared_topics(sink, script_name, prepared))
}

pub fn emit_prepared_topics(
    sink: &mut dyn IngestSink,
    script_name: &str,
    prepared: PreparedTopics,
) -> SourceId {
    let key = format!("script:{script_name}");
    let source = sink.open_source(&key, SourceKind::Derived);
    for topic in prepared.topics {
        let PreparedTopic {
            schema,
            timestamps,
            columns,
        } = topic;
        sink.submit(ParsedBatch::new(source, schema, timestamps, columns));
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
        out.push(PreparedTopic {
            schema,
            timestamps,
            columns,
        });
    }
    Ok(PreparedTopics { topics: out })
}
