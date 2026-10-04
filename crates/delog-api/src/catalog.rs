//! Snapshot-backed native catalog access.

use delog_core::field_view::{FieldView, array_row_as_f64, array_row_as_str};
use delog_core::identity::{FieldId, SourceId, TopicId, parse_topic_instance};
use delog_core::snapshot::StoreSnapshot;

use crate::timestamps::TimestampMode;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicMatch {
    pub source_id: SourceId,
    pub source_label: String,
    pub topic_id: TopicId,
    pub topic_name: String,
    pub base_name: String,
    pub instance: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMatch {
    pub source_id: SourceId,
    pub source_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldMatch {
    pub source_id: SourceId,
    pub source_label: String,
    pub topic_id: TopicId,
    pub topic_name: String,
    pub base_name: String,
    pub instance: Option<u32>,
    pub field_id: FieldId,
    pub field_name: String,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaterializedField {
    pub times_us: Vec<i64>,
    pub values: Vec<f64>,
    pub strings: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaterializedColumn {
    pub name: String,
    pub values: Vec<f64>,
    pub strings: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaterializedTable {
    pub times_us: Vec<i64>,
    pub columns: Vec<MaterializedColumn>,
}

pub fn topic_matches(
    topic_name: &str,
    base_name: &str,
    parsed_instance: Option<u32>,
    requested_topic: Option<&str>,
    requested_instance: Option<u32>,
) -> bool {
    if let Some(topic) = requested_topic
        && topic_name != topic
        && base_name != topic
    {
        return false;
    }
    if let Some(instance) = requested_instance
        && parsed_instance != Some(instance)
    {
        return false;
    }
    true
}

pub fn find_topics(
    snapshot: &StoreSnapshot,
    topic: Option<&str>,
    source: Option<&str>,
    instance: Option<u32>,
) -> Vec<TopicMatch> {
    let mut matches = Vec::new();
    for source_snapshot in snapshot.sources.iter() {
        if source_snapshot.entry.removed {
            continue;
        }
        if let Some(source) = source
            && source_snapshot.entry.label != source
        {
            continue;
        }
        for &topic_id in source_snapshot.topics.iter() {
            let Some(topic_snapshot) = snapshot.topic(topic_id) else {
                continue;
            };
            if topic_snapshot.entry.removed {
                continue;
            }
            let (base_name, parsed_instance) = parse_topic_instance(&topic_snapshot.entry.name);
            if !topic_matches(
                &topic_snapshot.entry.name,
                &base_name,
                parsed_instance,
                topic,
                instance,
            ) {
                continue;
            }
            matches.push(TopicMatch {
                source_id: source_snapshot.entry.id,
                source_label: source_snapshot.entry.label.clone(),
                topic_id,
                topic_name: topic_snapshot.entry.name.clone(),
                base_name,
                instance: parsed_instance,
            });
        }
    }
    matches
}

fn field_unit(snapshot: &StoreSnapshot, topic: TopicId, field_name: &str) -> Option<String> {
    snapshot
        .topic_store(topic)?
        .schema
        .field_by_name(field_name)?
        .unit
        .clone()
}

pub fn find_fields(
    snapshot: &StoreSnapshot,
    topic: Option<&str>,
    field: Option<&str>,
    source: Option<&str>,
    instance: Option<u32>,
) -> Vec<FieldMatch> {
    let mut matches = Vec::new();
    for topic_match in find_topics(snapshot, topic, source, instance) {
        for field_entry in snapshot.fields.iter() {
            if field_entry.removed || field_entry.topic != topic_match.topic_id {
                continue;
            }
            if let Some(field) = field
                && field_entry.name != field
            {
                continue;
            }
            matches.push(FieldMatch {
                source_id: topic_match.source_id,
                source_label: topic_match.source_label.clone(),
                topic_id: topic_match.topic_id,
                topic_name: topic_match.topic_name.clone(),
                base_name: topic_match.base_name.clone(),
                instance: topic_match.instance,
                field_id: field_entry.id,
                field_name: field_entry.name.clone(),
                unit: field_unit(snapshot, topic_match.topic_id, &field_entry.name),
            });
        }
    }
    matches
}

pub fn find_fields_in_topic(
    snapshot: &StoreSnapshot,
    topic_id: TopicId,
    field: Option<&str>,
) -> Vec<FieldMatch> {
    let Some(topic_snapshot) = snapshot.topic(topic_id) else {
        return Vec::new();
    };
    if topic_snapshot.entry.removed {
        return Vec::new();
    }
    let Some(source) = snapshot
        .sources
        .iter()
        .find(|source| !source.entry.removed && source.topics.contains(&topic_id))
    else {
        return Vec::new();
    };
    let (base_name, instance) = parse_topic_instance(&topic_snapshot.entry.name);
    let mut matches = Vec::new();
    for field_entry in snapshot.fields.iter() {
        if field_entry.removed || field_entry.topic != topic_id {
            continue;
        }
        if let Some(field) = field
            && field_entry.name != field
        {
            continue;
        }
        matches.push(FieldMatch {
            source_id: source.entry.id,
            source_label: source.entry.label.clone(),
            topic_id,
            topic_name: topic_snapshot.entry.name.clone(),
            base_name: base_name.clone(),
            instance,
            field_id: field_entry.id,
            field_name: field_entry.name.clone(),
            unit: field_unit(snapshot, topic_id, &field_entry.name),
        });
    }
    matches
}

pub fn candidate_topic_paths(matches: &[TopicMatch]) -> String {
    matches
        .iter()
        .map(|topic| format!("{}/{}", topic.source_label, topic.topic_name))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn candidate_field_paths(matches: &[FieldMatch]) -> String {
    matches
        .iter()
        .map(|field| {
            format!(
                "{}/{}/{}",
                field.source_label, field.topic_name, field.field_name
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn resolve_topic(
    snapshot: &StoreSnapshot,
    name: &str,
    source: Option<&str>,
    instance: Option<u32>,
) -> Result<TopicMatch> {
    let matches = find_topics(snapshot, Some(name), source, instance);
    match matches.as_slice() {
        [topic] => Ok(topic.clone()),
        [] => {
            let candidates = find_topics(snapshot, None, source, instance);
            if candidates.is_empty() {
                Err(Error::not_found(format!("topic '{name}' not found")))
            } else {
                Err(Error::not_found(format!(
                    "topic '{name}' not found; candidates: {}",
                    candidate_topic_paths(&candidates)
                )))
            }
        }
        _ => Err(Error::ambiguous(format!(
            "topic '{name}' is ambiguous; candidates: {}; pass source= or instance=",
            candidate_topic_paths(&matches)
        ))),
    }
}

pub fn resolve_source(snapshot: &StoreSnapshot, requested: &str) -> Result<SourceMatch> {
    let matches: Vec<_> = snapshot
        .sources
        .iter()
        .filter(|source| !source.entry.removed && source.entry.label == requested)
        .collect();
    match matches.as_slice() {
        [source] => Ok(SourceMatch {
            source_id: source.entry.id,
            source_label: source.entry.label.clone(),
        }),
        [] => Err(Error::not_found(format!("source '{requested}' not found"))),
        _ => Err(Error::ambiguous(format!(
            "source '{requested}' is ambiguous; candidate IDs: {}",
            matches
                .iter()
                .map(|source| source.entry.id.0.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

pub fn resolve_field_path(snapshot: &StoreSnapshot, path: &str) -> Result<FieldMatch> {
    let Some((topic, field)) = path.split_once('.') else {
        return Err(Error::invalid_input(format!(
            "field '{path}' must be 'topic.field'"
        )));
    };
    let matches = find_fields(snapshot, Some(topic), Some(field), None, None);
    match matches.as_slice() {
        [field] => Ok(field.clone()),
        [] => Err(Error::not_found(format!("field '{path}' not found"))),
        _ => Err(Error::ambiguous(format!(
            "field '{path}' is ambiguous; candidates: {}",
            candidate_field_paths(&matches)
        ))),
    }
}

pub fn resolve_field(
    snapshot: &StoreSnapshot,
    topic: &str,
    field: &str,
    source: Option<&str>,
    instance: Option<u32>,
) -> Result<FieldMatch> {
    let matches = find_fields(snapshot, Some(topic), Some(field), source, instance);
    match matches.as_slice() {
        [field] => Ok(field.clone()),
        [] => {
            let candidates = find_fields(snapshot, Some(topic), None, source, instance);
            if candidates.is_empty() {
                Err(Error::not_found(format!(
                    "field '{field}' not found in topic '{topic}'"
                )))
            } else {
                Err(Error::not_found(format!(
                    "field '{field}' not found in topic '{topic}'; candidates: {}",
                    candidate_field_paths(&candidates)
                )))
            }
        }
        _ => Err(Error::ambiguous(format!(
            "field '{field}' in topic '{topic}' is ambiguous; candidates: {}; pass source= or instance=",
            candidate_field_paths(&matches)
        ))),
    }
}

pub fn materialize_field(
    snapshot: &StoreSnapshot,
    field: FieldId,
    timestamp_mode: TimestampMode,
) -> Result<MaterializedField> {
    let view =
        FieldView::new(snapshot, field).map_err(|error| Error::invalid_input(error.to_string()))?;
    let store = snapshot
        .topic_store(view.topic())
        .ok_or_else(|| Error::invalid_input(format!("topic {:?} has no data", view.topic())))?;
    if store.chunks.is_empty() && snapshot.global_time_range().is_none() {
        return Err(Error::invalid_input("field has no data"));
    }
    let column = view.col_index();
    let offset_us = view.offset_us_for_export();
    let mut times_us = Vec::new();
    let mut values = Vec::new();
    let mut strings = view.schema_field().is_string().then(Vec::new);
    for chunk in store.chunks.iter() {
        for row in 0..chunk.len() {
            let raw_time = chunk.t.value(row);
            let time = match timestamp_mode {
                TimestampMode::Effective => raw_time.checked_add(offset_us).ok_or_else(|| {
                    Error::invalid_input("source offset overflows a script timestamp")
                })?,
                TimestampMode::Original => raw_time,
            };
            times_us.push(time);
            values.push(array_row_as_f64(chunk.cols[column].as_ref(), row));
            if let Some(strings) = &mut strings {
                strings.push(
                    array_row_as_str(chunk.cols[column].as_ref(), row)
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
        }
    }
    Ok(MaterializedField {
        times_us,
        values,
        strings,
    })
}

pub fn resolve_field_in_topic(
    snapshot: &StoreSnapshot,
    topic_id: TopicId,
    name: &str,
) -> Result<FieldMatch> {
    let topic_name = snapshot
        .topic(topic_id)
        .map(|topic| topic.entry.name.as_str())
        .unwrap_or_default();
    let matches = find_fields_in_topic(snapshot, topic_id, Some(name));
    match matches.as_slice() {
        [field] => Ok(field.clone()),
        [] => {
            let candidates = find_fields_in_topic(snapshot, topic_id, None);
            if candidates.is_empty() {
                Err(Error::not_found(format!(
                    "field '{name}' not found in topic '{topic_name}'"
                )))
            } else {
                Err(Error::not_found(format!(
                    "field '{name}' not found in topic '{topic_name}'; candidates: {}",
                    candidate_field_paths(&candidates)
                )))
            }
        }
        _ => Err(Error::ambiguous(format!(
            "field '{name}' in topic '{topic_name}' is ambiguous"
        ))),
    }
}

pub fn materialize_topic(
    snapshot: &StoreSnapshot,
    topic: TopicId,
    fields: &[String],
    timestamp_mode: TimestampMode,
) -> Result<MaterializedTable> {
    let requested = if fields.is_empty() {
        find_fields_in_topic(snapshot, topic, None)
            .into_iter()
            .map(|field| field.field_name)
            .collect()
    } else {
        fields.to_vec()
    };
    let topic_name = snapshot
        .topic(topic)
        .map(|topic| topic.entry.name.as_str())
        .unwrap_or_default();
    let mut times_us: Option<Vec<i64>> = None;
    let mut columns = Vec::with_capacity(requested.len());
    for name in requested {
        let field = resolve_field_in_topic(snapshot, topic, &name)?;
        let materialized = materialize_field(snapshot, field.field_id, timestamp_mode)?;
        match &times_us {
            None => times_us = Some(materialized.times_us),
            Some(existing) if *existing == materialized.times_us => {}
            Some(_) => {
                return Err(Error::invalid_input(format!(
                    "topic '{topic_name}' field '{name}' does not share the topic timeline"
                )));
            }
        }
        columns.push(MaterializedColumn {
            name,
            values: materialized.values,
            strings: materialized.strings,
        });
    }
    Ok(MaterializedTable {
        times_us: times_us.unwrap_or_default(),
        columns,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;

    use crate::timestamps::TimestampMode;

    use super::*;

    #[test]
    fn snapshot_lookup_finds_topics_and_fields() {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("flight");
        let imu = id.add_topic_instance(src, "IMU", 0).unwrap();
        let gps = id.add_topic(src, "GPS").unwrap();
        let accx = id.add_field(imu, "AccX").unwrap();
        let accy = id.add_field(imu, "AccY").unwrap();
        let alt = id.add_field(gps, "Alt").unwrap();

        let imu_schema = Arc::new(
            TopicSchema::new(
                "IMU[0]",
                [
                    FieldSchema::new("AccX", DataType::Float64, Some("m/s^2"), 1.0).unwrap(),
                    FieldSchema::new("AccY", DataType::Float64, Some("m/s^2"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let gps_schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let imu_chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10]),
                vec![
                    Arc::new(Float64Array::from(vec![1.0])) as ArrayRef,
                    Arc::new(Float64Array::from(vec![2.0])) as ArrayRef,
                ],
                &imu_schema,
            )
            .unwrap(),
        );
        let gps_chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10]),
                vec![Arc::new(Float64Array::from(vec![100.0])) as ArrayRef],
                &gps_schema,
            )
            .unwrap(),
        );
        let imu_store = Arc::new(TopicStore::from_chunks(imu_schema, [imu_chunk]).unwrap());
        let gps_store = Arc::new(TopicStore::from_chunks(gps_schema, [gps_chunk]).unwrap());
        let snapshot =
            StoreSnapshot::from_registry(&id, [(imu, imu_store), (gps, gps_store)], 0).unwrap();

        let topics = find_topics(&snapshot, Some("IMU"), None, Some(0));
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].topic_id, imu);
        assert_eq!(topics[0].source_label, "flight");
        assert_eq!(topics[0].topic_name, "IMU[0]");
        assert_eq!(topics[0].base_name, "IMU");
        assert_eq!(topics[0].instance, Some(0));

        let fields = find_fields(&snapshot, Some("IMU"), Some("AccX"), None, Some(0));
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_id, accx);
        assert_eq!(fields[0].field_name, "AccX");
        assert_eq!(fields[0].unit.as_deref(), Some("m/s^2"));

        let all_fields = find_fields(&snapshot, Some("IMU"), None, None, Some(0));
        let ids: Vec<_> = all_fields.iter().map(|field| field.field_id).collect();
        assert_eq!(ids, vec![accx, accy]);

        let gps_fields = find_fields(&snapshot, Some("GPS"), Some("Alt"), Some("flight"), None);
        assert_eq!(gps_fields[0].field_id, alt);
    }

    #[test]
    fn topic_ref_field_lookup_keeps_exact_topic_identity() {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("flight");
        let gps = id.add_topic(src, "GPS").unwrap();
        let gps0 = id.add_topic_instance(src, "GPS", 0).unwrap();
        let lat = id.add_field(gps, "Lat").unwrap();
        let fix = id.add_field(gps0, "Fix").unwrap();
        let snapshot = StoreSnapshot::from_registry(&id, [], 0).unwrap();

        let fields = find_fields_in_topic(&snapshot, gps, None);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_id, lat);
        assert_ne!(fields[0].field_id, fix);
        assert!(find_fields_in_topic(&snapshot, gps, Some("Fix")).is_empty());
    }

    #[test]
    fn materialize_field_concatenates_chunks_in_time_order() {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("flight");
        let topic = id.add_topic(src, "BARO").unwrap();
        let alt = id.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let first: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![1.0, 2.0]))];
        let second: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![3.0]))];
        let first =
            Arc::new(Chunk::try_new(Int64Array::from(vec![10, 20]), first, &schema).unwrap());
        let second = Arc::new(Chunk::try_new(Int64Array::from(vec![30]), second, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(schema, [first, second]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();

        let field = materialize_field(&snapshot, alt, TimestampMode::Effective).unwrap();
        assert_eq!(field.times_us, vec![10, 20, 30]);
        assert_eq!(field.values, vec![1.0, 2.0, 3.0]);
        assert_eq!(field.strings, None);
    }

    #[test]
    fn materialize_empty_topic_when_another_topic_has_data() {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("flight");
        let populated = id.add_topic(src, "BARO").unwrap();
        id.add_field(populated, "Alt").unwrap();
        let empty = id.add_topic(src, "STATUS").unwrap();
        let mode = id.add_field(empty, "Mode").unwrap();

        let populated_schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let empty_schema = Arc::new(
            TopicSchema::new(
                "STATUS",
                [FieldSchema::new("Mode", DataType::Utf8, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10]),
                vec![Arc::new(Float64Array::from(vec![100.0])) as ArrayRef],
                &populated_schema,
            )
            .unwrap(),
        );
        let populated_store = Arc::new(TopicStore::from_chunks(populated_schema, [chunk]).unwrap());
        let empty_store = Arc::new(TopicStore::new(empty_schema));
        let snapshot = StoreSnapshot::from_registry(
            &id,
            [(populated, populated_store), (empty, empty_store)],
            0,
        )
        .unwrap();

        let field = materialize_field(&snapshot, mode, TimestampMode::Effective).unwrap();
        assert!(field.times_us.is_empty());
        assert!(field.values.is_empty());
        assert_eq!(field.strings, Some(Vec::new()));

        let topic = materialize_topic(&snapshot, empty, &[], TimestampMode::Effective).unwrap();
        assert!(topic.times_us.is_empty());
        assert_eq!(topic.columns.len(), 1);
        assert_eq!(topic.columns[0].name, "Mode");
        assert!(topic.columns[0].values.is_empty());
        assert_eq!(topic.columns[0].strings, Some(Vec::new()));
    }

    #[test]
    fn materialize_field_uses_effective_times_by_default_and_can_use_original_times() {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("flight");
        id.set_source_offset_us(src, -1_784_120_623_158_000)
            .unwrap();
        let topic = id.add_topic(src, "BARO").unwrap();
        let alt = id.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let columns: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![1.0]))];
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![1_784_120_700_000_000]),
                columns,
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();

        assert_eq!(
            materialize_field(&snapshot, alt, TimestampMode::Effective)
                .unwrap()
                .times_us,
            vec![76_842_000]
        );
        assert_eq!(
            materialize_field(&snapshot, alt, TimestampMode::Original)
                .unwrap()
                .times_us,
            vec![1_784_120_700_000_000]
        );

        let mut overflow_id = IdentityRegistry::new();
        let overflow_src = overflow_id.add_source("overflow");
        overflow_id
            .set_source_offset_us(overflow_src, i64::MAX)
            .unwrap();
        let overflow_topic = overflow_id.add_topic(overflow_src, "BARO").unwrap();
        let overflow_alt = overflow_id.add_field(overflow_topic, "Alt").unwrap();
        let overflow_chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![1]),
                vec![Arc::new(Float64Array::from(vec![1.0])) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let overflow_store = Arc::new(TopicStore::from_chunks(schema, [overflow_chunk]).unwrap());
        let overflow =
            StoreSnapshot::from_registry(&overflow_id, [(overflow_topic, overflow_store)], 0)
                .unwrap();
        assert_eq!(
            materialize_field(&overflow, overflow_alt, TimestampMode::Effective)
                .unwrap_err()
                .to_string(),
            "source offset overflows a script timestamp"
        );
    }

    #[test]
    fn materialize_field_extracts_strings_for_utf8_columns() {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("live");
        let topic = id.add_topic(src, "NAMED_VALUE_FLOAT").unwrap();
        let name = id.add_field(topic, "name").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "NAMED_VALUE_FLOAT",
                [FieldSchema::new("name", DataType::Utf8, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let columns: Vec<ArrayRef> = vec![Arc::new(StringArray::from(vec![Some("airspd"), None]))];
        let chunk =
            Arc::new(Chunk::try_new(Int64Array::from(vec![10, 20]), columns, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();

        let field = materialize_field(&snapshot, name, TimestampMode::Effective).unwrap();
        assert_eq!(field.times_us, vec![10, 20]);
        assert!(field.values.iter().all(|value| value.is_nan()));
        assert_eq!(
            field.strings,
            Some(vec!["airspd".to_owned(), String::new()])
        );
    }

    #[test]
    fn field_path_ambiguity_lists_candidates_in_snapshot_order() {
        let mut id = IdentityRegistry::new();
        for source_label in ["alpha", "beta"] {
            let source = id.add_source(source_label);
            let topic = id.add_topic(source, "GPS").unwrap();
            id.add_field(topic, "Lat").unwrap();
        }
        let snapshot = StoreSnapshot::from_registry(&id, [], 0).unwrap();

        assert_eq!(
            resolve_field_path(&snapshot, "GPS.Lat")
                .unwrap_err()
                .to_string(),
            "field 'GPS.Lat' is ambiguous; candidates: alpha/GPS/Lat, beta/GPS/Lat"
        );
    }
}
