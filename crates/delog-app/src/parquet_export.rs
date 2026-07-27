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
mod tests {
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use arrow::array::{Array, BooleanArray, Float32Array, Int64Array, StringArray};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicProvenance, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;
    use delog_parquet_format::decode_schema;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    use super::*;
    use crate::data_export::ExportField;

    /// Shadows the real writer so these tests read without a control argument;
    /// cancellation and progress are covered in `data_export`.
    fn write_structured_parquet<W: Write + Send>(
        writer: W,
        snapshot: &StoreSnapshot,
        fields: &[ExportField],
        window: (i64, i64),
    ) -> Result<u64, DataExportError> {
        super::write_structured_parquet(writer, snapshot, fields, window, &ExportCtl::default())
    }

    fn float_topic_snapshot(
        source_label: &str,
        topic_name: &str,
        timestamps: Vec<i64>,
        values: Vec<Option<f32>>,
        offset_us: i64,
    ) -> (StoreSnapshot, ExportField) {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source(source_label);
        registry.set_source_offset_us(source, offset_us);
        let topic = registry.add_topic(source, topic_name).unwrap();
        let field_id = registry.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                topic_name,
                [FieldSchema::new("value", DataType::Float32, Some("m"), 2.5)
                    .unwrap()
                    .with_description("native value")],
            )
            .unwrap(),
        );
        let store = if timestamps.is_empty() {
            Arc::new(TopicStore::new(schema))
        } else {
            let chunk = Arc::new(
                Chunk::try_new(
                    Int64Array::from(timestamps),
                    vec![Arc::new(Float32Array::from(values))],
                    &schema,
                )
                .unwrap(),
            );
            Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap())
        };
        let snapshot = StoreSnapshot::from_registry(&registry, [(topic, store)], 1).unwrap();
        let field = ExportField {
            id: field_id,
            source_id: source,
            topic_id: topic,
            source: source_label.into(),
            topic: topic_name.into(),
            name: "value".into(),
            label: format!("{source_label} / {topic_name}.value"),
            dtype: DataType::Float32,
            unit: Some("m".into()),
            multiplier: 2.5,
            description: Some("native value".into()),
        };
        (snapshot, field)
    }

    fn nonmonotonic_topic_snapshot() -> (StoreSnapshot, ExportField) {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source("flight");
        let topic = registry.add_topic(source, "ATT").unwrap();
        let field_id = registry.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "ATT",
                [FieldSchema::new("value", DataType::Float32, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunks = [
            (vec![100, 200], vec![1.0, 2.0]),
            (vec![50, 300], vec![3.0, 4.0]),
        ]
        .map(|(timestamps, values)| {
            Arc::new(
                Chunk::try_new(
                    Int64Array::from(timestamps),
                    vec![Arc::new(Float32Array::from(values))],
                    &schema,
                )
                .unwrap(),
            )
        });
        let store = Arc::new(TopicStore::from_chunks(schema, chunks).unwrap());
        assert!(!store.is_monotonic());
        let snapshot = StoreSnapshot::from_registry(&registry, [(topic, store)], 1).unwrap();
        let field = ExportField {
            id: field_id,
            source_id: source,
            topic_id: topic,
            source: "flight".into(),
            topic: "ATT".into(),
            name: "value".into(),
            label: "flight / ATT.value".into(),
            dtype: DataType::Float32,
            unit: None,
            multiplier: 1.0,
            description: None,
        };
        (snapshot, field)
    }

    fn independent_topic_snapshot() -> (StoreSnapshot, Vec<ExportField>) {
        let mut registry = IdentityRegistry::new();

        let att_source = registry.add_source("flight-a");
        registry.set_source_offset_us(att_source, 5);
        let att_topic = registry.add_topic(att_source, "ATT").unwrap();
        let roll_id = registry.add_field(att_topic, "Roll").unwrap();
        let att_schema = Arc::new(
            TopicSchema::new(
                "ATT",
                [
                    FieldSchema::new("Roll", DataType::Float32, Some("rad"), 1.0)
                        .unwrap()
                        .with_description("airframe roll"),
                ],
            )
            .unwrap(),
        );
        let att_chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![5, 25]),
                vec![Arc::new(Float32Array::from(vec![Some(1.5), None]))],
                &att_schema,
            )
            .unwrap(),
        );
        let att_store =
            Arc::new(TopicStore::from_chunks(Arc::clone(&att_schema), [att_chunk]).unwrap());

        let status_source = registry.add_source("flight-b");
        registry.set_source_offset_us(status_source, -2);
        let status_topic = registry.add_topic(status_source, "STATUS").unwrap();
        let armed_id = registry.add_field(status_topic, "armed").unwrap();
        let mode_id = registry.add_field(status_topic, "mode").unwrap();
        let status_schema = Arc::new(
            TopicSchema::new(
                "STATUS",
                [
                    FieldSchema::new("armed", DataType::Boolean, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("mode", DataType::Utf8, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let status_chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![7, 17, 27]),
                vec![
                    Arc::new(BooleanArray::from(vec![true, false, true])),
                    Arc::new(StringArray::from(vec!["MANUAL", "AUTO", "RTL"])),
                ],
                &status_schema,
            )
            .unwrap(),
        );
        let status_store =
            Arc::new(TopicStore::from_chunks(Arc::clone(&status_schema), [status_chunk]).unwrap());

        let snapshot = StoreSnapshot::from_registry(
            &registry,
            [(att_topic, att_store), (status_topic, status_store)],
            1,
        )
        .unwrap();
        let fields = vec![
            ExportField {
                id: roll_id,
                source_id: att_source,
                topic_id: att_topic,
                source: "flight-a".into(),
                topic: "ATT".into(),
                name: "Roll".into(),
                label: "flight-a / ATT.Roll".into(),
                dtype: DataType::Float32,
                unit: Some("rad".into()),
                multiplier: 1.0,
                description: Some("airframe roll".into()),
            },
            ExportField {
                id: armed_id,
                source_id: status_source,
                topic_id: status_topic,
                source: "flight-b".into(),
                topic: "STATUS".into(),
                name: "armed".into(),
                label: "flight-b / STATUS.armed".into(),
                dtype: DataType::Boolean,
                unit: None,
                multiplier: 1.0,
                description: None,
            },
            ExportField {
                id: mode_id,
                source_id: status_source,
                topic_id: status_topic,
                source: "flight-b".into(),
                topic: "STATUS".into(),
                name: "mode".into(),
                label: "flight-b / STATUS.mode".into(),
                dtype: DataType::Utf8,
                unit: None,
                multiplier: 1.0,
                description: None,
            },
        ];
        (snapshot, fields)
    }

    #[test]
    fn writes_independent_topic_timelines_with_typed_padding() {
        let (snapshot, fields) = independent_topic_snapshot();
        let file = tempfile::NamedTempFile::new().unwrap();

        let logical_rows = write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &fields,
            (5, 30),
        )
        .unwrap();

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
                .unwrap();
        let manifest = decode_schema(builder.schema().as_ref()).unwrap().unwrap();
        assert_eq!(manifest.topics.len(), 2);
        assert_eq!(manifest.topics[0].original_topic, "ATT");
        assert_eq!(manifest.topics[1].original_topic, "STATUS");
        assert_eq!(logical_rows, 5);
        assert_eq!(builder.metadata().file_metadata().num_rows(), 3);
        assert_eq!(builder.schema().field(1).data_type(), &DataType::Float32);
        assert_eq!(builder.schema().field(3).data_type(), &DataType::Boolean);
        assert_eq!(builder.schema().field(4).data_type(), &DataType::Utf8);

        let batch = builder.build().unwrap().next().unwrap().unwrap();
        let att_time = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        let roll = batch
            .column(1)
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        let status_time = batch
            .column(2)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        let armed = batch
            .column(3)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        let mode = batch
            .column(4)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();

        assert_eq!(att_time.value(0), 10);
        assert_eq!(att_time.value(1), 30);
        assert!(att_time.is_null(2));
        assert_eq!(roll.value(0), 1.5);
        assert!(roll.is_null(1));
        assert!(roll.is_null(2));
        assert!(att_time.is_valid(1));
        assert_eq!(
            (0..3)
                .map(|index| status_time.value(index))
                .collect::<Vec<_>>(),
            [5, 15, 25]
        );
        assert_eq!(
            (0..3).map(|index| armed.value(index)).collect::<Vec<_>>(),
            [true, false, true]
        );
        assert_eq!(
            (0..3).map(|index| mode.value(index)).collect::<Vec<_>>(),
            ["MANUAL", "AUTO", "RTL"]
        );
    }

    fn exported_schema(file: &tempfile::NamedTempFile) -> SchemaRef {
        ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
            .unwrap()
            .schema()
            .clone()
    }

    fn column_names(schema: &SchemaRef) -> Vec<&str> {
        schema
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect()
    }

    #[test]
    fn names_physical_columns_after_their_topic_and_field() {
        let (snapshot, fields) = independent_topic_snapshot();
        let file = tempfile::NamedTempFile::new().unwrap();

        write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &fields,
            (5, 30),
        )
        .unwrap();

        assert_eq!(
            column_names(&exported_schema(&file)),
            [
                "ATT.t_us",
                "ATT.Roll",
                "STATUS.t_us",
                "STATUS.armed",
                "STATUS.mode"
            ]
        );
    }

    #[test]
    fn duplicate_topic_names_get_instance_qualified_columns() {
        let mut registry = IdentityRegistry::new();
        let mut stores = Vec::new();
        let mut fields = Vec::new();
        for source_label in ["flight-a", "flight-b"] {
            let source = registry.add_source(source_label);
            let topic = registry.add_topic(source, "STATUS").unwrap();
            let field_id = registry.add_field(topic, "armed").unwrap();
            let schema = Arc::new(
                TopicSchema::new(
                    "STATUS",
                    [FieldSchema::new("armed", DataType::Boolean, None::<String>, 1.0).unwrap()],
                )
                .unwrap(),
            );
            let chunk = Arc::new(
                Chunk::try_new(
                    Int64Array::from(vec![1]),
                    vec![Arc::new(BooleanArray::from(vec![true]))],
                    &schema,
                )
                .unwrap(),
            );
            stores.push((
                topic,
                Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
            ));
            fields.push(ExportField {
                id: field_id,
                source_id: source,
                topic_id: topic,
                source: source_label.into(),
                topic: "STATUS".into(),
                name: "armed".into(),
                label: format!("{source_label} / STATUS.armed"),
                dtype: DataType::Boolean,
                unit: None,
                multiplier: 1.0,
                description: None,
            });
        }
        let snapshot = StoreSnapshot::from_registry(&registry, stores, 1).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();

        write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &fields,
            (1, 1),
        )
        .unwrap();

        assert_eq!(
            column_names(&exported_schema(&file)),
            [
                "STATUS[0].t_us",
                "STATUS[0].armed",
                "STATUS[1].t_us",
                "STATUS[1].armed"
            ]
        );
    }

    #[test]
    fn value_columns_carry_unit_multiplier_and_description_metadata() {
        let (snapshot, field) = float_topic_snapshot("flight", "ALT", vec![1], vec![Some(3.5)], 0);
        let file = tempfile::NamedTempFile::new().unwrap();

        write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &[field],
            (1, 1),
        )
        .unwrap();

        let schema = exported_schema(&file);
        assert!(schema.field(0).metadata().is_empty());
        assert_eq!(schema.field(1).metadata().get("unit").unwrap(), "m");
        assert_eq!(schema.field(1).metadata().get("multiplier").unwrap(), "2.5");
        assert_eq!(
            schema.field(1).metadata().get("description").unwrap(),
            "native value"
        );
    }

    #[test]
    fn unit_multiplier_metadata_is_omitted_when_it_carries_nothing() {
        let (snapshot, fields) = independent_topic_snapshot();
        let file = tempfile::NamedTempFile::new().unwrap();

        write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &fields,
            (5, 30),
        )
        .unwrap();

        let schema = exported_schema(&file);
        assert_eq!(schema.field(1).metadata().get("unit").unwrap(), "rad");
        assert_eq!(schema.field(1).metadata().get("multiplier"), None);
        assert!(schema.field(3).metadata().is_empty());
    }

    #[test]
    fn a_field_shadowing_the_timestamp_column_name_stays_unique() {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source("flight");
        let topic = registry.add_topic(source, "ATT").unwrap();
        let field_id = registry.add_field(topic, "t_us").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "ATT",
                [FieldSchema::new("t_us", DataType::Float32, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![1]),
                vec![Arc::new(Float32Array::from(vec![1.0]))],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&registry, [(topic, store)], 1).unwrap();
        let field = ExportField {
            id: field_id,
            source_id: source,
            topic_id: topic,
            source: "flight".into(),
            topic: "ATT".into(),
            name: "t_us".into(),
            label: "flight / ATT.t_us".into(),
            dtype: DataType::Float32,
            unit: None,
            multiplier: 1.0,
            description: None,
        };
        let file = tempfile::NamedTempFile::new().unwrap();

        write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &[field],
            (1, 1),
        )
        .unwrap();

        assert_eq!(
            column_names(&exported_schema(&file)),
            ["ATT.t_us", "ATT.t_us_2"]
        );
    }

    #[test]
    fn writes_unit_multiplier_and_description_to_manifest() {
        let (snapshot, field) = float_topic_snapshot("flight", "ALT", vec![1], vec![Some(3.5)], 0);
        let file = tempfile::NamedTempFile::new().unwrap();

        write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &[field],
            (1, 1),
        )
        .unwrap();

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
                .unwrap();
        let manifest = decode_schema(builder.schema().as_ref()).unwrap().unwrap();
        let field = &manifest.topics[0].fields[0];
        assert_eq!(field.unit.as_deref(), Some("m"));
        assert_eq!(field.multiplier, 2.5);
        assert_eq!(field.description.as_deref(), Some("native value"));
    }

    #[test]
    fn rejects_source_offset_overflow() {
        let (snapshot, field) =
            float_topic_snapshot("overflow", "VALUE", vec![i64::MAX], vec![Some(1.0)], 1);

        let error = write_structured_parquet(Vec::new(), &snapshot, &[field], (i64::MIN, i64::MAX))
            .unwrap_err();

        assert!(error.to_string().contains("timestamp overflow"));
        assert!(error.to_string().contains("9223372036854775807 + 1"));
    }

    #[test]
    fn bounds_topic_stripes_and_row_groups_to_8192_rows() {
        let timestamps = (0..=EXPORT_BATCH_ROWS as i64).collect::<Vec<_>>();
        let values = timestamps
            .iter()
            .map(|value| Some(*value as f32))
            .collect::<Vec<_>>();
        let (snapshot, field) = float_topic_snapshot("flight", "VALUE", timestamps, values, 0);
        let file = tempfile::NamedTempFile::new().unwrap();

        let rows = write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &[field],
            (0, EXPORT_BATCH_ROWS as i64),
        )
        .unwrap();

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
                .unwrap();
        let row_groups = builder
            .metadata()
            .row_groups()
            .iter()
            .map(|group| group.num_rows())
            .collect::<Vec<_>>();
        assert_eq!(rows, 8_193);
        assert_eq!(row_groups, [8_192, 1]);
    }

    #[test]
    fn cursor_caps_multi_chunk_stripes_after_effective_window_filtering() {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source("flight");
        registry.set_source_offset_us(source, 10);
        let topic = registry.add_topic(source, "VALUE").unwrap();
        registry.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "VALUE",
                [FieldSchema::new("value", DataType::Float32, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunks = [(0_i64, 5_000_i64), (5_000, 10_000)].map(|(start, end)| {
            let timestamps = (start..end).collect::<Vec<_>>();
            Arc::new(
                Chunk::try_new(
                    Int64Array::from(timestamps.clone()),
                    vec![Arc::new(Float32Array::from(
                        timestamps
                            .into_iter()
                            .map(|timestamp| timestamp as f32)
                            .collect::<Vec<_>>(),
                    ))],
                    &schema,
                )
                .unwrap(),
            )
        });
        let store = Arc::new(TopicStore::from_chunks(schema, chunks).unwrap());
        let snapshot =
            StoreSnapshot::from_registry(&registry, [(topic, Arc::clone(&store))], 1).unwrap();
        let mut cursor = TopicExportCursor {
            source_label: "flight".into(),
            topic_name: "VALUE".into(),
            store: snapshot.topic_store(topic).unwrap(),
            selected_columns: vec![0],
            source_offset_us: 10,
            window: TimeRange::new(510, 9_010).unwrap(),
            chunk_index: 0,
            row_index: 0,
        };

        let first = cursor.next_stripe(EXPORT_BATCH_ROWS).unwrap().unwrap();
        assert_eq!(first.timestamps.len(), EXPORT_BATCH_ROWS);
        assert_eq!(first.timestamps.value(0), 510);
        assert_eq!(first.timestamps.value(EXPORT_BATCH_ROWS - 1), 8_701);
        let first_values = first.columns[0]
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        assert_eq!(first_values.value(0), 500.0);
        assert_eq!(first_values.value(EXPORT_BATCH_ROWS - 1), 8_691.0);

        let second = cursor.next_stripe(EXPORT_BATCH_ROWS).unwrap().unwrap();
        assert_eq!(second.timestamps.len(), 309);
        assert_eq!(second.timestamps.value(0), 8_702);
        assert_eq!(second.timestamps.value(308), 9_010);
        let second_values = second.columns[0]
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap();
        assert_eq!(second_values.value(0), 8_692.0);
        assert_eq!(second_values.value(308), 9_000.0);
        assert!(cursor.next_stripe(EXPORT_BATCH_ROWS).unwrap().is_none());
    }

    #[test]
    fn pads_unequal_topics_across_multiple_stripes() {
        let mut registry = IdentityRegistry::new();
        let mut stores = Vec::new();
        let mut fields = Vec::new();
        for (source_label, topic_name, row_count) in
            [("flight-a", "A", 8_193_usize), ("flight-b", "B", 8_194)]
        {
            let source = registry.add_source(source_label);
            let topic = registry.add_topic(source, topic_name).unwrap();
            let field_id = registry.add_field(topic, "value").unwrap();
            let schema = Arc::new(
                TopicSchema::new(
                    topic_name,
                    [FieldSchema::new("value", DataType::Float32, None::<String>, 1.0).unwrap()],
                )
                .unwrap(),
            );
            let timestamps = (0..row_count as i64).collect::<Vec<_>>();
            let values = timestamps
                .iter()
                .map(|value| Some(*value as f32))
                .collect::<Vec<_>>();
            let chunk = Arc::new(
                Chunk::try_new(
                    Int64Array::from(timestamps),
                    vec![Arc::new(Float32Array::from(values))],
                    &schema,
                )
                .unwrap(),
            );
            stores.push((
                topic,
                Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
            ));
            fields.push(ExportField {
                id: field_id,
                source_id: source,
                topic_id: topic,
                source: source_label.into(),
                topic: topic_name.into(),
                name: "value".into(),
                label: format!("{source_label} / {topic_name}.value"),
                dtype: DataType::Float32,
                unit: None,
                multiplier: 1.0,
                description: None,
            });
        }
        let snapshot = StoreSnapshot::from_registry(&registry, stores, 1).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();

        let logical_rows = write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &fields,
            (0, 8_193),
        )
        .unwrap();

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
                .unwrap();
        assert_eq!(
            builder
                .metadata()
                .row_groups()
                .iter()
                .map(|group| group.num_rows())
                .collect::<Vec<_>>(),
            [8_192, 2]
        );
        assert_eq!(logical_rows, 8_193 + 8_194);
        let mut reader = builder.with_batch_size(EXPORT_BATCH_ROWS).build().unwrap();
        assert_eq!(
            reader.next().unwrap().unwrap().num_rows(),
            EXPORT_BATCH_ROWS
        );
        let tail = reader.next().unwrap().unwrap();
        let a_time = tail
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        let b_time = tail
            .column(2)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(a_time.value(0), 8_192);
        assert!(a_time.is_null(1));
        assert_eq!(b_time.value(0), 8_192);
        assert_eq!(b_time.value(1), 8_193);
    }

    #[test]
    fn preserves_selected_field_order_within_topic() {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source("flight");
        let topic = registry.add_topic(source, "STATUS").unwrap();
        let count_id = registry.add_field(topic, "count").unwrap();
        registry.add_field(topic, "armed").unwrap();
        let mode_id = registry.add_field(topic, "mode").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "STATUS",
                [
                    FieldSchema::new("count", DataType::Float32, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("armed", DataType::Boolean, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("mode", DataType::Utf8, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10]),
                vec![
                    Arc::new(Float32Array::from(vec![2.0])),
                    Arc::new(BooleanArray::from(vec![true])),
                    Arc::new(StringArray::from(vec!["AUTO"])),
                ],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&registry, [(topic, store)], 1).unwrap();
        let field = |id, name: &str, dtype| ExportField {
            id,
            source_id: source,
            topic_id: topic,
            source: "flight".into(),
            topic: "STATUS".into(),
            name: name.into(),
            label: format!("flight / STATUS.{name}"),
            dtype,
            unit: None,
            multiplier: 1.0,
            description: None,
        };
        let fields = vec![
            field(mode_id, "mode", DataType::Utf8),
            field(count_id, "count", DataType::Float32),
        ];
        let file = tempfile::NamedTempFile::new().unwrap();

        write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &fields,
            (10, 10),
        )
        .unwrap();

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
                .unwrap();
        let manifest = decode_schema(builder.schema().as_ref()).unwrap().unwrap();
        assert_eq!(
            manifest.topics[0]
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["mode", "count"]
        );
        assert_eq!(builder.schema().field(1).data_type(), &DataType::Utf8);
        assert_eq!(builder.schema().field(2).data_type(), &DataType::Float32);
    }

    #[test]
    fn keeps_duplicate_topic_names_separate_by_source() {
        let mut registry = IdentityRegistry::new();
        let mut stores = Vec::new();
        let mut fields = Vec::new();
        for source_label in ["flight-a", "flight-b"] {
            let source = registry.add_source(source_label);
            let topic = registry.add_topic(source, "STATUS").unwrap();
            let field_id = registry.add_field(topic, "armed").unwrap();
            let schema = Arc::new(
                TopicSchema::new(
                    "STATUS",
                    [FieldSchema::new("armed", DataType::Boolean, None::<String>, 1.0).unwrap()],
                )
                .unwrap(),
            );
            let chunk = Arc::new(
                Chunk::try_new(
                    Int64Array::from(vec![1]),
                    vec![Arc::new(BooleanArray::from(vec![true]))],
                    &schema,
                )
                .unwrap(),
            );
            stores.push((
                topic,
                Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
            ));
            fields.push(ExportField {
                id: field_id,
                source_id: source,
                topic_id: topic,
                source: source_label.into(),
                topic: "STATUS".into(),
                name: "armed".into(),
                label: format!("{source_label} / STATUS.armed"),
                dtype: DataType::Boolean,
                unit: None,
                multiplier: 1.0,
                description: None,
            });
        }
        let snapshot = StoreSnapshot::from_registry(&registry, stores, 1).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();

        write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &fields,
            (1, 1),
        )
        .unwrap();

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
                .unwrap();
        let manifest = decode_schema(builder.schema().as_ref()).unwrap().unwrap();
        assert_eq!(manifest.topics[0].original_topic, "STATUS");
        assert_eq!(manifest.topics[1].original_topic, "STATUS");
        assert_eq!(manifest.topics[0].original_source, "flight-a");
        assert_eq!(manifest.topics[1].original_source, "flight-b");
    }

    #[test]
    fn reexport_preserves_imported_topic_provenance() {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source("imported.parquet");
        let topic = registry.add_topic(source, "STATUS[0]").unwrap();
        let field_id = registry.add_field(topic, "armed").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "STATUS[0]",
                [FieldSchema::new("armed", DataType::Boolean, None::<String>, 1.0).unwrap()],
            )
            .unwrap()
            .with_provenance(TopicProvenance::new("flight-a", "STATUS").unwrap()),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10]),
                vec![Arc::new(BooleanArray::from(vec![true]))],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&registry, [(topic, store)], 1).unwrap();
        let field = ExportField {
            id: field_id,
            source_id: source,
            topic_id: topic,
            source: "imported.parquet".into(),
            topic: "STATUS[0]".into(),
            name: "armed".into(),
            label: "imported.parquet / STATUS[0].armed".into(),
            dtype: DataType::Boolean,
            unit: None,
            multiplier: 1.0,
            description: None,
        };
        let file = tempfile::NamedTempFile::new().unwrap();

        write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &[field],
            (10, 10),
        )
        .unwrap();

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
                .unwrap();
        let manifest = decode_schema(builder.schema().as_ref()).unwrap().unwrap();
        assert_eq!(manifest.topics[0].original_source, "flight-a");
        assert_eq!(manifest.topics[0].original_topic, "STATUS");
    }

    #[test]
    fn writes_readable_marked_file_when_all_topics_are_empty() {
        let (snapshot, field) = float_topic_snapshot("flight", "EMPTY", vec![], vec![], 0);
        let file = tempfile::NamedTempFile::new().unwrap();

        let rows = write_structured_parquet(
            std::fs::File::create(file.path()).unwrap(),
            &snapshot,
            &[field],
            (0, 100),
        )
        .unwrap();

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
                .unwrap();
        let manifest = decode_schema(builder.schema().as_ref()).unwrap().unwrap();
        assert_eq!(rows, 0);
        assert_eq!(builder.metadata().file_metadata().num_rows(), 0);
        assert_eq!(manifest.topics[0].original_topic, "EMPTY");
        assert!(builder.build().unwrap().next().is_none());
    }

    struct CountingWriter {
        writes: Arc<AtomicUsize>,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn rejects_cross_chunk_timestamp_regression_before_writing_bytes() {
        let (snapshot, field) = nonmonotonic_topic_snapshot();
        let writes = Arc::new(AtomicUsize::new(0));

        let error = write_structured_parquet(
            CountingWriter {
                writes: Arc::clone(&writes),
            },
            &snapshot,
            &[field],
            (0, 300),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("topic timestamps are non-monotonic across chunks")
        );
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn rejects_stale_fields_before_creating_destination_writer() {
        let (snapshot, mut fields) = independent_topic_snapshot();
        fields[0].id = delog_core::identity::FieldId(u32::MAX);
        let writes = Arc::new(AtomicUsize::new(0));

        let error = write_structured_parquet(
            CountingWriter {
                writes: Arc::clone(&writes),
            },
            &snapshot,
            &fields,
            (5, 30),
        )
        .unwrap_err();

        assert!(error.to_string().contains("field is stale"));
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn rejects_mixed_schema_ownership_before_creating_destination_writer() {
        let (snapshot, fields) = independent_topic_snapshot();
        let mut forged = fields[1].clone();
        forged.topic_id = fields[0].topic_id;
        let writes = Arc::new(AtomicUsize::new(0));

        let error = write_structured_parquet(
            CountingWriter {
                writes: Arc::clone(&writes),
            },
            &snapshot,
            &[fields[0].clone(), forged],
            (5, 30),
        )
        .unwrap_err();

        assert!(error.to_string().contains("mixed schema ownership"));
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[derive(Default)]
    struct FinalFlushFailure {
        bytes: Vec<u8>,
    }

    impl Write for FinalFlushFailure {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other(
                "injected structured final flush failure",
            ))
        }
    }

    #[test]
    fn returns_final_flush_failure_after_writing_footer() {
        let (snapshot, field) =
            float_topic_snapshot("flight", "VALUE", vec![1], vec![Some(2.0)], 0);

        let error =
            write_structured_parquet(FinalFlushFailure::default(), &snapshot, &[field], (1, 1))
                .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injected structured final flush failure")
        );
    }
}
