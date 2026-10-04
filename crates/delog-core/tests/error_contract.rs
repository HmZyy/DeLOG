use std::error::Error;

use arrow::datatypes::DataType;
use delog_core::chunk::ChunkError;
use delog_core::export::ExportError;
use delog_core::field_view::FieldViewError;
use delog_core::identity::{FieldId, TopicId};
use delog_core::schema::SchemaError;
use delog_core::snapshot::{DataStoreError, SnapshotError};
use delog_core::store::TopicStoreError;

#[test]
fn chunk_error_keeps_debug_type_names() {
    let error = ChunkError::ColumnTypeMismatch {
        column: 0,
        expected: DataType::Int32,
        actual: DataType::Float64,
    };
    assert_eq!(
        error.to_string(),
        "column 0 type mismatch: expected Int32, got Float64"
    );
}

#[test]
fn nested_export_error_keeps_message_without_adding_a_source() {
    let error = ExportError::Field(FieldId(3), FieldViewError::MissingSource);
    assert_eq!(
        error.to_string(),
        "field FieldId(3): missing source for field topic"
    );
    assert!(error.source().is_none());
}

#[test]
fn field_view_error_keeps_message() {
    assert_eq!(
        FieldViewError::MissingSource.to_string(),
        "missing source for field topic"
    );
}

#[test]
fn schema_error_keeps_quoted_field_name() {
    assert_eq!(
        SchemaError::DuplicateFieldName("roll".into()).to_string(),
        "duplicate field name `roll`"
    );
}

#[test]
fn snapshot_error_keeps_topic_context() {
    let error = SnapshotError::TopicStoreSchemaMismatch {
        topic: TopicId(4),
        expected: "a".into(),
        actual: "b".into(),
    };
    assert_eq!(
        error.to_string(),
        "topic TopicId(4) schema mismatch: expected `a`, got `b`"
    );
}

#[test]
fn data_store_error_stays_copy_and_keeps_message() {
    let error = DataStoreError::EpochOverflow;
    let copied = error;
    assert_eq!(error, copied);
    assert_eq!(error.to_string(), "store epoch overflow");
}

#[test]
fn topic_store_error_keeps_message() {
    assert_eq!(
        TopicStoreError::RowCountOverflow.to_string(),
        "topic row count overflow"
    );
}
