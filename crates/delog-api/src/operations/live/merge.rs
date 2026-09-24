use std::collections::HashMap;

use crate::operations::MergeSpec;
use crate::operations::snapshot::{MergeSeed, SeedField, pending_topic};
use crate::{Error, Result};
use delog_core::derived::{PendingColumn, PendingTopic};
use delog_core::ingest::ParsedBatch;

use super::{configured_topic_matches, selected_columns};

#[derive(Debug, Clone)]
pub(super) enum ColumnHistory {
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
    fn insert(&mut self, times: &[i64], values: &PendingColumn) -> Result<()> {
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
                return Err(Error::execution("merge field type changed between batches"));
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

    pub(super) fn prune(&mut self, cutoff: i64) {
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
pub(super) struct MergeState {
    pub(super) histories: HashMap<(String, String), ColumnHistory>,
    units: HashMap<(String, String), Option<String>>,
    pub(super) last_base_time: Option<i64>,
    pub(super) pending_base: Vec<PendingBaseBatch>,
}

#[derive(Debug)]
pub(super) struct PendingBaseBatch {
    times: Vec<i64>,
    fields: Vec<(String, PendingColumn, Option<String>)>,
}

pub(super) struct MergeOutput {
    pub(super) topics: Vec<PendingTopic>,
    pub(super) consumed_pending: bool,
    pub(super) cutoff: Option<i64>,
}

impl MergeState {
    pub(super) fn from_seed(seed: MergeSeed) -> Self {
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

pub(super) fn execute_merge(
    batch: &ParsedBatch,
    source_label: &str,
    spec: &MergeSpec,
    rows: Vec<usize>,
    state: &mut MergeState,
) -> Result<MergeOutput> {
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
        return Err(Error::invalid_input(
            "merge output field names do not match selected fields",
        ));
    }
    let base_index = spec
        .topics
        .iter()
        .position(|(topic, _)| topic == &spec.base_topic)
        .ok_or_else(|| {
            Error::invalid_input(format!(
                "merge base_topic '{}' must be present in topics",
                spec.base_topic
            ))
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
                return Err(Error::execution(format!(
                    "merge field '{}/{}' unit changed from {:?} to {:?}",
                    key.0, key.1, existing_unit, unit
                )));
            }
            if let Some(history) = state.histories.get(&key)
                && !matches!(
                    (history, values),
                    (ColumnHistory::F64 { .. }, PendingColumn::F64(_))
                        | (ColumnHistory::Utf8 { .. }, PendingColumn::Utf8(_))
                )
            {
                return Err(Error::execution(format!(
                    "merge field '{}/{}' type changed between batches",
                    key.0, key.1
                )));
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
) -> Result<()> {
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
        return Err(Error::execution(
            "merge base schema changed while waiting for secondary metadata",
        ));
    }
    Ok(())
}

fn build_merged_base_topic(
    spec: &MergeSpec,
    base_index: usize,
    bases: &[PendingBaseBatch],
    state: &MergeState,
) -> Result<PendingTopic> {
    build_merged_base_topic_refs(spec, base_index, &bases.iter().collect::<Vec<_>>(), state)
}

fn build_merged_base_topic_refs(
    spec: &MergeSpec,
    base_index: usize,
    bases: &[&PendingBaseBatch],
    state: &MergeState,
) -> Result<PendingTopic> {
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
