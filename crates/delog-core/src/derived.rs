//! Shared construction and ingestion of derived topics.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::DataType;

use crate::identity::SourceId;
use crate::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
use crate::schema::{FieldSchema, TopicSchema};
use crate::store::TopicStore;

#[derive(Debug, Clone)]
pub struct PreparedDerivedSource {
    topics: Vec<Arc<TopicStore>>,
}

#[derive(Debug, Clone)]
pub struct DerivedCommit {
    pub key: String,
    pub provenance: DerivedProvenance,
    pub previous: Option<SourceId>,
    pub source: PreparedDerivedSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedProvenance {
    pub owner: String,
    pub logical_topic: String,
    pub generation: u64,
    pub commit_nonce: [u8; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedCommitReceipt {
    pub source: SourceId,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivedCommitError {
    EmptySource,
    EmptyKey,
    EmptyOwner,
    EmptyLogicalTopic,
    ZeroGeneration,
    EmptyTopicName,
    EmptyFieldName { topic: String },
    DuplicateTopicName(String),
    DuplicateFieldName { topic: String, field: String },
    UnsupportedDataType { topic: String, field: String },
    SourceKeyInUse(String),
    PreviousSourceNotLive(SourceId),
    PreviousSourceNotDerived(SourceId),
    PreviousSourceMismatch(SourceId),
    SourceNotLive(SourceId),
    SourceNotDerived(SourceId),
    SnapshotBuild(String),
    StorePublish(String),
}

impl PreparedDerivedSource {
    pub fn try_new(
        topics: impl IntoIterator<Item = Arc<TopicStore>>,
    ) -> Result<Self, DerivedCommitError> {
        let topics: Vec<_> = topics.into_iter().collect();
        if topics.is_empty() || topics.iter().all(|topic| topic.is_empty()) {
            return Err(DerivedCommitError::EmptySource);
        }

        let mut topic_names = HashSet::with_capacity(topics.len());
        for topic in &topics {
            let topic_name = topic.schema.name();
            if topic_name.is_empty() {
                return Err(DerivedCommitError::EmptyTopicName);
            }
            if !topic_names.insert(topic_name.to_owned()) {
                return Err(DerivedCommitError::DuplicateTopicName(
                    topic_name.to_owned(),
                ));
            }

            let mut field_names = HashSet::with_capacity(topic.schema.len());
            for field in topic.schema.fields() {
                if field.name.is_empty() {
                    return Err(DerivedCommitError::EmptyFieldName {
                        topic: topic_name.to_owned(),
                    });
                }
                if !field_names.insert(field.name.clone()) {
                    return Err(DerivedCommitError::DuplicateFieldName {
                        topic: topic_name.to_owned(),
                        field: field.name.clone(),
                    });
                }
                if !supported_derived_dtype(&field.dtype) {
                    return Err(DerivedCommitError::UnsupportedDataType {
                        topic: topic_name.to_owned(),
                        field: field.name.clone(),
                    });
                }
            }
        }

        Ok(Self { topics })
    }

    pub fn topics(&self) -> &[Arc<TopicStore>] {
        &self.topics
    }
}

impl DerivedCommit {
    pub(crate) fn validate(&self) -> Result<(), DerivedCommitError> {
        if self.key.trim().is_empty() {
            return Err(DerivedCommitError::EmptyKey);
        }
        if self.provenance.owner.trim().is_empty() {
            return Err(DerivedCommitError::EmptyOwner);
        }
        if self.provenance.logical_topic.trim().is_empty() {
            return Err(DerivedCommitError::EmptyLogicalTopic);
        }
        if self.provenance.generation == 0 {
            return Err(DerivedCommitError::ZeroGeneration);
        }
        Ok(())
    }
}

fn supported_derived_dtype(dtype: &DataType) -> bool {
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
            | DataType::Utf8
            | DataType::LargeUtf8
    )
}

impl fmt::Display for DerivedCommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySource => write!(f, "derived source must contain at least one row"),
            Self::EmptyKey => write!(f, "derived source key must not be empty"),
            Self::EmptyOwner => write!(f, "derived source owner must not be empty"),
            Self::EmptyLogicalTopic => write!(f, "derived logical topic must not be empty"),
            Self::ZeroGeneration => write!(f, "derived generation must be nonzero"),
            Self::EmptyTopicName => write!(f, "derived topic name must not be empty"),
            Self::EmptyFieldName { topic } => {
                write!(f, "derived topic `{topic}` contains an empty field name")
            }
            Self::DuplicateTopicName(topic) => {
                write!(f, "duplicate derived topic name `{topic}`")
            }
            Self::DuplicateFieldName { topic, field } => {
                write!(
                    f,
                    "duplicate field name `{field}` in derived topic `{topic}`"
                )
            }
            Self::UnsupportedDataType { topic, field } => {
                write!(f, "unsupported dtype for derived field `{topic}/{field}`")
            }
            Self::SourceKeyInUse(key) => write!(f, "derived source key `{key}` is already live"),
            Self::PreviousSourceNotLive(source) => {
                write!(f, "previous derived source {source:?} is not live")
            }
            Self::PreviousSourceNotDerived(source) => {
                write!(
                    f,
                    "previous source {source:?} is not an external derived source"
                )
            }
            Self::PreviousSourceMismatch(source) => {
                write!(
                    f,
                    "previous derived source {source:?} belongs to another publication"
                )
            }
            Self::SourceNotLive(source) => write!(f, "derived source {source:?} is not live"),
            Self::SourceNotDerived(source) => {
                write!(f, "source {source:?} is not an external derived source")
            }
            Self::SnapshotBuild(message) => {
                write!(f, "failed to build derived snapshot: {message}")
            }
            Self::StorePublish(message) => {
                write!(f, "failed to publish derived snapshot: {message}")
            }
        }
    }
}

