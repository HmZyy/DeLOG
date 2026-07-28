use std::sync::Arc;

use arrow::array::{Array, BooleanArray, Int64Array};
use delog_core::identity::SourceMetadata;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch};
use delog_core::parse_ctl::ParseCtl;
use delog_core::schema::{FieldSchema, TopicProvenance, TopicSchema};
use delog_core::time::TimeRange;
use delog_parquet_format::{ValidatedManifest, ValidatedTopic, resolved_topic_names};
use parquet::arrow::arrow_reader::{ArrowReaderMetadata, ParquetRecordBatchReaderBuilder};

use super::{
    PARQUET_BATCH_ROWS, SeekChunkReader, cancellation_error, parse_arrow_error,
};
use crate::parser::ParseError;

struct TopicState {
    schema: Arc<TopicSchema>,
    timestamp_column: usize,
    value_columns: Vec<usize>,
    emitted_rows: u64,
    emitted_any: bool,
}

pub(super) fn parse(
    reader: SeekChunkReader,
    metadata: ArrowReaderMetadata,
    manifest: ValidatedManifest,
    sink: &mut dyn IngestSink,
    ctl: &ParseCtl,
) -> Result<ParseSummary, ParseError> {
    let mut topics = topic_states(metadata.schema().as_ref(), &manifest)?;
    let total_rows = metadata.metadata().file_metadata().num_rows().max(0) as u64;
    let row_group_count = metadata.metadata().num_row_groups();
    let mut processed_rows = 0_u64;
    let mut time_range: Option<TimeRange> = None;
    let mut emitted_any = false;

    let parse_result = (|| {
        if ctl.is_cancelled() {
            return Err(cancellation_error(emitted_any));
        }

        for row_group in 0..row_group_count {
            let mut batch_reader = ParquetRecordBatchReaderBuilder::new_with_metadata(
                reader.clone(),
                metadata.clone(),
            )
            .with_batch_size(PARQUET_BATCH_ROWS)
            .with_row_groups(vec![row_group])
            .build()
            .map_err(|error| parse_arrow_error(error, emitted_any))?;

            for batch in &mut batch_reader {
                let batch = batch.map_err(|error| parse_arrow_error(error, emitted_any))?;
                processed_rows += batch.num_rows() as u64;

                for topic in &mut topics {
                    let timestamps = batch
                        .column(topic.timestamp_column)
                        .as_any()
                        .downcast_ref::<Int64Array>()
                        .expect("validated manifest guarantees nullable Int64 timestamps");
                    let keep = BooleanArray::from_iter(
                        (0..timestamps.len()).map(|row| timestamps.is_valid(row)),
                    );
                    let filtered_timestamps = arrow::compute::filter(timestamps, &keep)
                        .map_err(|error| parse_arrow_error(error, emitted_any))?;
                    let filtered_timestamps = filtered_timestamps
                        .as_any()
                        .downcast_ref::<Int64Array>()
                        .expect("filter preserves Int64 timestamps")
                        .clone();
                    if filtered_timestamps.is_empty() {
                        continue;
                    }

                    let columns = topic
                        .value_columns
                        .iter()
                        .map(|&column| arrow::compute::filter(batch.column(column).as_ref(), &keep))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| parse_arrow_error(error, emitted_any))?;

                    let first = filtered_timestamps.value(0);
                    let last = filtered_timestamps.value(filtered_timestamps.len() - 1);
                    let batch_range = TimeRange::new(first.min(last), first.max(last))
                        .expect("min <= max by construction");
                    time_range = Some(match time_range {
                        Some(range) => range.union(batch_range),
                        None => batch_range,
                    });
                    topic.emitted_rows += filtered_timestamps.len() as u64;
                    topic.emitted_any = true;
                    sink.submit(ParsedBatch::new(
                        ctl.source(),
                        Arc::clone(&topic.schema),
                        filtered_timestamps,
                        columns,
                    ));
                    emitted_any = true;
                    if ctl.is_cancelled() {
                        return Err(cancellation_error(emitted_any));
                    }
                }

                ctl.report_fraction(sink, processed_rows as f32 / total_rows.max(1) as f32);
                if ctl.is_cancelled() {
                    return Err(cancellation_error(emitted_any));
                }
            }
        }
        Ok(())
    })();

    let summary = ParseSummary {
        topic_count: topics.iter().filter(|topic| topic.emitted_any).count() as u64,
        row_count: topics.iter().map(|topic| topic.emitted_rows).sum(),
        time_range,
        diagnostics: 0,
        source_meta: SourceMetadata::default(),
    };
    match parse_result {
        Ok(()) => {
            sink.close_source(ctl.source(), summary.clone());
            Ok(summary)
        }
        Err(error) => {
            if emitted_any {
                sink.close_source(ctl.source(), summary);
            }
            Err(error)
        }
    }
}

fn topic_states(
    physical_schema: &arrow::datatypes::Schema,
    manifest: &ValidatedManifest,
) -> Result<Vec<TopicState>, ParseError> {
    let names = resolved_topic_names(manifest);
    manifest
        .topics
        .iter()
        .zip(names)
        .map(|(topic, name)| topic_state(physical_schema, topic, name))
        .collect()
}

fn topic_state(
    physical_schema: &arrow::datatypes::Schema,
    topic: &ValidatedTopic,
    name: String,
) -> Result<TopicState, ParseError> {
    let fields = topic
        .fields
        .iter()
        .map(|field| -> Result<FieldSchema, ParseError> {
            let physical = physical_schema.field(field.column);
            let mut schema = FieldSchema::new(
                field.name.clone(),
                physical.data_type().clone(),
                field.unit.clone(),
                field.multiplier,
            )
            .map_err(|error| ParseError::Setup {
                detail: format!(
                    "invalid field `{}` for structured topic `{name}`: {error}",
                    field.name
                ),
            })?;
            if let Some(description) = &field.description {
                schema = schema.with_description(description);
            }
            Ok(schema)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let provenance =
        TopicProvenance::new(&topic.original_source, &topic.original_topic).map_err(|error| {
            ParseError::Setup {
                detail: format!("invalid provenance for structured topic `{name}`: {error}"),
            }
        })?;
    let schema = TopicSchema::new(name, fields)
        .map_err(|error| ParseError::Setup {
            detail: format!("invalid structured topic schema: {error}"),
        })?
        .with_provenance(provenance);

    Ok(TopicState {
        schema: Arc::new(schema),
        timestamp_column: topic.timestamp_column,
        value_columns: topic.fields.iter().map(|field| field.column).collect(),
        emitted_rows: 0,
        emitted_any: false,
    })
}
