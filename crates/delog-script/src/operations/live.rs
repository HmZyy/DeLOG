use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use arrow::datatypes::DataType;
use delog_core::field_view::{array_row_as_f64, array_row_as_str};
use delog_core::identity::SourceId;
use delog_core::ingest::ParsedBatch;

use crate::api::{PendingColumn, PendingTopic, parse_topic_instance, topic_matches};
use crate::emit::prepare_topics;
use crate::operations::snapshot::{
    MergeSeed, SeedField, StreamKey, split_key, pending_topic, slice_column,
};
use crate::operations::{
    SplitBySpec, MergeSpec, OperationSpec, TopicRegistry, TopicSelector, TransformSpec,
};

type EmittedSchema = Vec<(String, DataType, Option<String>)>;

#[derive(Debug, Clone)]
enum ColumnHistory {
    F64 {
        times: Vec<i64>,
        values: Vec<f64>,
    },
    Utf8 {
        times: Vec<i64>,
        values: Vec<String>,
    },
}

impl ColumnHistory {
    fn insert(&mut self, times: &[i64], values: &PendingColumn) -> Result<(), String> {
        match (self, values) {
            (
                Self::F64 {
                    times: history_times,
                    values: history_values,
                },
                PendingColumn::F64(values),
            ) => insert_sorted(history_times, history_values, times, values),
            (
                Self::Utf8 {
                    times: history_times,
                    values: history_values,
                },
                PendingColumn::Utf8(values),
            ) => insert_sorted(history_times, history_values, times, values),
            (Self::F64 { .. }, PendingColumn::Utf8(_))
            | (Self::Utf8 { .. }, PendingColumn::F64(_)) => {
                return Err("merge field type changed between batches".to_owned());
            }
        }
        Ok(())
    }

    fn align(&self, base_times: &[i64]) -> PendingColumn {
        match self {
            Self::F64 { times, values } => PendingColumn::F64(
                base_times
                    .iter()
                    .map(|time| {
                        previous_index(times, *time).map_or(f64::NAN, |index| values[index])
                    })
                    .collect(),
            ),
            Self::Utf8 { times, values } => PendingColumn::Utf8(
                base_times
                    .iter()
                    .map(|time| {
                        previous_index(times, *time)
                            .map_or_else(String::new, |index| values[index].clone())
                    })
                    .collect(),
            ),
        }
    }

    fn prune(&mut self, cutoff: i64) {
        let times = match self {
            Self::F64 { times, .. } | Self::Utf8 { times, .. } => times,
        };
        let remove = times
            .partition_point(|time| *time <= cutoff)
            .saturating_sub(1);
        if remove == 0 {
            return;
        }
        match self {
            Self::F64 { times, values } => {
                times.drain(..remove);
                values.drain(..remove);
            }
            Self::Utf8 { times, values } => {
                times.drain(..remove);
                values.drain(..remove);
            }
        }
    }
}

fn insert_sorted<T: Clone>(
    history_times: &mut Vec<i64>,
    history_values: &mut Vec<T>,
    times: &[i64],
    values: &[T],
) {
    for (&time, value) in times.iter().zip(values) {
        let index = history_times.partition_point(|existing| *existing <= time);
        history_times.insert(index, time);
        history_values.insert(index, value.clone());
    }
}

fn previous_index(times: &[i64], time: i64) -> Option<usize> {
    times
        .partition_point(|candidate| *candidate <= time)
        .checked_sub(1)
}

#[derive(Debug, Default)]
struct MergeState {
    histories: HashMap<(String, String), ColumnHistory>,
    units: HashMap<(String, String), Option<String>>,
    last_base_time: Option<i64>,
    pending_base: Vec<PendingBaseBatch>,
}

#[derive(Debug)]
struct PendingBaseBatch {
    times: Vec<i64>,
    fields: Vec<(String, PendingColumn, Option<String>)>,
}

struct MergeOutput {
    topics: Vec<PendingTopic>,
    consumed_pending: bool,
    cutoff: Option<i64>,
}

impl MergeState {
    fn from_seed(seed: MergeSeed) -> Self {
        let mut state = Self::default();
        for (key, field) in seed.fields {
            let (history, unit) = match field {
                SeedField::F64 { unit, sample } => {
                    let (times, values) = sample.map_or_else(
                        || (Vec::new(), Vec::new()),
                        |(time, value)| (vec![time], vec![value]),
                    );
                    (ColumnHistory::F64 { times, values }, unit)
                }
                SeedField::Utf8 { unit, sample } => {
                    let (times, values) = sample.map_or_else(
                        || (Vec::new(), Vec::new()),
                        |(time, value)| (vec![time], vec![value]),
                    );
                    (ColumnHistory::Utf8 { times, values }, unit)
                }
            };
            state.histories.insert(key.clone(), history);
            state.units.insert(key, unit);
        }
        state
    }
}

pub struct ActiveOperation {
    operation_index: usize,
    spec: OperationSpec,
    source: SourceId,
    watermarks: HashMap<StreamKey, i64>,
    merge: HashMap<SourceId, MergeState>,
    emitted_schemas: HashMap<String, EmittedSchema>,
    registry: Rc<RefCell<TopicRegistry>>,
    consecutive_errors: u8,
    disabled: bool,
}

impl ActiveOperation {
    pub fn new(
        spec: OperationSpec,
        derived_source: SourceId,
        watermarks: HashMap<StreamKey, i64>,
        merge_seeds: HashMap<SourceId, MergeSeed>,
    ) -> Self {
        let mut registry = TopicRegistry::default();
        registry
            .preclaim_static(std::slice::from_ref(&spec))
            .expect("one operation cannot have duplicate static ownership");
        Self::with_registry(
            0,
            spec,
            derived_source,
            watermarks,
            merge_seeds,
            Rc::new(RefCell::new(registry)),
        )
    }

    pub(crate) fn with_registry(
        operation_index: usize,
        spec: OperationSpec,
        derived_source: SourceId,
        watermarks: HashMap<StreamKey, i64>,
        merge_seeds: HashMap<SourceId, MergeSeed>,
        registry: Rc<RefCell<TopicRegistry>>,
    ) -> Self {
        Self {
            operation_index,
            spec,
            source: derived_source,
            watermarks,
            merge: merge_seeds
                .into_iter()
                .map(|(source, seed)| (source, MergeState::from_seed(seed)))
                .collect(),
            emitted_schemas: HashMap::new(),
            registry,
            consecutive_errors: 0,
            disabled: false,
        }
    }

