use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Int64Array, new_null_array};
use arrow::compute::concat;
use arrow::datatypes::{DataType, Field, SchemaRef};
use arrow::record_batch::RecordBatch;
use delog_core::identity::TopicId;
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;
use delog_core::time::TimeRange;
use delog_parquet_format::{
    FIELD_DESCRIPTION_KEY, FIELD_MULTIPLIER_KEY, FIELD_UNIT_KEY, FORMAT_VERSION, FieldManifest,
    Manifest, TopicManifest, encode_schema, resolve_topic_instances,
};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use crate::data_export::{
    DataExportError, EXPORT_BATCH_ROWS, ExportCtl, ExportField, window_fraction,
};

struct TopicExportCursor<'a> {
    source_label: String,
    topic_name: String,
    store: &'a TopicStore,
    selected_columns: Vec<usize>,
    source_offset_us: i64,
    window: TimeRange,
    chunk_index: usize,
    row_index: usize,
}

struct TopicStripe {
    timestamps: Int64Array,
    columns: Vec<ArrayRef>,
}

/// A physical column whose final name waits on the instance-qualified topic
/// labels, which are only known once every topic has been collected.
struct PlannedColumn {
    topic_ix: usize,
    leaf: String,
    dtype: DataType,
    metadata: HashMap<String, String>,
}

const TIMESTAMP_LEAF: &str = "t_us";

impl TopicExportCursor<'_> {
    fn next_stripe(&mut self, max_rows: usize) -> Result<Option<TopicStripe>, DataExportError> {
        let mut timestamps = Vec::with_capacity(max_rows);
        let mut slices = (0..self.selected_columns.len())
            .map(|_| Vec::<ArrayRef>::new())
            .collect::<Vec<_>>();

        while timestamps.len() < max_rows && self.chunk_index < self.store.chunks.len() {
            let chunk = &self.store.chunks[self.chunk_index];
            if self.row_index == chunk.len() {
                self.chunk_index += 1;
                self.row_index = 0;
                continue;
            }

            let raw_us = chunk.t.value(self.row_index);
            let effective_us = raw_us.checked_add(self.source_offset_us).ok_or_else(|| {
                DataExportError::TimestampOverflow {
                    source: format!("{} / {}", self.source_label, self.topic_name),
                    timestamp_us: raw_us,
                    offset_us: self.source_offset_us,
                }
            })?;
            if !self.window.contains(effective_us) {
                self.row_index += 1;
                continue;
            }

            let run_start = self.row_index;
            let mut run_len = 0;
            while self.row_index < chunk.len() && timestamps.len() < max_rows {
                let raw_us = chunk.t.value(self.row_index);
                let effective_us = raw_us.checked_add(self.source_offset_us).ok_or_else(|| {
                    DataExportError::TimestampOverflow {
                        source: format!("{} / {}", self.source_label, self.topic_name),
                        timestamp_us: raw_us,
                        offset_us: self.source_offset_us,
                    }
                })?;
                if !self.window.contains(effective_us) {
                    break;
                }
                timestamps.push(effective_us);
                self.row_index += 1;
                run_len += 1;
            }

            for (destination, column) in slices.iter_mut().zip(&self.selected_columns) {
                destination.push(chunk.cols[*column].slice(run_start, run_len));
            }
        }

        if timestamps.is_empty() {
            return Ok(None);
        }

        let columns = slices
            .into_iter()
            .map(concat_slices)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(TopicStripe {
            timestamps: Int64Array::from(timestamps),
            columns,
        }))
    }
}

fn concat_slices(slices: Vec<ArrayRef>) -> Result<ArrayRef, arrow::error::ArrowError> {
    if slices.len() == 1 {
        return Ok(slices.into_iter().next().expect("one slice"));
    }
    let references = slices
        .iter()
        .map(|array| array.as_ref())
        .collect::<Vec<_>>();
    concat(&references)
}

fn pad_array(
    array: ArrayRef,
    data_type: &DataType,
    len: usize,
) -> Result<ArrayRef, DataExportError> {
    if array.len() == len {
        return Ok(array);
    }
    let padding = new_null_array(data_type, len - array.len());
    Ok(concat(&[array.as_ref(), padding.as_ref()])?)
}

