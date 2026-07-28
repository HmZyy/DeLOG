use std::collections::HashMap;

use delog_core::identity::SourceId;
use delog_core::snapshot::StoreSnapshot;

use crate::api::{
    PendingColumn, PendingField, PendingTopic, TopicMatch, find_fields_in_topic, find_topics,
    materialize_field,
};
use crate::operations::{
    SplitBySpec, MergeSpec, OperationMode, OperationSpec, TopicRegistry, TopicSelector,
    TransformSpec,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StreamKey {
    pub source: SourceId,
    pub topic: String,
}

impl StreamKey {
    pub fn new(source: SourceId, topic: impl Into<String>) -> Self {
        Self {
            source,
            topic: topic.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SeedField {
    F64 {
        unit: Option<String>,
        sample: Option<(i64, f64)>,
    },
    Utf8 {
        unit: Option<String>,
        sample: Option<(i64, String)>,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergeSeed {
    pub(crate) fields: HashMap<(String, String), SeedField>,
}

impl MergeSeed {
    pub fn f64(&self, topic: &str, field: &str) -> Option<(i64, f64)> {
        match self.fields.get(&(topic.to_owned(), field.to_owned())) {
            Some(SeedField::F64 {
                sample: Some((time, value)),
                ..
            }) => Some((*time, *value)),
            _ => None,
        }
    }

    pub fn utf8(&self, topic: &str, field: &str) -> Option<(i64, &str)> {
        match self.fields.get(&(topic.to_owned(), field.to_owned())) {
            Some(SeedField::Utf8 {
                sample: Some((time, value)),
                ..
            }) => Some((*time, value)),
            _ => None,
        }
    }

    pub fn unit(&self, topic: &str, field: &str) -> Option<Option<&str>> {
        self.fields
            .get(&(topic.to_owned(), field.to_owned()))
            .map(|field| match field {
                SeedField::F64 { unit, .. } | SeedField::Utf8 { unit, .. } => unit.as_deref(),
            })
    }

    pub fn is_utf8(&self, topic: &str, field: &str) -> bool {
        matches!(
            self.fields.get(&(topic.to_owned(), field.to_owned())),
            Some(SeedField::Utf8 { .. })
        )
    }
}

pub struct SnapshotOperationOutput {
    pub topics: Vec<PendingTopic>,
    pub watermarks: HashMap<StreamKey, i64>,
    pub merge_seeds: HashMap<(usize, SourceId), MergeSeed>,
    pub registry: TopicRegistry,
}

impl Default for SnapshotOperationOutput {
    fn default() -> Self {
        Self {
            topics: Vec::new(),
            watermarks: HashMap::new(),
            merge_seeds: HashMap::new(),
            registry: TopicRegistry::default(),
        }
    }
}

struct MaterializedTopic {
    key: StreamKey,
    times: Vec<i64>,
    fields: Vec<(String, PendingColumn, Option<String>)>,
}

fn candidate_topic_paths(matches: &[TopicMatch]) -> String {
    matches
        .iter()
        .map(|candidate| format!("{}/{}", candidate.source_label, candidate.topic_name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn missing_topic_error(snapshot: &StoreSnapshot, selector: &TopicSelector) -> String {
    let candidates = find_topics(
        snapshot,
        None,
        selector.source.as_deref(),
        selector.instance,
    );
    if candidates.is_empty() {
        format!("topic '{}' not found", selector.topic)
    } else {
        format!(
            "topic '{}' not found; candidates: {}",
            selector.topic,
            candidate_topic_paths(&candidates)
        )
    }
}

fn require_usable_schema(
    snapshot: &StoreSnapshot,
    topic: TopicMatch,
    selector: &TopicSelector,
    mode: OperationMode,
) -> Result<Option<TopicMatch>, String> {
    if snapshot.topic_store(topic.topic_id).is_some() {
        Ok(Some(topic))
    } else if mode == OperationMode::Snapshot {
        Err(missing_topic_error(snapshot, selector))
    } else {
        Ok(None)
    }
}

fn resolve_topic(
    snapshot: &StoreSnapshot,
    selector: &TopicSelector,
    mode: OperationMode,
) -> Result<Option<TopicMatch>, String> {
    let matches = find_topics(
        snapshot,
        Some(&selector.topic),
        selector.source.as_deref(),
        selector.instance,
    );
    match matches.len() {
        1 => Ok(matches.into_iter().next()),
        0 if mode == OperationMode::Snapshot => Err(missing_topic_error(snapshot, selector)),
        0 => Ok(None),
        _ => Err(format!(
            "topic '{}' is ambiguous; candidates: {}; pass source= or instance=",
            selector.topic,
            candidate_topic_paths(&matches)
        )),
    }
}

fn materialize_topic(
    snapshot: &StoreSnapshot,
    topic: TopicMatch,
    requested: Option<&[String]>,
) -> Result<MaterializedTopic, String> {
    let store = snapshot
        .topic_store(topic.topic_id)
        .ok_or_else(|| format!("topic '{}' has no usable schema", topic.topic_name))?;
    let available = find_fields_in_topic(snapshot, topic.topic_id, None);
    for field in &available {
        if store.schema.field_by_name(&field.field_name).is_none() {
            return Err(format!(
                "topic '{}' live identity field '{}' is missing from schema",
                topic.topic_name, field.field_name
            ));
        }
    }
    let resolved = store
        .schema
        .fields()
        .iter()
        .map(|schema_field| {
            let field = available
                .iter()
                .find(|field| field.field_name == schema_field.name)
                .ok_or_else(|| {
                    format!(
                        "topic '{}' schema field '{}' has no live identity field",
                        topic.topic_name, schema_field.name
                    )
                })?;
            Ok((field.clone(), schema_field))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let selected = match requested {
        Some(requested) => {
            for name in requested {
                if store.schema.field_by_name(name).is_none() {
                    return Err(format!(
                        "field '{name}' not found in topic '{}'",
                        topic.topic_name
                    ));
                }
            }
            requested
                .iter()
                .map(|name| {
                    resolved
                        .iter()
                        .find(|(_, schema_field)| schema_field.name == *name)
                        .cloned()
                        .ok_or_else(|| {
                            format!("field '{name}' not found in topic '{}'", topic.topic_name)
                        })
                })
                .collect::<Result<Vec<_>, String>>()?
        }
        None => resolved,
    };

    if store.is_empty() {
        let fields = selected
            .into_iter()
            .map(|(field, schema_field)| {
                let values = if schema_field.is_string() {
                    PendingColumn::Utf8(Vec::new())
                } else {
                    PendingColumn::F64(Vec::new())
                };
                (field.field_name, values, schema_field.unit.clone())
            })
            .collect();
        return Ok(MaterializedTopic {
            key: StreamKey::new(topic.source_id, topic.topic_name),
            times: Vec::new(),
            fields,
        });
    }

    let mut times = None;
    let mut fields = Vec::with_capacity(selected.len());
    for (field, schema_field) in selected {
        let (field_times, values, strings) = materialize_field(snapshot, field.field_id)?;
        match &times {
            Some(existing) if existing != &field_times => {
                return Err(format!(
                    "topic '{}' field '{}' does not share the topic timeline",
                    topic.topic_name, field.field_name
                ));
            }
            None => times = Some(field_times),
            _ => {}
        }
        let column = match strings {
            Some(strings) => PendingColumn::Utf8(strings),
            None => PendingColumn::F64(values),
        };
        fields.push((field.field_name, column, schema_field.unit.clone()));
    }

    let mut topic = MaterializedTopic {
        key: StreamKey::new(topic.source_id, topic.topic_name),
        times: times.unwrap_or_default(),
        fields,
    };
    stable_sort_topic(&mut topic);
    Ok(topic)
}

fn stable_sort_topic(topic: &mut MaterializedTopic) {
    let mut rows = (0..topic.times.len()).collect::<Vec<_>>();
    rows.sort_by_key(|&row| topic.times[row]);
    if rows.iter().enumerate().all(|(index, row)| index == *row) {
        return;
    }
    topic.times = rows.iter().map(|&row| topic.times[row]).collect();
    for (_, column, _) in &mut topic.fields {
        *column = slice_column(column, &rows);
    }
}

fn record_watermark(out: &mut SnapshotOperationOutput, topic: &MaterializedTopic) {
    if let Some(last) = topic.times.last() {
        out.watermarks.insert(topic.key.clone(), *last);
    }
}

pub(crate) fn pending_topic(
    name: String,
    times: Vec<i64>,
    fields: impl IntoIterator<Item = (String, PendingColumn, Option<String>)>,
) -> Result<PendingTopic, String> {
    let mut topic = PendingTopic::new(name, times);
    for (name, values, unit) in fields {
        topic.add_field(PendingField { name, values, unit })?;
    }
    Ok(topic)
}

fn execute_transform(
    snapshot: &StoreSnapshot,
    spec: &TransformSpec,
    out: &mut SnapshotOperationOutput,
) -> Result<(), String> {
    let Some(topic_match) = resolve_topic(snapshot, &spec.input, spec.mode)? else {
        return Ok(());
    };
    let Some(topic_match) = require_usable_schema(snapshot, topic_match, &spec.input, spec.mode)?
    else {
        return Ok(());
    };
    // Materialize all fields because unselected fields are pass-through columns.
    let mut topic = materialize_topic(snapshot, topic_match, None)?;
    if let Some(requested) = &spec.fields {
        for name in requested {
            if !topic.fields.iter().any(|(field, _, _)| field == name) {
                return Err(format!(
                    "field '{name}' not found in topic '{}'",
                    spec.input.topic
                ));
            }
        }
    }
    record_watermark(out, &topic);
    if !spec.mode.wants_snapshot() || topic.times.is_empty() {
        return Ok(());
    }

    for (name, values, unit) in &mut topic.fields {
        let selected = spec
            .fields
            .as_ref()
            .is_none_or(|fields| fields.iter().any(|field| field == name));
        if selected && let PendingColumn::F64(values) = values {
            for value in values {
                *value = *value * spec.multiplier + spec.offset;
            }
            if let Some(override_unit) = spec.units.get(name).or(spec.unit.as_ref()) {
                *unit = Some(override_unit.clone());
            }
        }
    }
    out.topics.push(pending_topic(
        spec.output_topic.clone(),
        topic.times,
        topic.fields,
    )?);
    Ok(())
}

pub(crate) fn split_key(column: &PendingColumn, row: usize) -> Option<String> {
    match column {
        PendingColumn::Utf8(values) => values.get(row).filter(|value| !value.is_empty()).cloned(),
        PendingColumn::F64(values) => {
            let value = *values.get(row)?;
            if !value.is_finite() {
                None
            } else if value.fract() == 0.0 {
                Some(format!("{value:.0}"))
            } else {
                Some(value.to_string())
            }
        }
    }
}

pub(crate) fn slice_column(column: &PendingColumn, rows: &[usize]) -> PendingColumn {
    match column {
        PendingColumn::F64(values) => {
            PendingColumn::F64(rows.iter().map(|&row| values[row]).collect())
        }
        PendingColumn::Utf8(values) => {
            PendingColumn::Utf8(rows.iter().map(|&row| values[row].clone()).collect())
        }
    }
}

fn execute_split(
    snapshot: &StoreSnapshot,
    spec: &SplitBySpec,
    out: &mut SnapshotOperationOutput,
) -> Result<(), String> {
    let Some(topic_match) = resolve_topic(snapshot, &spec.input, spec.mode)? else {
        return Ok(());
    };
    let Some(topic_match) = require_usable_schema(snapshot, topic_match, &spec.input, spec.mode)?
    else {
        return Ok(());
    };
    let topic = materialize_topic(snapshot, topic_match, None)?;
    let split_column = topic
        .fields
        .iter()
        .find(|(name, _, _)| name == &spec.field)
        .ok_or_else(|| {
            format!(
                "field '{}' not found in topic '{}'",
                spec.field, spec.input.topic
            )
        })?;
    let selected: Vec<String> = match &spec.fields {
        Some(fields) => {
            for name in fields {
                if !topic.fields.iter().any(|(field, _, _)| field == name) {
                    return Err(format!(
                        "field '{name}' not found in topic '{}'",
                        spec.input.topic
                    ));
                }
            }
            fields
                .iter()
                .filter(|name| *name != &spec.field)
                .cloned()
                .collect()
        }
        None => topic
            .fields
            .iter()
            .filter_map(|(name, _, _)| (name != &spec.field).then_some(name.clone()))
            .collect(),
    };
    record_watermark(out, &topic);
    if !spec.mode.wants_snapshot() || topic.times.is_empty() {
        return Ok(());
    }

    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut positions = HashMap::<String, usize>::new();
    for row in 0..topic.times.len() {
        let Some(key) = split_key(&split_column.1, row) else {
            continue;
        };
        let position = match positions.get(&key) {
            Some(position) => *position,
            None => {
                let position = groups.len();
                positions.insert(key.clone(), position);
                groups.push((key, Vec::new()));
                position
            }
        };
        groups[position].1.push(row);
    }

    for (key, rows) in groups {
        let name = spec
            .output_template
            .replace("{topic}", &spec.input.topic)
            .replace("{value}", &key);
        let times = rows.iter().map(|&row| topic.times[row]).collect();
        let fields = selected.iter().map(|selected_name| {
            let (name, column, unit) = topic
                .fields
                .iter()
                .find(|(name, _, _)| name == selected_name)
                .expect("selected fields were validated");
            (name.clone(), slice_column(column, &rows), unit.clone())
        });
        out.topics.push(pending_topic(name, times, fields)?);
    }
    Ok(())
}

fn prev_indices(source: &[i64], base: &[i64]) -> Vec<Option<usize>> {
    base.iter()
        .map(|time| match source.binary_search(time) {
            Ok(index) => Some(index),
            Err(0) => None,
            Err(index) => Some(index - 1),
        })
        .collect()
}

fn align_column(column: &PendingColumn, indices: &[Option<usize>]) -> PendingColumn {
    match column {
        PendingColumn::F64(values) => PendingColumn::F64(
            indices
                .iter()
                .map(|index| index.map_or(f64::NAN, |index| values[index]))
                .collect(),
        ),
        PendingColumn::Utf8(values) => PendingColumn::Utf8(
            indices
                .iter()
                .map(|index| index.map_or_else(String::new, |index| values[index].clone()))
                .collect(),
        ),
    }
}

fn seed_secondary(seed: &mut MergeSeed, configured_topic: &str, topic: &MaterializedTopic) {
    let final_sample = topic
        .times
        .last()
        .copied()
        .map(|time| (time, topic.times.len() - 1));
    for (name, column, unit) in &topic.fields {
        let field = match column {
            PendingColumn::F64(values) => SeedField::F64 {
                unit: unit.clone(),
                sample: final_sample.map(|(time, index)| (time, values[index])),
            },
            PendingColumn::Utf8(values) => SeedField::Utf8 {
                unit: unit.clone(),
                sample: final_sample.map(|(time, index)| (time, values[index].clone())),
            },
        };
        seed.fields
            .insert((configured_topic.to_owned(), name.clone()), field);
    }
}

fn execute_merge(
    snapshot: &StoreSnapshot,
    operation_index: usize,
    spec: &MergeSpec,
    out: &mut SnapshotOperationOutput,
) -> Result<(), String> {
    if spec.output_names.len() != spec.topics.len()
        || spec
            .output_names
            .iter()
            .zip(&spec.topics)
            .any(|(names, (_, fields))| names.len() != fields.len())
    {
        return Err("merge output field names do not match selected fields".to_owned());
    }

    let mut inputs = Vec::with_capacity(spec.topics.len());
    let mut any_missing = false;
    for (topic_name, fields) in &spec.topics {
        let selector = TopicSelector {
            topic: topic_name.clone(),
            source: spec.source.clone(),
            instance: None,
        };
        let Some(topic_match) = resolve_topic(snapshot, &selector, spec.mode)? else {
            any_missing = true;
            inputs.push(None);
            continue;
        };
        let Some(topic_match) = require_usable_schema(snapshot, topic_match, &selector, spec.mode)?
        else {
            any_missing = true;
            inputs.push(None);
            continue;
        };
        let topic = materialize_topic(snapshot, topic_match, Some(fields))?;
        record_watermark(out, &topic);
        inputs.push(Some(topic));
    }

    let base_index = spec
        .topics
        .iter()
        .position(|(topic, _)| topic == &spec.base_topic)
        .ok_or_else(|| {
            format!(
                "merge base_topic '{}' must be present in topics",
                spec.base_topic
            )
        })?;
    if spec.mode.wants_live() {
        for (index, topic) in inputs.iter().enumerate() {
            if index == base_index {
                continue;
            }
            if let Some(topic) = topic {
                seed_secondary(
                    out.merge_seeds
                        .entry((operation_index, topic.key.source))
                        .or_default(),
                    &spec.topics[index].0,
                    topic,
                );
            }
        }
    }
    if any_missing {
        return Ok(());
    }
    let inputs = inputs
        .into_iter()
        .map(|topic| topic.expect("all merge inputs were resolved"))
        .collect::<Vec<_>>();
    let source = inputs[base_index].key.source;
    if inputs.iter().any(|topic| topic.key.source != source) {
        return Err("merge inputs resolved to different sources".to_owned());
    }
    if !spec.mode.wants_snapshot() || inputs[base_index].times.is_empty() {
        return Ok(());
    }

    let base_times = inputs[base_index].times.clone();
    let mut output_fields = Vec::new();
    for (input_index, topic) in inputs.iter().enumerate() {
        let indices = (input_index != base_index).then(|| prev_indices(&topic.times, &base_times));
        for (field_index, (_, column, unit)) in topic.fields.iter().enumerate() {
            let output_name = spec
                .output_names
                .get(input_index)
                .and_then(|names| names.get(field_index))
                .ok_or_else(|| {
                    "merge output field names do not match selected fields".to_owned()
                })?;
            let values = match &indices {
                Some(indices) => align_column(column, indices),
                None => column.clone(),
            };
            output_fields.push((output_name.clone(), values, unit.clone()));
        }
    }
    out.topics.push(pending_topic(
        spec.output_topic.clone(),
        base_times,
        output_fields,
    )?);
    Ok(())
}

pub fn prepare_snapshot(
    snapshot: &StoreSnapshot,
    specs: &[OperationSpec],
) -> Result<SnapshotOperationOutput, String> {
    let mut out = SnapshotOperationOutput::default();
    out.registry.preclaim_static(specs)?;
    for (index, spec) in specs.iter().enumerate() {
        let first_topic = out.topics.len();
        match spec {
            OperationSpec::Transform(spec) => execute_transform(snapshot, spec, &mut out)?,
            OperationSpec::SplitBy(spec) => execute_split(snapshot, spec, &mut out)?,
            OperationSpec::Merge(spec) => execute_merge(snapshot, index, spec, &mut out)?,
        }
        let claims = out.topics[first_topic..]
            .iter()
            .map(|topic| {
                let schema = topic
                    .fields
                    .iter()
                    .map(|field| {
                        let dtype = match &field.values {
                            PendingColumn::F64(_) => arrow::datatypes::DataType::Float64,
                            PendingColumn::Utf8(_) => arrow::datatypes::DataType::Utf8,
                        };
                        (field.name.clone(), dtype, field.unit.clone())
                    })
                    .collect();
                (topic.name.clone(), Some(schema))
            })
            .collect::<Vec<_>>();
        out.registry.claim_batch(index, &claims)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::{IdentityRegistry, SourceId};
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;

    use crate::api::{PendingColumn, PendingTopic};
    use crate::operations::{
        SplitBySpec, MergeSpec, OperationMode, OperationSpec, TopicSelector, TransformSpec,
    };

    use super::{StreamKey, prepare_snapshot};

    fn topic_store(
        name: &str,
        fields: Vec<FieldSchema>,
        times: Vec<i64>,
        columns: Vec<ArrayRef>,
    ) -> Arc<TopicStore> {
        let schema = Arc::new(TopicSchema::new(name, fields).unwrap());
        let chunk = Arc::new(Chunk::try_new(Int64Array::from(times), columns, &schema).unwrap());
        Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap())
    }

    fn chunked_topic_store(
        name: &str,
        fields: Vec<FieldSchema>,
        chunks: Vec<(Vec<i64>, Vec<ArrayRef>)>,
    ) -> Arc<TopicStore> {
        let schema = Arc::new(TopicSchema::new(name, fields).unwrap());
        let chunks = chunks
            .into_iter()
            .map(|(times, columns)| {
                Arc::new(Chunk::try_new(Int64Array::from(times), columns, &schema).unwrap())
            })
            .collect::<Vec<_>>();
        Arc::new(TopicStore::from_chunks(schema, chunks).unwrap())
    }

    fn operation_fixture() -> StoreSnapshot {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let attitude = ids.add_topic(source, "ATTITUDE").unwrap();
        let gps = ids.add_topic(source, "GPS").unwrap();
        let param = ids.add_topic(source, "PARAM_VALUE").unwrap();
        ids.add_field(attitude, "roll").unwrap();
        ids.add_field(attitude, "frame").unwrap();
        ids.add_field(gps, "alt").unwrap();
        ids.add_field(param, "param_id").unwrap();
        ids.add_field(param, "param_value").unwrap();

        let attitude_store = topic_store(
            "ATTITUDE",
            vec![
                FieldSchema::new("roll", DataType::Float64, Some("rad"), 1.0).unwrap(),
                FieldSchema::new("frame", DataType::Utf8, None::<String>, 1.0).unwrap(),
            ],
            vec![100, 200],
            vec![
                Arc::new(Float64Array::from(vec![0.0, std::f64::consts::FRAC_PI_2])),
                Arc::new(StringArray::from(vec!["NED", "NED"])),
            ],
        );
        let gps_store = topic_store(
            "GPS",
            vec![FieldSchema::new("alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            vec![150],
            vec![Arc::new(Float64Array::from(vec![100.0]))],
        );
        let param_store = topic_store(
            "PARAM_VALUE",
            vec![
                FieldSchema::new("param_id", DataType::Utf8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("param_value", DataType::Float64, None::<String>, 1.0).unwrap(),
            ],
            vec![110, 120, 130],
            vec![
                Arc::new(StringArray::from(vec![
                    "MAX_SPEED",
                    "MIN_SPEED",
                    "MAX_SPEED",
                ])),
                Arc::new(Float64Array::from(vec![12.0, 5.0, 14.0])),
            ],
        );
        StoreSnapshot::from_registry(
            &ids,
            [
                (attitude, attitude_store),
                (gps, gps_store),
                (param, param_store),
            ],
            0,
        )
        .unwrap()
    }

    fn fixture_specs() -> Vec<OperationSpec> {
        vec![
            OperationSpec::Transform(TransformSpec {
                input: TopicSelector {
                    topic: "ATTITUDE".into(),
                    source: None,
                    instance: None,
                },
                multiplier: 180.0 / std::f64::consts::PI,
                offset: 0.0,
                fields: Some(vec!["roll".into()]),
                unit: Some("deg".into()),
                units: HashMap::new(),
                output_topic: "ATTITUDE_DEG".into(),
                mode: OperationMode::Both,
            }),
            OperationSpec::SplitBy(SplitBySpec {
                input: TopicSelector {
                    topic: "PARAM_VALUE".into(),
                    source: None,
                    instance: None,
                },
                field: "param_id".into(),
                fields: Some(vec!["param_value".into()]),
                output_template: "{topic}/{value}".into(),
                mode: OperationMode::Both,
            }),
            OperationSpec::Merge(MergeSpec {
                topics: vec![
                    ("ATTITUDE".into(), vec!["roll".into()]),
                    ("GPS".into(), vec!["alt".into()]),
                ],
                base_topic: "ATTITUDE".into(),
                output_topic: "STATE".into(),
                source: None,
                output_names: vec![vec!["roll".into()], vec!["alt".into()]],
                mode: OperationMode::Both,
            }),
        ]
    }

    fn field<'a>(topics: &'a [PendingTopic], topic: &str, field: &str) -> &'a PendingColumn {
        &topics
            .iter()
            .find(|candidate| candidate.name == topic)
            .unwrap_or_else(|| panic!("missing topic {topic}"))
            .fields
            .iter()
            .find(|candidate| candidate.name == field)
            .unwrap_or_else(|| panic!("missing field {topic}/{field}"))
            .values
    }

    fn assert_topic_f64(topics: &[PendingTopic], topic: &str, name: &str, expected: &[f64]) {
        let PendingColumn::F64(actual) = field(topics, topic, name) else {
            panic!("{topic}/{name} is not numeric");
        };
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert!(
                (actual.is_nan() && expected.is_nan()) || (actual - expected).abs() < 1e-9,
                "expected {expected:?}, got {actual:?}"
            );
        }
    }

    fn assert_topic_utf8(topics: &[PendingTopic], topic: &str, name: &str, expected: &[&str]) {
        let PendingColumn::Utf8(actual) = field(topics, topic, name) else {
            panic!("{topic}/{name} is not UTF-8");
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn snapshot_operations_transform_split_and_merge() {
        let snap = operation_fixture();
        let out = prepare_snapshot(&snap, &fixture_specs()).unwrap();

        assert_topic_f64(&out.topics, "ATTITUDE_DEG", "roll", &[0.0, 90.0]);
        assert_topic_utf8(&out.topics, "ATTITUDE_DEG", "frame", &["NED", "NED"]);
        assert_topic_f64(
            &out.topics,
            "PARAM_VALUE/MAX_SPEED",
            "param_value",
            &[12.0, 14.0],
        );
        assert_topic_f64(&out.topics, "STATE", "alt", &[f64::NAN, 100.0]);
        assert_eq!(
            out.watermarks[&StreamKey::new(SourceId(0), "ATTITUDE")],
            200
        );
        assert_eq!(
            out.merge_seeds[&(2, SourceId(0))].f64("GPS", "alt"),
            Some((150, 100.0))
        );
    }

    #[test]
    fn snapshot_rejects_two_operations_owning_one_output_topic() {
        let specs = vec![
            OperationSpec::Transform(TransformSpec {
                input: TopicSelector {
                    topic: "ATTITUDE".into(),
                    source: None,
                    instance: None,
                },
                multiplier: 1.0,
                offset: 0.0,
                fields: Some(vec!["roll".into(), "frame".into()]),
                unit: None,
                units: HashMap::new(),
                output_topic: "COLLISION".into(),
                mode: OperationMode::Snapshot,
            }),
            OperationSpec::Merge(MergeSpec {
                topics: vec![("ATTITUDE".into(), vec!["frame".into(), "roll".into()])],
                base_topic: "ATTITUDE".into(),
                output_topic: "COLLISION".into(),
                source: None,
                output_names: vec![vec!["renamed".into(), "roll".into()]],
                mode: OperationMode::Snapshot,
            }),
        ];

        let error = match prepare_snapshot(&operation_fixture(), &specs) {
            Ok(_) => panic!("duplicate ownership unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.contains("output topic 'COLLISION'"), "{error}");
        assert!(error.contains("operation 0"), "{error}");
        assert!(error.contains("operation 1"), "{error}");
    }

    #[test]
    fn failed_dynamic_split_claim_is_atomic_across_all_topics() {
        let specs = vec![
            OperationSpec::Transform(TransformSpec {
                input: TopicSelector {
                    topic: "ATTITUDE".into(),
                    source: None,
                    instance: None,
                },
                multiplier: 1.0,
                offset: 0.0,
                fields: None,
                unit: None,
                units: HashMap::new(),
                output_topic: "PARAM_VALUE/MIN_SPEED".into(),
                mode: OperationMode::Snapshot,
            }),
            OperationSpec::SplitBy(SplitBySpec {
                input: TopicSelector {
                    topic: "PARAM_VALUE".into(),
                    source: None,
                    instance: None,
                },
                field: "param_id".into(),
                fields: Some(vec!["param_value".into()]),
                output_template: "{topic}/{value}".into(),
                mode: OperationMode::Snapshot,
            }),
        ];

        let error = match prepare_snapshot(&operation_fixture(), &specs) {
            Ok(_) => panic!("partial dynamic ownership unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.contains("PARAM_VALUE/MIN_SPEED"), "{error}");
    }

    #[test]
    fn merge_prefixes_colliding_selected_field_names() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let attitude = ids.add_topic(source, "ATTITUDE").unwrap();
        let target = ids.add_topic(source, "TARGET").unwrap();
        ids.add_field(attitude, "roll").unwrap();
        ids.add_field(target, "roll").unwrap();
        let snap = StoreSnapshot::from_registry(
            &ids,
            [
                (
                    attitude,
                    topic_store(
                        "ATTITUDE",
                        vec![
                            FieldSchema::new("roll", DataType::Float64, None::<String>, 1.0)
                                .unwrap(),
                        ],
                        vec![100],
                        vec![Arc::new(Float64Array::from(vec![1.0]))],
                    ),
                ),
                (
                    target,
                    topic_store(
                        "TARGET",
                        vec![
                            FieldSchema::new("roll", DataType::Float64, None::<String>, 1.0)
                                .unwrap(),
                        ],
                        vec![90],
                        vec![Arc::new(Float64Array::from(vec![2.0]))],
                    ),
                ),
            ],
            0,
        )
        .unwrap();
        let spec = OperationSpec::Merge(MergeSpec {
            topics: vec![
                ("ATTITUDE".into(), vec!["roll".into()]),
                ("TARGET".into(), vec!["roll".into()]),
            ],
            base_topic: "ATTITUDE".into(),
            output_topic: "STATE".into(),
            source: None,
            output_names: vec![vec!["ATTITUDE_roll".into()], vec!["TARGET_roll".into()]],
            mode: OperationMode::Snapshot,
        });

        let out = prepare_snapshot(&snap, &[spec]).unwrap();
        assert_topic_f64(&out.topics, "STATE", "ATTITUDE_roll", &[1.0]);
        assert_topic_f64(&out.topics, "STATE", "TARGET_roll", &[2.0]);
    }

    #[test]
    fn both_mode_absent_topic_is_not_an_error() {
        let spec = OperationSpec::Transform(TransformSpec {
            input: TopicSelector {
                topic: "FUTURE".into(),
                source: None,
                instance: None,
            },
            multiplier: 1.0,
            offset: 0.0,
            fields: None,
            unit: None,
            units: HashMap::new(),
            output_topic: "FUTURE_OUT".into(),
            mode: OperationMode::Both,
        });

        let out = prepare_snapshot(&StoreSnapshot::empty(), &[spec]).unwrap();
        assert!(out.topics.is_empty());
        assert!(out.watermarks.is_empty());
    }

    #[test]
    fn split_omits_the_split_field_from_an_explicit_selection() {
        let spec = OperationSpec::SplitBy(SplitBySpec {
            input: TopicSelector {
                topic: "PARAM_VALUE".into(),
                source: None,
                instance: None,
            },
            field: "param_id".into(),
            fields: Some(vec!["param_id".into(), "param_value".into()]),
            output_template: "{topic}/{value}".into(),
            mode: OperationMode::Snapshot,
        });

        let out = prepare_snapshot(&operation_fixture(), &[spec]).unwrap();
        let topic = out
            .topics
            .iter()
            .find(|topic| topic.name == "PARAM_VALUE/MAX_SPEED")
            .unwrap();
        assert_eq!(
            topic
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            vec!["param_value"]
        );
    }

    #[test]
    fn merge_preserves_each_requested_field_order() {
        let spec = OperationSpec::Merge(MergeSpec {
            topics: vec![("ATTITUDE".into(), vec!["frame".into(), "roll".into()])],
            base_topic: "ATTITUDE".into(),
            output_topic: "ORDERED".into(),
            source: None,
            output_names: vec![vec!["frame".into(), "roll".into()]],
            mode: OperationMode::Snapshot,
        });

        let out = prepare_snapshot(&operation_fixture(), &[spec]).unwrap();
        assert_topic_utf8(&out.topics, "ORDERED", "frame", &["NED", "NED"]);
        assert_topic_f64(
            &out.topics,
            "ORDERED",
            "roll",
            &[0.0, std::f64::consts::FRAC_PI_2],
        );
    }

    #[test]
    fn both_merge_missing_base_still_watermarks_and_seeds_all_secondaries() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let gps = ids.add_topic(source, "GPS").unwrap();
        let status = ids.add_topic(source, "STATUS").unwrap();
        ids.add_field(gps, "alt").unwrap();
        ids.add_field(status, "mode").unwrap();
        let snap = StoreSnapshot::from_registry(
            &ids,
            [
                (
                    gps,
                    topic_store(
                        "GPS",
                        vec![
                            FieldSchema::new("alt", DataType::Float64, None::<String>, 1.0)
                                .unwrap(),
                        ],
                        vec![150, 250],
                        vec![Arc::new(Float64Array::from(vec![100.0, 110.0]))],
                    ),
                ),
                (
                    status,
                    topic_store(
                        "STATUS",
                        vec![
                            FieldSchema::new("mode", DataType::Utf8, None::<String>, 1.0).unwrap(),
                        ],
                        vec![175, 275],
                        vec![Arc::new(StringArray::from(vec!["AUTO", "RTL"]))],
                    ),
                ),
            ],
            0,
        )
        .unwrap();
        let spec = OperationSpec::Merge(MergeSpec {
            topics: vec![
                ("ATTITUDE".into(), vec!["roll".into()]),
                ("GPS".into(), vec!["alt".into()]),
                ("STATUS".into(), vec!["mode".into()]),
            ],
            base_topic: "ATTITUDE".into(),
            output_topic: "STATE".into(),
            source: None,
            output_names: vec![vec!["roll".into()], vec!["alt".into()], vec!["mode".into()]],
            mode: OperationMode::Both,
        });

        let out = prepare_snapshot(&snap, &[spec]).unwrap();
        assert!(out.topics.is_empty());
        assert_eq!(out.watermarks[&StreamKey::new(source, "GPS")], 250);
        assert_eq!(out.watermarks[&StreamKey::new(source, "STATUS")], 275);
        let seed = &out.merge_seeds[&(0, source)];
        assert_eq!(seed.f64("GPS", "alt"), Some((250, 110.0)));
        assert_eq!(seed.utf8("STATUS", "mode"), Some((275, "RTL")));
    }

    #[test]
    fn both_merge_missing_middle_input_still_processes_later_input() {
        let spec = OperationSpec::Merge(MergeSpec {
            topics: vec![
                ("ATTITUDE".into(), vec!["roll".into()]),
                ("MISSING".into(), vec!["value".into()]),
                ("GPS".into(), vec!["alt".into()]),
            ],
            base_topic: "ATTITUDE".into(),
            output_topic: "STATE".into(),
            source: None,
            output_names: vec![
                vec!["roll".into()],
                vec!["value".into()],
                vec!["alt".into()],
            ],
            mode: OperationMode::Both,
        });

        let out = prepare_snapshot(&operation_fixture(), &[spec]).unwrap();
        assert!(out.topics.is_empty());
        assert_eq!(
            out.watermarks[&StreamKey::new(SourceId(0), "ATTITUDE")],
            200
        );
        assert_eq!(out.watermarks[&StreamKey::new(SourceId(0), "GPS")], 150);
        assert_eq!(
            out.merge_seeds[&(0, SourceId(0))].f64("GPS", "alt"),
            Some((150, 100.0))
        );
    }

    #[test]
    fn merge_seed_preserves_empty_field_metadata_and_configured_topic_identity() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let base = ids.add_topic(source, "ATTITUDE").unwrap();
        let gps = ids.add_topic(source, "GPS[1]").unwrap();
        let status = ids.add_topic(source, "STATUS[1]").unwrap();
        ids.add_field(base, "roll").unwrap();
        ids.add_field(gps, "alt").unwrap();
        ids.add_field(status, "mode").unwrap();
        let status_schema = Arc::new(
            TopicSchema::new(
                "STATUS[1]",
                [FieldSchema::new("mode", DataType::Utf8, Some("state"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let snap = StoreSnapshot::from_registry(
            &ids,
            [
                (
                    base,
                    topic_store(
                        "ATTITUDE",
                        vec![
                            FieldSchema::new("roll", DataType::Float64, Some("rad"), 1.0).unwrap(),
                        ],
                        vec![200],
                        vec![Arc::new(Float64Array::from(vec![2.0]))],
                    ),
                ),
                (
                    gps,
                    topic_store(
                        "GPS[1]",
                        vec![FieldSchema::new("alt", DataType::Float64, Some("m"), 1.0).unwrap()],
                        vec![150],
                        vec![Arc::new(Float64Array::from(vec![100.0]))],
                    ),
                ),
                (status, Arc::new(TopicStore::new(status_schema))),
            ],
            0,
        )
        .unwrap();
        let spec = OperationSpec::Merge(MergeSpec {
            topics: vec![
                ("ATTITUDE".into(), vec!["roll".into()]),
                ("GPS".into(), vec!["alt".into()]),
                ("STATUS".into(), vec!["mode".into()]),
            ],
            base_topic: "ATTITUDE".into(),
            output_topic: "STATE".into(),
            source: None,
            output_names: vec![vec!["roll".into()], vec!["alt".into()], vec!["mode".into()]],
            mode: OperationMode::Both,
        });

        let out = prepare_snapshot(&snap, &[spec]).unwrap();
        let seed = &out.merge_seeds[&(0, source)];
        assert_eq!(seed.f64("GPS", "alt"), Some((150, 100.0)));
        assert_eq!(seed.unit("GPS", "alt"), Some(Some("m")));
        assert_eq!(seed.utf8("STATUS", "mode"), None);
        assert!(seed.is_utf8("STATUS", "mode"));
        assert_eq!(seed.unit("STATUS", "mode"), Some(Some("state")));
        assert_eq!(seed.f64("GPS[1]", "alt"), None);
    }

    #[test]
    fn merge_stable_sorts_cross_chunk_regressions_before_alignment_and_seeding() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let base = ids.add_topic(source, "BASE").unwrap();
        let secondary = ids.add_topic(source, "SECONDARY").unwrap();
        ids.add_field(base, "x").unwrap();
        ids.add_field(secondary, "value").unwrap();
        ids.add_field(secondary, "label").unwrap();
        let snap = StoreSnapshot::from_registry(
            &ids,
            [
                (
                    base,
                    chunked_topic_store(
                        "BASE",
                        vec![
                            FieldSchema::new("x", DataType::Float64, None::<String>, 1.0).unwrap(),
                        ],
                        vec![
                            (
                                vec![100, 300],
                                vec![Arc::new(Float64Array::from(vec![1.0, 3.0]))],
                            ),
                            (
                                vec![200, 400],
                                vec![Arc::new(Float64Array::from(vec![2.0, 4.0]))],
                            ),
                        ],
                    ),
                ),
                (
                    secondary,
                    chunked_topic_store(
                        "SECONDARY",
                        vec![
                            FieldSchema::new("value", DataType::Float64, None::<String>, 1.0)
                                .unwrap(),
                            FieldSchema::new("label", DataType::Utf8, None::<String>, 1.0).unwrap(),
                        ],
                        vec![
                            (
                                vec![150, 350],
                                vec![
                                    Arc::new(Float64Array::from(vec![15.0, 35.0])),
                                    Arc::new(StringArray::from(vec!["a", "c"])),
                                ],
                            ),
                            (
                                vec![250, 450],
                                vec![
                                    Arc::new(Float64Array::from(vec![25.0, 45.0])),
                                    Arc::new(StringArray::from(vec!["b", "d"])),
                                ],
                            ),
                        ],
                    ),
                ),
            ],
            0,
        )
        .unwrap();
        let spec = OperationSpec::Merge(MergeSpec {
            topics: vec![
                ("BASE".into(), vec!["x".into()]),
                ("SECONDARY".into(), vec!["value".into(), "label".into()]),
            ],
            base_topic: "BASE".into(),
            output_topic: "MERGED".into(),
            source: None,
            output_names: vec![vec!["x".into()], vec!["value".into(), "label".into()]],
            mode: OperationMode::Both,
        });

        let out = prepare_snapshot(&snap, &[spec]).unwrap();
        assert_topic_f64(&out.topics, "MERGED", "x", &[1.0, 2.0, 3.0, 4.0]);
        assert_topic_f64(
            &out.topics,
            "MERGED",
            "value",
            &[f64::NAN, 15.0, 25.0, 35.0],
        );
        assert_topic_utf8(&out.topics, "MERGED", "label", &["", "a", "b", "c"]);
        assert_eq!(out.watermarks[&StreamKey::new(source, "SECONDARY")], 450);
        let seed = &out.merge_seeds[&(0, source)];
        assert_eq!(seed.f64("SECONDARY", "value"), Some((450, 45.0)));
        assert_eq!(seed.utf8("SECONDARY", "label"), Some((450, "d")));
    }

    #[test]
    fn schema_only_topic_materializes_typed_empty_columns_and_validates_fields() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let empty = ids.add_topic(source, "EMPTY").unwrap();
        ids.add_field(empty, "value").unwrap();
        ids.add_field(empty, "label").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "EMPTY",
                [
                    FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("label", DataType::Utf8, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let snap =
            StoreSnapshot::from_registry(&ids, [(empty, Arc::new(TopicStore::new(schema)))], 0)
                .unwrap();
        let topic_match = crate::api::find_topics(&snap, Some("EMPTY"), None, None)
            .into_iter()
            .next()
            .unwrap();

        let topic = super::materialize_topic(&snap, topic_match.clone(), None).unwrap();
        assert!(topic.times.is_empty());
        assert!(matches!(&topic.fields[0].1, PendingColumn::F64(values) if values.is_empty()));
        assert!(matches!(&topic.fields[1].1, PendingColumn::Utf8(values) if values.is_empty()));
        assert!(super::materialize_topic(&snap, topic_match, Some(&["missing".into()])).is_err());

        let spec = OperationSpec::Transform(TransformSpec {
            input: TopicSelector {
                topic: "EMPTY".into(),
                source: None,
                instance: None,
            },
            multiplier: 2.0,
            offset: 1.0,
            fields: Some(vec!["value".into()]),
            unit: None,
            units: HashMap::new(),
            output_topic: "EMPTY_OUT".into(),
            mode: OperationMode::Both,
        });
        let out = prepare_snapshot(&snap, &[spec]).unwrap();
        assert!(out.topics.is_empty());
        assert!(out.watermarks.is_empty());
    }

    #[test]
    fn topic_without_usable_schema_follows_both_mode_missing_behavior() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        ids.add_topic(source, "SCHEMA_PENDING").unwrap();
        let snap = StoreSnapshot::from_registry(&ids, [], 0).unwrap();
        let spec = OperationSpec::Transform(TransformSpec {
            input: TopicSelector {
                topic: "SCHEMA_PENDING".into(),
                source: None,
                instance: None,
            },
            multiplier: 2.0,
            offset: 1.0,
            fields: Some(vec!["value".into()]),
            unit: None,
            units: HashMap::new(),
            output_topic: "OUT".into(),
            mode: OperationMode::Both,
        });

        let out = prepare_snapshot(&snap, &[spec]).unwrap();
        assert!(out.topics.is_empty());
        assert!(out.watermarks.is_empty());
    }

    #[test]
    fn merge_rejects_short_and_extra_output_name_shapes() {
        let mut specs = Vec::new();
        for output_names in [
            vec![vec!["roll".into()]],
            vec![vec!["roll".into()], vec![]],
            vec![vec!["roll".into()], vec!["alt".into(), "extra".into()]],
            vec![vec!["roll".into()], vec!["alt".into()], vec![]],
        ] {
            specs.push(OperationSpec::Merge(MergeSpec {
                topics: vec![
                    ("ATTITUDE".into(), vec!["roll".into()]),
                    ("GPS".into(), vec!["alt".into()]),
                ],
                base_topic: "ATTITUDE".into(),
                output_topic: "STATE".into(),
                source: None,
                output_names,
                mode: OperationMode::Snapshot,
            }));
        }

        for spec in specs {
            assert!(
                prepare_snapshot(&operation_fixture(), &[spec]).is_err(),
                "invalid output_names shape was accepted"
            );
        }
    }

    #[test]
    fn schema_field_without_live_identity_returns_an_error() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let topic = ids.add_topic(source, "MISMATCH").unwrap();
        ids.add_field(topic, "known").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "MISMATCH",
                [
                    FieldSchema::new("known", DataType::Float64, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("schema_only", DataType::Utf8, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let snap =
            StoreSnapshot::from_registry(&ids, [(topic, Arc::new(TopicStore::new(schema)))], 0)
                .unwrap();
        let topic_match = crate::api::find_topics(&snap, Some("MISMATCH"), None, None)
            .into_iter()
            .next()
            .unwrap();

        let result =
            super::materialize_topic(&snap, topic_match, Some(&["schema_only".to_owned()]));
        match result {
            Err(error) => assert_eq!(
                error,
                "topic 'MISMATCH' schema field 'schema_only' has no live identity field"
            ),
            Ok(_) => panic!("schema/identity disagreement was silently accepted"),
        }
    }

    #[test]
    fn live_identity_field_without_schema_returns_an_error() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let topic = ids.add_topic(source, "MISMATCH").unwrap();
        ids.add_field(topic, "known").unwrap();
        ids.add_field(topic, "identity_only").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "MISMATCH",
                [FieldSchema::new("known", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let snap =
            StoreSnapshot::from_registry(&ids, [(topic, Arc::new(TopicStore::new(schema)))], 0)
                .unwrap();
        let topic_match = crate::api::find_topics(&snap, Some("MISMATCH"), None, None)
            .into_iter()
            .next()
            .unwrap();

        let result = super::materialize_topic(&snap, topic_match, None);
        match result {
            Err(error) => assert_eq!(
                error,
                "topic 'MISMATCH' live identity field 'identity_only' is missing from schema"
            ),
            Ok(_) => panic!("schema/identity disagreement was silently accepted"),
        }
    }

    #[test]
    fn missing_topic_candidates_include_registry_topics_without_stores() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        ids.add_topic(source, "REGISTRY_ONLY").unwrap();
        let available = ids.add_topic(source, "AVAILABLE").unwrap();
        let schema = Arc::new(TopicSchema::new("AVAILABLE", []).unwrap());
        let snap =
            StoreSnapshot::from_registry(&ids, [(available, Arc::new(TopicStore::new(schema)))], 0)
                .unwrap();
        let selector = TopicSelector {
            topic: "MISSING".into(),
            source: None,
            instance: None,
        };

        let error = super::resolve_topic(&snap, &selector, OperationMode::Snapshot).unwrap_err();
        assert_eq!(
            error,
            "topic 'MISSING' not found; candidates: flight/REGISTRY_ONLY, flight/AVAILABLE"
        );
    }
}