    pub fn process(
        &mut self,
        batch: &ParsedBatch,
        source_label: &str,
    ) -> Result<Vec<ParsedBatch>, String> {
        if self.disabled || !self.matches(batch, source_label) {
            return Ok(Vec::new());
        }

        let result = self.process_raw(batch, source_label);
        match &result {
            Ok(_) => self.consecutive_errors = 0,
            Err(_) => self.consecutive_errors = self.consecutive_errors.saturating_add(1),
        }
        result
    }

    pub fn matches(&self, batch: &ParsedBatch, source_label: &str) -> bool {
        if self.disabled || source_label.starts_with("script:") {
            return false;
        }
        match &self.spec {
            OperationSpec::Transform(spec) => {
                spec.mode.wants_live() && selector_matches(&spec.input, batch.topic(), source_label)
            }
            OperationSpec::SplitBy(spec) => {
                spec.mode.wants_live() && selector_matches(&spec.input, batch.topic(), source_label)
            }
            OperationSpec::Merge(spec) => {
                spec.mode.wants_live()
                    && spec
                        .source
                        .as_deref()
                        .is_none_or(|source| source == source_label)
                    && spec
                        .topics
                        .iter()
                        .any(|(topic, _)| configured_topic_matches(topic, batch.topic()))
            }
        }
    }

    pub fn set_source(&mut self, source: SourceId) {
        self.source = source;
    }

    pub fn consecutive_errors(&self) -> u8 {
        self.consecutive_errors
    }

    pub fn disable(&mut self) {
        self.disabled = true;
    }

    pub fn description(&self) -> String {
        match &self.spec {
            OperationSpec::Transform(spec) => format!("transform({})", spec.input.topic),
            OperationSpec::SplitBy(spec) => format!("split_by({})", spec.input.topic),
            OperationSpec::Merge(spec) => format!("merge({})", spec.base_topic),
        }
    }

    fn process_raw(
        &mut self,
        batch: &ParsedBatch,
        source_label: &str,
    ) -> Result<Vec<ParsedBatch>, String> {
        if let OperationSpec::Merge(spec) = &self.spec
            && spec.mode.wants_live()
        {
            let spec = spec.clone();
            let rows = self.watermark_rows(batch, batch.topic());
            let output = {
                let state = self.merge.entry(batch.source).or_default();
                execute_merge(batch, source_label, &spec, rows, state)?
            };
            let batches = self.pin_and_build(output.topics)?;
            if output.consumed_pending || output.cutoff.is_some() {
                let state = self
                    .merge
                    .get_mut(&batch.source)
                    .expect("merge state was created before execution");
                if output.consumed_pending {
                    state.pending_base.clear();
                }
                if let Some(last_time) = output.cutoff {
                    let cutoff = state
                        .last_base_time
                        .map_or(last_time, |previous| previous.max(last_time));
                    state.last_base_time = Some(cutoff);
                    for history in state.histories.values_mut() {
                        history.prune(cutoff);
                    }
                }
            }
            return Ok(batches);
        }

        let topics = match &self.spec {
            OperationSpec::Transform(spec)
                if spec.mode.wants_live()
                    && selector_matches(&spec.input, batch.topic(), source_label) =>
            {
                execute_transform(batch, spec, self.watermark_rows(batch, batch.topic()))?
            }
            OperationSpec::SplitBy(spec)
                if spec.mode.wants_live()
                    && selector_matches(&spec.input, batch.topic(), source_label) =>
            {
                execute_split(batch, spec, self.watermark_rows(batch, batch.topic()))?
            }
            _ => Vec::new(),
        };

        self.pin_and_build(topics)
    }

    fn watermark_rows(&self, batch: &ParsedBatch, topic: &str) -> Vec<usize> {
        let watermark = self
            .watermarks
            .get(&StreamKey::new(batch.source, topic))
            .copied();
        (0..batch.rows())
            .filter(|&row| {
                watermark.is_none_or(|watermark| batch.timestamps.value(row) > watermark)
            })
            .collect()
    }

    fn pin_and_build(&mut self, topics: Vec<PendingTopic>) -> Result<Vec<ParsedBatch>, String> {
        let schemas = topics
            .iter()
            .map(|topic| {
                let schema = topic
                    .fields
                    .iter()
                    .map(|field| {
                        let dtype = match &field.values {
                            PendingColumn::F64(_) => DataType::Float64,
                            PendingColumn::Utf8(_) => DataType::Utf8,
                        };
                        (field.name.clone(), dtype, field.unit.clone())
                    })
                    .collect::<Vec<_>>();
                (topic.name.clone(), schema)
            })
            .collect::<Vec<_>>();

        let prepared = prepare_topics(&topics)?;

        for (topic, schema) in &schemas {
            if let Some(pinned) = self.emitted_schemas.get(topic)
                && pinned != schema
            {
                return Err(format!(
                    "output topic '{topic}' schema changed from {pinned:?} to {schema:?}"
                ));
            }
        }
        let claims = schemas
            .iter()
            .map(|(topic, schema)| (topic.clone(), Some(schema.clone())))
            .collect::<Vec<_>>();
        self.registry
            .borrow_mut()
            .claim_batch(self.operation_index, &claims)?;
        for (topic, schema) in schemas {
            self.emitted_schemas.entry(topic).or_insert(schema);
        }

        Ok(prepared.into_batches(self.source))
    }
}

fn selector_matches(selector: &TopicSelector, topic: &str, source_label: &str) -> bool {
    if selector
        .source
        .as_deref()
        .is_some_and(|source| source != source_label)
    {
        return false;
    }
    let (base, instance) = parse_topic_instance(topic);
    topic_matches(
        topic,
        &base,
        instance,
        Some(&selector.topic),
        selector.instance,
    )
}

fn validate_batch(batch: &ParsedBatch) -> Result<(), String> {
    if batch.columns.len() != batch.schema.len() {
        return Err(format!(
            "topic '{}' schema has {} fields but batch has {} columns",
            batch.topic(),
            batch.schema.len(),
            batch.columns.len()
        ));
    }
    for (field, column) in batch.schema.fields().iter().zip(&batch.columns) {
        if column.len() != batch.rows() {
            return Err(format!(
                "topic '{}' field '{}' has {} rows but timestamps have {}",
                batch.topic(),
                field.name,
                column.len(),
                batch.rows()
            ));
        }
        if column.data_type() != &field.dtype {
            return Err(format!(
                "topic '{}' field '{}' schema type {:?} does not match column type {:?}",
                batch.topic(),
                field.name,
                field.dtype,
                column.data_type()
            ));
        }
    }
    Ok(())
}