pub fn write_structured_parquet<W: Write + Send>(
    writer: W,
    snapshot: &StoreSnapshot,
    fields: &[ExportField],
    window: (i64, i64),
    ctl: &ExportCtl,
) -> Result<u64, DataExportError> {
    let window = TimeRange::new(window.0, window.1).ok_or_else(|| {
        DataExportError::InvalidSelection(format!(
            "time range is inverted: {} > {}",
            window.0, window.1
        ))
    })?;
    let groups = group_fields(fields);
    let mut cursors = Vec::with_capacity(groups.len());
    let mut planned = Vec::<PlannedColumn>::new();
    let mut manifest_topics = Vec::with_capacity(groups.len());

    for (topic_ix, group) in groups.into_iter().enumerate() {
        let first = group[0];
        let source = snapshot
            .source(first.source_id)
            .filter(|source| !source.entry.removed)
            .ok_or_else(|| invalid_field(first, "source is stale"))?;
        let topic = snapshot
            .topic(first.topic_id)
            .filter(|topic| !topic.entry.removed)
            .ok_or_else(|| invalid_field(first, "topic is stale"))?;
        if topic.entry.source != first.source_id {
            return Err(invalid_field(first, "topic belongs to a different source"));
        }
        if source.entry.label != first.source || topic.entry.name != first.topic {
            return Err(invalid_field(first, "source or topic label is stale"));
        }
        let store = topic
            .store
            .as_deref()
            .ok_or_else(|| invalid_field(first, "topic store is unavailable"))?;
        if !store.is_monotonic() {
            return Err(invalid_field(
                first,
                "topic timestamps are non-monotonic across chunks",
            ));
        }

        let timestamp_column = u32::try_from(planned.len())
            .map_err(|_| invalid_field(first, "too many physical columns"))?;
        planned.push(PlannedColumn {
            topic_ix,
            leaf: TIMESTAMP_LEAF.to_owned(),
            dtype: DataType::Int64,
            metadata: HashMap::new(),
        });

        let mut selected_columns = Vec::with_capacity(group.len());
        let mut field_manifest = Vec::with_capacity(group.len());
        for field in group {
            validate_field(snapshot, store, first, field)?;
            let column_ix = store
                .schema
                .fields()
                .iter()
                .position(|schema_field| schema_field.name == field.name)
                .expect("validated field exists in schema");
            let column = u32::try_from(planned.len())
                .map_err(|_| invalid_field(field, "too many physical columns"))?;
            planned.push(PlannedColumn {
                topic_ix,
                leaf: field.name.clone(),
                dtype: field.dtype.clone(),
                metadata: field_metadata(field),
            });
            selected_columns.push(column_ix);
            field_manifest.push(FieldManifest {
                column,
                name: field.name.clone(),
                unit: field.unit.clone(),
                multiplier: field.multiplier,
                description: field.description.clone(),
            });
        }

        let (source_label, topic_name) = store
            .schema
            .provenance()
            .map(|provenance| {
                (
                    provenance.original_source().to_owned(),
                    provenance.original_topic().to_owned(),
                )
            })
            .unwrap_or_else(|| (source.entry.label.clone(), topic.entry.name.clone()));
        let topic_id =
            u32::try_from(topic_ix).map_err(|_| invalid_field(first, "too many topics"))?;
        manifest_topics.push(TopicManifest {
            id: topic_id,
            original_source: source_label.clone(),
            original_topic: topic_name.clone(),
            timestamp_column,
            fields: field_manifest,
        });
        cursors.push(TopicExportCursor {
            source_label,
            topic_name,
            store,
            selected_columns,
            source_offset_us: source.entry.offset_us,
            window,
            chunk_index: 0,
            row_index: 0,
        });
    }

    let manifest = Manifest {
        version: FORMAT_VERSION,
        topics: manifest_topics,
    };
    let schema: SchemaRef = Arc::new(encode_schema(named_columns(planned, &manifest), &manifest)?);
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .set_max_row_group_row_count(Some(EXPORT_BATCH_ROWS))
        .build();
    let mut writer = ArrowWriter::try_new(writer, Arc::clone(&schema), Some(properties))?;
    let mut logical_rows = 0_u64;
    // Tracked per topic: topics advance independently, so only the slowest one
    // describes how much of the window is written.
    let mut topic_progress = vec![0.0_f32; manifest.topics.len()];

    loop {
        if ctl.is_cancelled() {
            return Err(DataExportError::Cancelled);
        }
        let stripes = cursors
            .iter_mut()
            .map(|cursor| cursor.next_stripe(EXPORT_BATCH_ROWS))
            .collect::<Result<Vec<_>, _>>()?;
        let stripe_len = stripes
            .iter()
            .filter_map(|stripe| stripe.as_ref().map(|stripe| stripe.timestamps.len()))
            .max()
            .unwrap_or(0);
        if stripe_len == 0 {
            break;
        }

        let mut columns = Vec::with_capacity(schema.fields().len());
        for (topic_ix, (stripe, topic)) in stripes.into_iter().zip(&manifest.topics).enumerate() {
            let logical_len = stripe
                .as_ref()
                .map(|stripe| stripe.timestamps.len())
                .unwrap_or(0);
            topic_progress[topic_ix] = stripe
                .as_ref()
                .and_then(|stripe| {
                    let last_row = stripe.timestamps.len().checked_sub(1)?;
                    Some(window_fraction(
                        stripe.timestamps.value(last_row),
                        (window.min_us, window.max_us),
                    ))
                })
                .unwrap_or(1.0);
            logical_rows = logical_rows
                .checked_add(logical_len as u64)
                .ok_or_else(|| {
                    DataExportError::InvalidSelection("logical row count overflow".into())
                })?;
            match stripe {
                Some(stripe) => {
                    columns.push(pad_array(
                        Arc::new(stripe.timestamps),
                        &DataType::Int64,
                        stripe_len,
                    )?);
                    for (array, field) in stripe.columns.into_iter().zip(&topic.fields) {
                        columns.push(pad_array(
                            array,
                            schema.field(field.column as usize).data_type(),
                            stripe_len,
                        )?);
                    }
                }
                None => {
                    columns.push(new_null_array(&DataType::Int64, stripe_len));
                    for field in &topic.fields {
                        columns.push(new_null_array(
                            schema.field(field.column as usize).data_type(),
                            stripe_len,
                        ));
                    }
                }
            }
        }
        writer.write(&RecordBatch::try_new(Arc::clone(&schema), columns)?)?;
        writer.flush()?;
        if let Some(slowest) = topic_progress.iter().copied().reduce(f32::min) {
            ctl.report_fraction(slowest);
        }
    }

    writer.finish()?;
    writer.sync()?;
    ctl.report_fraction(1.0);
    Ok(logical_rows)
}