impl Error for DerivedCommitError {}

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

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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

pub fn open_derived_source(
    sink: &mut dyn IngestSink,
    source_key: &str,
    kind: SourceKind,
) -> SourceId {
    sink.open_source(source_key, kind)
}

pub fn submit_prepared_topics(
    sink: &mut dyn IngestSink,
    source: SourceId,
    prepared: PreparedTopics,
) {
    for batch in prepared.into_batches(source) {
        sink.submit(batch);
    }
}

pub fn emit_prepared_topics(
    sink: &mut dyn IngestSink,
    source_key: &str,
    prepared: PreparedTopics,
) -> SourceId {
    let source = open_derived_source(sink, source_key, SourceKind::Derived);
    submit_prepared_topics(sink, source, prepared);
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

    #[test]
    fn submit_prepared_topics_appends_without_closing() {
        use crate::diagnostics::Diag;
        use crate::identity::SourceId;
        use crate::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};

        struct Sink {
            opened: usize,
            batches: usize,
            closed: usize,
        }
        impl IngestSink for Sink {
            fn open_source(&mut self, _key: &str, _kind: SourceKind) -> SourceId {
                self.opened += 1;
                SourceId(7)
            }
            fn submit(&mut self, _batch: ParsedBatch) {
                self.batches += 1;
            }
            fn diagnostic(&mut self, _d: Diag) {}
            fn progress(&mut self, _s: SourceId, _f: f32) {}
            fn close_source(&mut self, _s: SourceId, _summary: ParseSummary) {
                self.closed += 1;
            }
        }

        let mut topic = PendingTopic::new("derived".into(), vec![10, 20]);
        topic
            .add_field(PendingField::numeric("v", vec![1.0, 2.0], None))
            .unwrap();

        let mut sink = Sink {
            opened: 0,
            batches: 0,
            closed: 0,
        };
        let source = open_derived_source(&mut sink, "dataflow:g", SourceKind::LiveDerived);
        submit_prepared_topics(&mut sink, source, prepare_topics(&[topic]).unwrap());

        assert_eq!(sink.opened, 1);
        assert_eq!(sink.batches, 1);
        assert_eq!(sink.closed, 0);
    }
}