fn materialize_columns(
    batch: &ParsedBatch,
    rows: &[usize],
) -> Result<Vec<(String, PendingColumn, Option<String>)>, String> {
    validate_batch(batch)?;
    batch
        .schema
        .fields()
        .iter()
        .zip(&batch.columns)
        .map(|(field, column)| {
            let values = if field.is_string() {
                PendingColumn::Utf8(
                    rows.iter()
                        .map(|&row| {
                            array_row_as_str(column.as_ref(), row)
                                .unwrap_or_default()
                                .to_owned()
                        })
                        .collect(),
                )
            } else if field.is_numeric() || field.dtype == DataType::Boolean {
                PendingColumn::F64(
                    rows.iter()
                        .map(|&row| array_row_as_f64(column.as_ref(), row))
                        .collect(),
                )
            } else {
                return Err(format!(
                    "topic '{}' field '{}' has unsupported live type {:?}",
                    batch.topic(),
                    field.name,
                    field.dtype
                ));
            };
            Ok((field.name.clone(), values, field.unit.clone()))
        })
        .collect()
}

fn validate_requested_fields(
    batch: &ParsedBatch,
    requested: Option<&[String]>,
) -> Result<(), String> {
    if let Some(requested) = requested {
        for name in requested {
            if batch.schema.field_by_name(name).is_none() {
                return Err(format!(
                    "field '{name}' not found in topic '{}'",
                    batch.topic()
                ));
            }
        }
    }
    Ok(())
}

fn execute_transform(
    batch: &ParsedBatch,
    spec: &TransformSpec,
    rows: Vec<usize>,
) -> Result<Vec<PendingTopic>, String> {
    validate_requested_fields(batch, spec.fields.as_deref())?;
    let mut fields = materialize_columns(batch, &rows)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    for (name, values, unit) in &mut fields {
        let selected = spec
            .fields
            .as_ref()
            .is_none_or(|selected| selected.iter().any(|field| field == name));
        if selected && let PendingColumn::F64(values) = values {
            for value in values {
                *value = *value * spec.multiplier + spec.offset;
            }
            if let Some(override_unit) = spec.units.get(name).or(spec.unit.as_ref()) {
                *unit = Some(override_unit.clone());
            }
        }
    }

    let times = rows
        .iter()
        .map(|&row| batch.timestamps.value(row))
        .collect();
    Ok(vec![pending_topic(
        spec.output_topic.clone(),
        times,
        fields,
    )?])
}

fn execute_split(
    batch: &ParsedBatch,
    spec: &SplitBySpec,
    rows: Vec<usize>,
) -> Result<Vec<PendingTopic>, String> {
    validate_requested_fields(batch, spec.fields.as_deref())?;
    if batch.schema.field_by_name(&spec.field).is_none() {
        return Err(format!(
            "field '{}' not found in topic '{}'",
            spec.field,
            batch.topic()
        ));
    }
    let fields = materialize_columns(batch, &rows)?;
    let split_column = fields
        .iter()
        .find(|(name, _, _)| name == &spec.field)
        .expect("split field was validated");
    let selected = match &spec.fields {
        Some(requested) => requested
            .iter()
            .filter(|name| *name != &spec.field)
            .cloned()
            .collect::<Vec<_>>(),
        None => fields
            .iter()
            .filter_map(|(name, _, _)| (name != &spec.field).then_some(name.clone()))
            .collect(),
    };

    let mut groups = Vec::<(String, Vec<usize>)>::new();
    let mut positions = HashMap::<String, usize>::new();
    for row in 0..rows.len() {
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

    groups
        .into_iter()
        .map(|(key, grouped_rows)| {
            let topic = spec
                .output_template
                .replace("{topic}", &spec.input.topic)
                .replace("{value}", &key);
            let times = grouped_rows
                .iter()
                .map(|&row| batch.timestamps.value(rows[row]))
                .collect();
            let output_fields = selected.iter().map(|selected_name| {
                let (name, column, unit) = fields
                    .iter()
                    .find(|(name, _, _)| name == selected_name)
                    .expect("selected fields were validated");
                (
                    name.clone(),
                    slice_column(column, &grouped_rows),
                    unit.clone(),
                )
            });
            pending_topic(topic, times, output_fields)
        })
        .collect()
}

fn configured_topic_matches(configured: &str, actual: &str) -> bool {
    let (base, instance) = parse_topic_instance(actual);
    topic_matches(actual, &base, instance, Some(configured), None)
}

fn selected_columns(
    batch: &ParsedBatch,
    names: &[String],
    rows: &[usize],
) -> Result<Vec<(String, PendingColumn, Option<String>)>, String> {
    validate_requested_fields(batch, Some(names))?;
    let columns = materialize_columns(batch, rows)?;
    Ok(names
        .iter()
        .map(|name| {
            columns
                .iter()
                .find(|(candidate, _, _)| candidate == name)
                .expect("selected merge fields were validated")
                .clone()
        })
        .collect())
}

fn execute_merge(
    batch: &ParsedBatch,
    source_label: &str,
    spec: &MergeSpec,
    rows: Vec<usize>,
    state: &mut MergeState,
) -> Result<MergeOutput, String> {
    let empty = || MergeOutput {
        topics: Vec::new(),
        consumed_pending: false,
        cutoff: None,
    };
    if spec
        .source
        .as_deref()
        .is_some_and(|source| source != source_label)
    {
        return Ok(empty());
    }
    if spec.output_names.len() != spec.topics.len()
        || spec
            .output_names
            .iter()
            .zip(&spec.topics)
            .any(|(output_names, (_, fields))| output_names.len() != fields.len())
    {
        return Err("merge output field names do not match selected fields".to_owned());
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
    let Some(input_index) = spec
        .topics
        .iter()
        .position(|(topic, _)| configured_topic_matches(topic, batch.topic()))
    else {
        return Ok(empty());
    };

    let (input_topic, requested_fields) = &spec.topics[input_index];
    let fields = selected_columns(batch, requested_fields, &rows)?;
    let times = rows
        .iter()
        .map(|&row| batch.timestamps.value(row))
        .collect::<Vec<_>>();
    if input_index != base_index {
        for (field, values, unit) in &fields {
            let key = (input_topic.clone(), field.clone());
            if let Some(existing_unit) = state.units.get(&key)
                && existing_unit != unit
            {
                return Err(format!(
                    "merge field '{}/{}' unit changed from {:?} to {:?}",
                    key.0, key.1, existing_unit, unit
                ));
            }
            if let Some(history) = state.histories.get(&key)
                && !matches!(
                    (history, values),
                    (ColumnHistory::F64 { .. }, PendingColumn::F64(_))
                        | (ColumnHistory::Utf8 { .. }, PendingColumn::Utf8(_))
                )
            {
                return Err(format!(
                    "merge field '{}/{}' type changed between batches",
                    key.0, key.1
                ));
            }
        }
        for (field, values, unit) in fields {
            let key = (input_topic.clone(), field);
            match state.histories.entry(key.clone()) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().insert(&times, &values)?;
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let history = match values {
                        PendingColumn::F64(values) => ColumnHistory::F64 {
                            times: times.clone(),
                            values,
                        },
                        PendingColumn::Utf8(values) => ColumnHistory::Utf8 {
                            times: times.clone(),
                            values,
                        },
                    };
                    entry.insert(history);
                }
            }
            state.units.entry(key).or_insert(unit);
        }
        if merge_secondary_schema_ready(spec, base_index, state) && !state.pending_base.is_empty() {
            let topic = build_merged_base_topic(spec, base_index, &state.pending_base, state)?;
            let cutoff = topic.times.iter().copied().max();
            return Ok(MergeOutput {
                topics: vec![topic],
                consumed_pending: true,
                cutoff,
            });
        }
        return Ok(empty());
    }
    if rows.is_empty() {
        return Ok(empty());
    }
    let current = PendingBaseBatch { times, fields };
    validate_pending_base_schema(state.pending_base.first(), &current)?;
    if !merge_secondary_schema_ready(spec, base_index, state) {
        state.pending_base.push(current);
        return Ok(empty());
    }

    let mut bases = state.pending_base.iter().collect::<Vec<_>>();
    bases.push(&current);
    let topic = build_merged_base_topic_refs(spec, base_index, &bases, state)?;
    let cutoff = topic.times.iter().copied().max();
    Ok(MergeOutput {
        topics: vec![topic],
        consumed_pending: !state.pending_base.is_empty(),
        cutoff,
    })
}