fn field_metadata(field: &ExportField) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    if let Some(unit) = field.unit.as_deref().filter(|unit| !unit.is_empty()) {
        metadata.insert(FIELD_UNIT_KEY.to_owned(), unit.to_owned());
    }
    if let Some(description) = field
        .description
        .as_deref()
        .filter(|description| !description.is_empty())
    {
        metadata.insert(FIELD_DESCRIPTION_KEY.to_owned(), description.to_owned());
    }
    if field.multiplier != 1.0 {
        metadata.insert(
            FIELD_MULTIPLIER_KEY.to_owned(),
            field.multiplier.to_string(),
        );
    }
    metadata
}

fn named_columns(planned: Vec<PlannedColumn>, manifest: &Manifest) -> Vec<Field> {
    let labels = resolve_topic_instances(
        manifest
            .topics
            .iter()
            .map(|topic| topic.original_topic.as_str()),
    );
    let mut used = HashSet::new();
    planned
        .into_iter()
        .map(|column| {
            let name = unique_name(
                format!("{}.{}", labels[column.topic_ix], column.leaf),
                &mut used,
            );
            Field::new(name, column.dtype, true).with_metadata(column.metadata)
        })
        .collect()
}

fn unique_name(base: String, used: &mut HashSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    (2_usize..)
        .map(|suffix| format!("{base}_{suffix}"))
        .find(|candidate| used.insert(candidate.clone()))
        .expect("an unbounded suffix range always yields a free name")
}

fn group_fields(fields: &[ExportField]) -> Vec<Vec<&ExportField>> {
    let mut by_topic = HashMap::<TopicId, usize>::new();
    let mut groups = Vec::<Vec<&ExportField>>::new();
    for field in fields {
        let group_ix = *by_topic.entry(field.topic_id).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[group_ix].push(field);
    }
    groups
}

fn validate_field(
    snapshot: &StoreSnapshot,
    store: &TopicStore,
    first: &ExportField,
    field: &ExportField,
) -> Result<(), DataExportError> {
    if field.source_id != first.source_id
        || field.topic_id != first.topic_id
        || field.source != first.source
        || field.topic != first.topic
    {
        return Err(invalid_field(field, "mixed schema ownership"));
    }
    if !snapshot.is_field_live(field.id) {
        return Err(invalid_field(field, "field is stale"));
    }
    let entry = snapshot
        .fields
        .get(field.id.index())
        .filter(|entry| entry.id == field.id)
        .ok_or_else(|| invalid_field(field, "field ID is stale"))?;
    if entry.topic != field.topic_id || entry.name != field.name {
        return Err(invalid_field(field, "field schema ownership is stale"));
    }
    let schema_field = store
        .schema
        .fields()
        .iter()
        .find(|schema_field| schema_field.name == field.name)
        .ok_or_else(|| invalid_field(field, "field is absent from its topic schema"))?;
    if schema_field.dtype != field.dtype
        || schema_field.unit != field.unit
        || schema_field.multiplier != field.multiplier
        || schema_field.description != field.description
    {
        return Err(invalid_field(field, "field metadata is stale"));
    }
    if !field.parquet_compatible() {
        return Err(invalid_field(field, "field type is not Parquet-compatible"));
    }
    Ok(())
}

fn invalid_field(field: &ExportField, reason: &str) -> DataExportError {
    DataExportError::InvalidSelection(format!(
        "{} / {}.{}: {reason}",
        field.source, field.topic, field.name
    ))
}

#[cfg(test)]
mod tests;
