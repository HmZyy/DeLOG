pub use delog_core::derived::{PreparedTopics, prepare_topics};
use delog_core::identity::SourceId;
use delog_core::ingest::IngestSink;

use crate::api::PendingTopic;

pub fn emit_topics(
    sink: &mut dyn IngestSink,
    script_name: &str,
    topics: &[PendingTopic],
) -> Result<SourceId, String> {
    delog_core::derived::emit_topics(sink, &format!("script:{script_name}"), topics)
}

pub fn emit_prepared_topics(
    sink: &mut dyn IngestSink,
    script_name: &str,
    prepared: PreparedTopics,
) -> SourceId {
    delog_core::derived::emit_prepared_topics(sink, &format!("script:{script_name}"), prepared)
}