fn merge_secondary_schema_ready(spec: &MergeSpec, base_index: usize, state: &MergeState) -> bool {
    spec.topics
        .iter()
        .enumerate()
        .all(|(topic_index, (topic, fields))| {
            topic_index == base_index
                || fields.iter().all(|field| {
                    state
                        .histories
                        .contains_key(&(topic.clone(), field.clone()))
                })
        })
}

fn validate_pending_base_schema(
    expected: Option<&PendingBaseBatch>,
    actual: &PendingBaseBatch,
) -> Result<(), String> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let schema = |batch: &PendingBaseBatch| {
        batch
            .fields
            .iter()
            .map(|(name, values, unit)| {
                (
                    name.clone(),
                    matches!(values, PendingColumn::Utf8(_)),
                    unit.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    if schema(expected) != schema(actual) {
        return Err("merge base schema changed while waiting for secondary metadata".to_owned());
    }
    Ok(())
}

fn build_merged_base_topic(
    spec: &MergeSpec,
    base_index: usize,
    bases: &[PendingBaseBatch],
    state: &MergeState,
) -> Result<PendingTopic, String> {
    build_merged_base_topic_refs(spec, base_index, &bases.iter().collect::<Vec<_>>(), state)
}

fn build_merged_base_topic_refs(
    spec: &MergeSpec,
    base_index: usize,
    bases: &[&PendingBaseBatch],
    state: &MergeState,
) -> Result<PendingTopic, String> {
    let mut rows = bases
        .iter()
        .enumerate()
        .flat_map(|(batch_index, batch)| {
            batch
                .times
                .iter()
                .copied()
                .enumerate()
                .map(move |(row, time)| (time, batch_index, row))
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|&(time, batch_index, row)| (time, batch_index, row));
    let times = rows.iter().map(|&(time, _, _)| time).collect::<Vec<_>>();

    let first = bases
        .first()
        .expect("merged base output has at least one batch");
    let mut output_fields = Vec::new();
    for (topic_index, (topic, requested)) in spec.topics.iter().enumerate() {
        if topic_index == base_index {
            for (field_index, (_, first_values, unit)) in first.fields.iter().enumerate() {
                let values = match first_values {
                    PendingColumn::F64(_) => PendingColumn::F64(
                        rows.iter()
                            .map(
                                |&(_, batch, row)| match &bases[batch].fields[field_index].1 {
                                    PendingColumn::F64(values) => values[row],
                                    PendingColumn::Utf8(_) => {
                                        unreachable!("base schemas were validated")
                                    }
                                },
                            )
                            .collect(),
                    ),
                    PendingColumn::Utf8(_) => PendingColumn::Utf8(
                        rows.iter()
                            .map(
                                |&(_, batch, row)| match &bases[batch].fields[field_index].1 {
                                    PendingColumn::Utf8(values) => values[row].clone(),
                                    PendingColumn::F64(_) => {
                                        unreachable!("base schemas were validated")
                                    }
                                },
                            )
                            .collect(),
                    ),
                };
                output_fields.push((
                    spec.output_names[base_index][field_index].clone(),
                    values,
                    unit.clone(),
                ));
            }
            continue;
        }
        for (field_index, field) in requested.iter().enumerate() {
            let key = (topic.clone(), field.clone());
            let history = state
                .histories
                .get(&key)
                .expect("secondary metadata readiness was checked");
            output_fields.push((
                spec.output_names[topic_index][field_index].clone(),
                history.align(&times),
                state.units.get(&key).cloned().flatten(),
            ));
        }
    }
    pending_topic(spec.output_topic.clone(), times, output_fields)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::Arc;

    use arrow::array::{Array, ArrayRef, Float64Array, Int64Array, StringArray};
    use arrow::datatypes::DataType;
    use delog_core::identity::SourceId;
    use delog_core::ingest::ParsedBatch;
    use delog_core::schema::{FieldSchema, TopicSchema};

    use crate::api::{PendingColumn, PendingField, PendingTopic};
    use crate::operations::snapshot::{MergeSeed, SeedField, StreamKey};
    use crate::operations::{
        SplitBySpec, MergeSpec, OperationMode, OperationSpec, TopicRegistry, TopicSelector,
        TransformSpec,
    };

    use super::{ActiveOperation, ColumnHistory};

    fn batch(
        source: SourceId,
        topic: &str,
        fields: Vec<FieldSchema>,
        times: &[i64],
        columns: Vec<ArrayRef>,
    ) -> ParsedBatch {
        ParsedBatch::new(
            source,
            Arc::new(TopicSchema::new(topic, fields).unwrap()),
            Int64Array::from(times.to_vec()),
            columns,
        )
    }

    fn attitude_batch(times: &[i64], rolls: &[f64]) -> ParsedBatch {
        batch(
            SourceId(1),
            "ATTITUDE",
            vec![
                FieldSchema::new("roll", DataType::Float64, Some("rad"), 1.0).unwrap(),
                FieldSchema::new("frame", DataType::Utf8, None::<String>, 1.0).unwrap(),
            ],
            times,
            vec![
                Arc::new(Float64Array::from(rolls.to_vec())),
                Arc::new(StringArray::from(vec!["NED"; times.len()])),
            ],
        )
    }

    fn param_batch() -> ParsedBatch {
        batch(
            SourceId(1),
            "PARAM_VALUE",
            vec![
                FieldSchema::new("param_id", DataType::Utf8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("param_value", DataType::Float64, None::<String>, 1.0).unwrap(),
            ],
            &[100, 110, 120],
            vec![
                Arc::new(StringArray::from(vec![
                    "MAX_SPEED",
                    "MIN_SPEED",
                    "MAX_SPEED",
                ])),
                Arc::new(Float64Array::from(vec![12.0, 5.0, 14.0])),
            ],
        )
    }

    fn param_batch_with(keys: &[&str], values: &[f64]) -> ParsedBatch {
        batch(
            SourceId(1),
            "PARAM_VALUE",
            vec![
                FieldSchema::new("param_id", DataType::Utf8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("param_value", DataType::Float64, None::<String>, 1.0).unwrap(),
            ],
            &(0..keys.len() as i64).collect::<Vec<_>>(),
            vec![
                Arc::new(StringArray::from(keys.to_vec())),
                Arc::new(Float64Array::from(values.to_vec())),
            ],
        )
    }

    fn gps_batch(source: SourceId, times: &[i64], altitudes: &[f64]) -> ParsedBatch {
        batch(
            source,
            "GPS",
            vec![FieldSchema::new("alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            times,
            vec![Arc::new(Float64Array::from(altitudes.to_vec()))],
        )
    }

    fn status_batch(source: SourceId, times: &[i64], modes: &[&str]) -> ParsedBatch {
        batch(
            source,
            "STATUS",
            vec![FieldSchema::new("mode", DataType::Utf8, None::<String>, 1.0).unwrap()],
            times,
            vec![Arc::new(StringArray::from(modes.to_vec()))],
        )
    }

    fn transform_operation_with_watermark(watermark: i64) -> ActiveOperation {
        ActiveOperation::new(
            OperationSpec::Transform(TransformSpec {
                input: TopicSelector {
                    topic: "ATTITUDE".into(),
                    source: None,
                    instance: None,
                },
                multiplier: 10.0,
                offset: 95.0,
                fields: Some(vec!["roll".into()]),
                unit: Some("deg".into()),
                units: HashMap::new(),
                output_topic: "ATTITUDE_DEG".into(),
                mode: OperationMode::Both,
            }),
            SourceId(99),
            HashMap::from([(StreamKey::new(SourceId(1), "ATTITUDE"), watermark)]),
            HashMap::<SourceId, MergeSeed>::new(),
        )
    }

    fn split_operation() -> ActiveOperation {
        ActiveOperation::new(
            OperationSpec::SplitBy(SplitBySpec {
                input: TopicSelector {
                    topic: "PARAM_VALUE".into(),
                    source: None,
                    instance: None,
                },
                field: "param_id".into(),
                fields: Some(vec!["param_value".into()]),
                output_template: "{topic}/{value}".into(),
                mode: OperationMode::Live,
            }),
            SourceId(99),
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn merge_operation() -> ActiveOperation {
        merge_operation_with(HashMap::new(), HashMap::new())
    }

    fn merge_operation_with(
        watermarks: HashMap<StreamKey, i64>,
        seeds: HashMap<SourceId, MergeSeed>,
    ) -> ActiveOperation {
        ActiveOperation::new(
            OperationSpec::Merge(MergeSpec {
                topics: vec![
                    ("ATTITUDE".into(), vec!["roll".into()]),
                    ("GPS".into(), vec!["alt".into()]),
                    ("STATUS".into(), vec!["mode".into()]),
                ],
                base_topic: "ATTITUDE".into(),
                output_topic: "STATE".into(),
                source: None,
                output_names: vec![vec!["roll".into()], vec!["alt".into()], vec!["mode".into()]],
                mode: OperationMode::Live,
            }),
            SourceId(99),
            watermarks,
            seeds,
        )
    }

    fn f64_values(batch: &ParsedBatch, field: &str) -> Vec<f64> {
        let index = batch.schema.field_index(field).unwrap();
        batch.columns[index]
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap()
            .values()
            .to_vec()
    }

    fn utf8_values<'a>(batch: &'a ParsedBatch, field: &str) -> Vec<&'a str> {
        let index = batch.schema.field_index(field).unwrap();
        let values = batch.columns[index]
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        (0..values.len()).map(|row| values.value(row)).collect()
    }

    #[test]
    fn live_transform_drops_snapshot_rows_and_preserves_strings() {
        let mut op = transform_operation_with_watermark(200);
        let out = op
            .process(&attitude_batch(&[200, 300], &[1.0, 2.0]), "live")
            .unwrap();
        assert_eq!(out[0].timestamps.values(), &[300]);
        assert_eq!(f64_values(&out[0], "roll"), [115.0]);
        assert_eq!(utf8_values(&out[0], "frame"), ["NED"]);
        assert_eq!(
            out[0].schema.field_by_name("roll").unwrap().unit.as_deref(),
            Some("deg")
        );
    }

    #[test]
    fn live_split_creates_one_batch_per_nonempty_key() {
        let mut op = split_operation();
        let out = op.process(&param_batch(), "live").unwrap();
        assert_eq!(
            out.iter().map(ParsedBatch::topic).collect::<Vec<_>>(),
            ["PARAM_VALUE/MAX_SPEED", "PARAM_VALUE/MIN_SPEED"]
        );
        assert_eq!(out[0].timestamps.values(), &[100, 120]);
        assert_eq!(f64_values(&out[0], "param_value"), [12.0, 14.0]);
    }

    #[test]
    fn dynamic_split_collision_is_atomic_and_counts_as_a_matching_error() {
        let registry = Rc::new(RefCell::new(TopicRegistry::default()));
        let make = |index| {
            ActiveOperation::with_registry(
                index,
                OperationSpec::SplitBy(SplitBySpec {
                    input: TopicSelector {
                        topic: "PARAM_VALUE".into(),
                        source: None,
                        instance: None,
                    },
                    field: "param_id".into(),
                    fields: Some(vec!["param_value".into()]),
                    output_template: "GROUP/{value}".into(),
                    mode: OperationMode::Live,
                }),
                SourceId(99),
                HashMap::new(),
                HashMap::new(),
                Rc::clone(&registry),
            )
        };
        let mut owner = make(0);
        let mut colliding = make(1);
        let mut later = make(2);

        owner
            .process(&param_batch_with(&["MIN_SPEED"], &[5.0]), "live")
            .unwrap();
        for failure in 1..=3 {
            let error = colliding
                .process(
                    &param_batch_with(&["MAX_SPEED", "MIN_SPEED"], &[12.0, 5.0]),
                    "live",
                )
                .unwrap_err();
            assert!(error.contains("owned by operation 0"), "{error}");
            assert_eq!(colliding.consecutive_errors(), failure);
        }
        let output = later
            .process(&param_batch_with(&["MAX_SPEED"], &[12.0]), "live")
            .unwrap();
        assert_eq!(output[0].topic(), "GROUP/MAX_SPEED");
    }

    #[test]
    fn live_merge_emits_only_on_base_with_previous_values_and_strings() {
        let mut op = merge_operation();
        assert!(
            op.process(&gps_batch(SourceId(1), &[150], &[100.0]), "live")
                .unwrap()
                .is_empty()
        );
        assert!(
            op.process(&status_batch(SourceId(1), &[175], &["AUTO"]), "live")
                .unwrap()
                .is_empty()
        );

        let out = op
            .process(&attitude_batch(&[100, 200], &[1.0, 2.0]), "live")
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].timestamps.values(), &[100, 200]);
        let altitude = f64_values(&out[0], "alt");
        assert!(altitude[0].is_nan());
        assert_eq!(altitude[1], 100.0);
        assert_eq!(utf8_values(&out[0], "mode"), ["", "AUTO"]);
    }

    #[test]
    fn live_merge_buffers_early_base_batches_until_typed_secondary_schema_arrives() {
        let mut op = merge_operation();
        for batch in [
            attitude_batch(&[300], &[3.0]),
            attitude_batch(&[100, 200], &[1.0, 2.0]),
            attitude_batch(&[250], &[2.5]),
        ] {
            assert!(op.process(&batch, "live").unwrap().is_empty());
            assert_eq!(op.consecutive_errors(), 0);
        }

        assert!(
            op.process(&gps_batch(SourceId(1), &[150], &[100.0]), "live")
                .unwrap()
                .is_empty()
        );
        let out = op
            .process(&status_batch(SourceId(1), &[175], &["AUTO"]), "live")
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].timestamps.values(), &[100, 200, 250, 300]);
        assert_eq!(f64_values(&out[0], "roll"), [1.0, 2.0, 2.5, 3.0]);
        let alt = f64_values(&out[0], "alt");
        assert!(alt[0].is_nan());
        assert_eq!(&alt[1..], &[100.0, 100.0, 100.0]);
        assert_eq!(utf8_values(&out[0], "mode"), ["", "AUTO", "AUTO", "AUTO"]);
        assert_eq!(
            out[0].schema.field_by_name("alt").unwrap().unit.as_deref(),
            Some("m")
        );
    }

    #[test]
    fn sustained_secondary_updates_append_linearly_and_prune_after_base() {
        let mut op = merge_operation();
        op.process(&status_batch(SourceId(1), &[], &[]), "live")
            .unwrap();
        for time in 0..2_000 {
            op.process(&gps_batch(SourceId(1), &[time], &[time as f64]), "live")
                .unwrap();
        }
        let state = &op.merge[&SourceId(1)];
        let ColumnHistory::F64 { times, values } =
            &state.histories[&("GPS".to_owned(), "alt".to_owned())]
        else {
            panic!("GPS altitude history was not numeric");
        };
        assert_eq!(times.len(), 2_000);
        assert_eq!(values.len(), 2_000);

        op.process(&attitude_batch(&[1_500], &[1.0]), "live")
            .unwrap();
        let ColumnHistory::F64 { times, values } =
            &op.merge[&SourceId(1)].histories[&("GPS".to_owned(), "alt".to_owned())]
        else {
            panic!("GPS altitude history was not numeric");
        };
        assert_eq!(times.first(), Some(&1_500));
        assert_eq!(times.len(), 500);
        assert_eq!(values.len(), 500);
    }

    #[test]
    fn live_merge_keeps_raw_sources_completely_isolated() {
        let mut op = merge_operation();
        op.process(&gps_batch(SourceId(1), &[150], &[100.0]), "live")
            .unwrap();
        op.process(&gps_batch(SourceId(2), &[150], &[200.0]), "live")
            .unwrap();
        op.process(&status_batch(SourceId(1), &[], &[]), "live")
            .unwrap();
        op.process(&status_batch(SourceId(2), &[], &[]), "live")
            .unwrap();

        let source_one = op.process(&attitude_batch(&[200], &[1.0]), "live").unwrap();
        let source_two_attitude = batch(
            SourceId(2),
            "ATTITUDE",
            vec![
                FieldSchema::new("roll", DataType::Float64, Some("rad"), 1.0).unwrap(),
                FieldSchema::new("frame", DataType::Utf8, None::<String>, 1.0).unwrap(),
            ],
            &[200],
            vec![
                Arc::new(Float64Array::from(vec![2.0])),
                Arc::new(StringArray::from(vec!["NED"])),
            ],
        );
        let source_two = op.process(&source_two_attitude, "live").unwrap();

        assert_eq!(f64_values(&source_one[0], "alt"), [100.0]);
        assert_eq!(f64_values(&source_two[0], "alt"), [200.0]);
    }

    #[test]
    fn pending_live_merge_bases_are_isolated_by_raw_source() {
        let mut op = merge_operation();
        let source_two_base = batch(
            SourceId(2),
            "ATTITUDE",
            vec![
                FieldSchema::new("roll", DataType::Float64, Some("rad"), 1.0).unwrap(),
                FieldSchema::new("frame", DataType::Utf8, None::<String>, 1.0).unwrap(),
            ],
            &[220],
            vec![
                Arc::new(Float64Array::from(vec![2.0])),
                Arc::new(StringArray::from(vec!["NED"])),
            ],
        );
        op.process(&attitude_batch(&[110], &[1.0]), "live").unwrap();
        op.process(&source_two_base, "live").unwrap();

        op.process(&gps_batch(SourceId(1), &[], &[]), "live")
            .unwrap();
        let source_one = op
            .process(&status_batch(SourceId(1), &[], &[]), "live")
            .unwrap();
        assert_eq!(source_one[0].timestamps.values(), &[110]);
        assert_eq!(op.merge[&SourceId(2)].pending_base.len(), 1);

        op.process(&gps_batch(SourceId(2), &[], &[]), "live")
            .unwrap();
        let source_two = op
            .process(&status_batch(SourceId(2), &[], &[]), "live")
            .unwrap();
        assert_eq!(source_two[0].timestamps.values(), &[220]);
    }

    #[test]
    fn live_merge_prunes_to_held_predecessor_and_accepts_late_sorted_samples() {
        let mut op = merge_operation();
        op.process(
            &gps_batch(SourceId(1), &[100, 200, 400], &[10.0, 20.0, 40.0]),
            "live",
        )
        .unwrap();
        op.process(&status_batch(SourceId(1), &[], &[]), "live")
            .unwrap();
        op.process(&attitude_batch(&[300], &[3.0]), "live").unwrap();

        let state = &op.merge[&SourceId(1)];
        let ColumnHistory::F64 { times, values } =
            &state.histories[&("GPS".to_owned(), "alt".to_owned())]
        else {
            panic!("GPS altitude history was not numeric");
        };
        assert_eq!(times, &[200, 400]);
        assert_eq!(values, &[20.0, 40.0]);

        op.process(&gps_batch(SourceId(1), &[250], &[25.0]), "live")
            .unwrap();
        let out = op.process(&attitude_batch(&[350], &[3.5]), "live").unwrap();
        assert_eq!(f64_values(&out[0], "alt"), [25.0]);
        let ColumnHistory::F64 { times, values } =
            &op.merge[&SourceId(1)].histories[&("GPS".to_owned(), "alt".to_owned())]
        else {
            panic!("GPS altitude history was not numeric");
        };
        assert_eq!(times, &[250, 400]);
        assert_eq!(values, &[25.0, 40.0]);
    }

    #[test]
    fn live_merge_starts_from_typed_snapshot_seeds_and_slices_watermarks() {
        let seed = MergeSeed {
            fields: HashMap::from([
                (
                    ("GPS".to_owned(), "alt".to_owned()),
                    SeedField::F64 {
                        unit: Some("m".to_owned()),
                        sample: Some((150, 100.0)),
                    },
                ),
                (
                    ("STATUS".to_owned(), "mode".to_owned()),
                    SeedField::Utf8 {
                        unit: Some("state".to_owned()),
                        sample: Some((175, "AUTO".to_owned())),
                    },
                ),
            ]),
        };
        let mut op = merge_operation_with(
            HashMap::from([(StreamKey::new(SourceId(1), "GPS"), 150)]),
            HashMap::from([(SourceId(1), seed)]),
        );
        let first = op.process(&attitude_batch(&[200], &[2.0]), "live").unwrap();
        assert_eq!(f64_values(&first[0], "alt"), [100.0]);
        assert_eq!(utf8_values(&first[0], "mode"), ["AUTO"]);
        assert_eq!(
            first[0]
                .schema
                .field_by_name("alt")
                .unwrap()
                .unit
                .as_deref(),
            Some("m")
        );
        assert_eq!(
            first[0].schema.field_by_name("mode").unwrap().dtype,
            DataType::Utf8
        );
        assert_eq!(
            first[0]
                .schema
                .field_by_name("mode")
                .unwrap()
                .unit
                .as_deref(),
            Some("state")
        );

        op.process(
            &gps_batch(SourceId(1), &[150, 250], &[999.0, 110.0]),
            "live",
        )
        .unwrap();
        let next = op.process(&attitude_batch(&[300], &[3.0]), "live").unwrap();
        assert_eq!(f64_values(&next[0], "alt"), [110.0]);
        assert_eq!(utf8_values(&next[0], "mode"), ["AUTO"]);
    }

    #[test]
    fn live_merge_schema_failure_does_not_commit_state_changes() {
        let mut op = merge_operation();
        op.process(&gps_batch(SourceId(1), &[], &[]), "live")
            .unwrap();
        op.process(&status_batch(SourceId(1), &[100], &["AUTO"]), "live")
            .unwrap();
        op.process(&attitude_batch(&[200], &[2.0]), "live").unwrap();

        op.process(
            &gps_batch(SourceId(2), &[100, 200, 400], &[10.0, 20.0, 40.0]),
            "live",
        )
        .unwrap();
        let numeric_status = batch(
            SourceId(2),
            "STATUS",
            vec![FieldSchema::new("mode", DataType::Float64, None::<String>, 1.0).unwrap()],
            &[],
            vec![Arc::new(Float64Array::from(Vec::<f64>::new()))],
        );
        op.process(&numeric_status, "live").unwrap();
        let source_two_base = batch(
            SourceId(2),
            "ATTITUDE",
            vec![
                FieldSchema::new("roll", DataType::Float64, Some("rad"), 1.0).unwrap(),
                FieldSchema::new("frame", DataType::Utf8, None::<String>, 1.0).unwrap(),
            ],
            &[300],
            vec![
                Arc::new(Float64Array::from(vec![3.0])),
                Arc::new(StringArray::from(vec!["NED"])),
            ],
        );
        let error = op.process(&source_two_base, "live").unwrap_err();
        assert!(error.contains("schema changed"), "{error}");

        let state = &op.merge[&SourceId(2)];
        assert_eq!(state.last_base_time, None);
        let ColumnHistory::F64 { times, values } =
            &state.histories[&("GPS".to_owned(), "alt".to_owned())]
        else {
            panic!("GPS altitude history was not numeric");
        };
        assert_eq!(times, &[100, 200, 400]);
        assert_eq!(values, &[10.0, 20.0, 40.0]);
    }

    #[test]
    fn live_merge_empty_seeded_utf8_field_emits_typed_gap() {
        let seed = MergeSeed {
            fields: HashMap::from([
                (
                    ("STATUS".to_owned(), "mode".to_owned()),
                    SeedField::Utf8 {
                        unit: Some("state".to_owned()),
                        sample: None,
                    },
                ),
                (
                    ("GPS".to_owned(), "alt".to_owned()),
                    SeedField::F64 {
                        unit: Some("m".to_owned()),
                        sample: None,
                    },
                ),
            ]),
        };
        let mut op = merge_operation_with(HashMap::new(), HashMap::from([(SourceId(1), seed)]));

        let out = op.process(&attitude_batch(&[200], &[2.0]), "live").unwrap();
        assert!(f64_values(&out[0], "alt")[0].is_nan());
        assert_eq!(utf8_values(&out[0], "mode"), [""]);
        assert_eq!(
            out[0].schema.field_by_name("mode").unwrap().dtype,
            DataType::Utf8
        );
        assert_eq!(
            out[0].schema.field_by_name("mode").unwrap().unit.as_deref(),
            Some("state")
        );
    }

    #[test]
    fn live_merge_configured_topic_matches_instance_and_equal_time_latest_wins() {
        let mut op = merge_operation();
        let gps_instance = |value| {
            batch(
                SourceId(1),
                "GPS[1]",
                vec![FieldSchema::new("alt", DataType::Float64, Some("m"), 1.0).unwrap()],
                &[150],
                vec![Arc::new(Float64Array::from(vec![value]))],
            )
        };
        op.process(&gps_instance(100.0), "live").unwrap();
        op.process(&gps_instance(200.0), "live").unwrap();
        op.process(&status_batch(SourceId(1), &[], &[]), "live")
            .unwrap();

        let out = op.process(&attitude_batch(&[200], &[2.0]), "live").unwrap();
        assert_eq!(f64_values(&out[0], "alt"), [200.0]);
        assert!(
            op.merge[&SourceId(1)]
                .histories
                .contains_key(&("GPS".to_owned(), "alt".to_owned()))
        );
        assert!(
            !op.merge[&SourceId(1)]
                .histories
                .contains_key(&("GPS[1]".to_owned(), "alt".to_owned()))
        );
    }

    #[test]
    fn live_matching_rejects_derived_sources_before_selector_matching() {
        let mut op = transform_operation_with_watermark(i64::MIN);
        assert!(
            op.process(&attitude_batch(&[300], &[2.0]), "script:other")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn only_matching_processing_changes_the_error_streak() {
        let mut op = transform_operation_with_watermark(i64::MIN);
        let malformed = batch(
            SourceId(1),
            "ATTITUDE",
            vec![FieldSchema::new("frame", DataType::Utf8, None::<String>, 1.0).unwrap()],
            &[300],
            vec![Arc::new(StringArray::from(vec!["NED"]))],
        );
        assert!(op.process(&malformed, "live").is_err());
        assert_eq!(op.consecutive_errors, 1);

        assert!(
            op.process(&gps_batch(SourceId(1), &[310], &[100.0]), "live")
                .unwrap()
                .is_empty()
        );
        assert_eq!(op.consecutive_errors, 1);
        assert!(
            op.process(&attitude_batch(&[320], &[3.0]), "script:other")
                .unwrap()
                .is_empty()
        );
        assert_eq!(op.consecutive_errors, 1);

        op.process(&attitude_batch(&[330], &[4.0]), "live").unwrap();
        assert_eq!(op.consecutive_errors, 0);
    }

    #[test]
    fn first_live_schema_is_validated_and_output_schema_is_pinned() {
        let mut op = transform_operation_with_watermark(i64::MIN);
        let first = attitude_batch(&[300], &[2.0]);
        op.process(&first, "live").unwrap();

        let changed = batch(
            SourceId(1),
            "ATTITUDE",
            vec![
                FieldSchema::new("frame", DataType::Utf8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("roll", DataType::Float64, Some("rad"), 1.0).unwrap(),
            ],
            &[400],
            vec![
                Arc::new(StringArray::from(vec!["NED"])),
                Arc::new(Float64Array::from(vec![3.0])),
            ],
        );
        let error = op.process(&changed, "live").unwrap_err();
        assert!(error.contains("schema changed"), "{error}");

        let missing = batch(
            SourceId(2),
            "ATTITUDE",
            vec![FieldSchema::new("frame", DataType::Utf8, None::<String>, 1.0).unwrap()],
            &[500],
            vec![Arc::new(StringArray::from(vec!["NED"]))],
        );
        let mut fresh = transform_operation_with_watermark(i64::MIN);
        let error = fresh.process(&missing, "live").unwrap_err();
        assert!(error.contains("field 'roll' not found"), "{error}");
    }

    #[test]
    fn failed_batch_preparation_does_not_leave_a_schema_pin() {
        let mut op = transform_operation_with_watermark(i64::MIN);
        let invalid = PendingTopic {
            name: "OUT".into(),
            times: vec![1],
            fields: vec![PendingField {
                name: String::new(),
                values: PendingColumn::F64(vec![1.0]),
                unit: None,
            }],
        };

        assert!(op.pin_and_build(vec![invalid]).is_err());
        assert!(op.emitted_schemas.is_empty());

        let valid = PendingTopic {
            name: "OUT".into(),
            times: vec![2],
            fields: vec![PendingField::numeric("value", vec![2.0], None)],
        };
        let out = op.pin_and_build(vec![valid]).unwrap();
        assert_eq!(f64_values(&out[0], "value"), [2.0]);
    }
}
