use delog_core::identity::parse_topic_instance;
use delog_core::snapshot::StoreSnapshot;
use delog_flow::graph::FieldSelector;

use crate::ui::fuzzy::fuzzy_match_score;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataHit {
    pub selector: FieldSelector,
    pub label: String,
    pub unit: Option<String>,
    pub rows: u64,
    pub score: u32,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FieldOptions {
    pub sources: Vec<String>,
    pub topics: Vec<String>,
    pub fields: Vec<String>,
}

pub fn topic_display(selector: &FieldSelector) -> String {
    match selector.instance {
        Some(instance) => format!("{}[{instance}]", selector.topic),
        None => selector.topic.clone(),
    }
}

pub fn field_options(snapshot: &StoreSnapshot, selector: &FieldSelector) -> FieldOptions {
    let selected_topic = topic_display(selector);
    let mut options = FieldOptions::default();
    for source in snapshot.sources.iter() {
        if source.entry.removed {
            continue;
        }
        let selected_source = selector
            .source
            .as_deref()
            .is_none_or(|label| label == source.entry.label);
        let mut has_numeric = false;
        for &topic_id in source.topics.iter() {
            let Some(topic) = snapshot.topic(topic_id) else {
                continue;
            };
            if topic.entry.removed {
                continue;
            }
            let Some(store) = topic.store.as_ref() else {
                continue;
            };
            let numeric = snapshot
                .fields
                .iter()
                .filter(|field| !field.removed && field.topic == topic_id)
                .filter(|field| {
                    store
                        .schema
                        .field_by_name(&field.name)
                        .is_some_and(delog_core::schema::FieldSchema::is_numeric)
                });
            let mut topic_has_numeric = false;
            for field in numeric {
                topic_has_numeric = true;
                if selected_source && topic.entry.name == selected_topic {
                    push_unique(&mut options.fields, &field.name);
                }
            }
            if !topic_has_numeric {
                continue;
            }
            has_numeric = true;
            if selected_source {
                push_unique(&mut options.topics, &topic.entry.name);
            }
        }
        if has_numeric {
            push_unique(&mut options.sources, &source.entry.label);
        }
    }
    options.topics.sort();
    options.fields.sort();
    options
}

pub fn retarget_topic(snapshot: &StoreSnapshot, selector: &mut FieldSelector, topic: &str) {
    let (base, instance) = parse_topic_instance(topic);
    selector.topic = base;
    selector.instance = instance;
    let moved = field_options(snapshot, selector);
    if !moved.fields.contains(&selector.field)
        && let Some(first) = moved.fields.first()
    {
        selector.field.clone_from(first);
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_owned());
    }
}

pub fn search_fields(snapshot: &StoreSnapshot, query: &str, limit: usize) -> Vec<DataHit> {
    let empty_query = query.trim().is_empty();
    let mut hits = Vec::new();
    for source in snapshot.sources.iter() {
        if source.entry.removed {
            continue;
        }
        for &topic_id in source.topics.iter() {
            let Some(topic) = snapshot.topic(topic_id) else {
                continue;
            };
            if topic.entry.removed {
                continue;
            }
            let Some(store) = topic.store.as_ref() else {
                continue;
            };
            let (base, instance) = parse_topic_instance(&topic.entry.name);
            for field in snapshot
                .fields
                .iter()
                .filter(|field| !field.removed && field.topic == topic_id)
            {
                let Some(schema) = store.schema.field_by_name(&field.name) else {
                    continue;
                };
                if !schema.is_numeric() {
                    continue;
                }
                let candidate = format!(
                    "{}/{}.{} {}",
                    source.entry.label,
                    topic.entry.name,
                    field.name,
                    schema.unit.as_deref().unwrap_or_default()
                );
                let score = if empty_query {
                    0
                } else if let Some(score) = fuzzy_match_score(query, &candidate) {
                    score
                } else {
                    continue;
                };
                hits.push(DataHit {
                    selector: FieldSelector {
                        source: Some(source.entry.label.clone()),
                        topic: base.clone(),
                        instance,
                        field: field.name.clone(),
                    },
                    label: format!(
                        "{} › {} › {}",
                        source.entry.label, topic.entry.name, field.name
                    ),
                    unit: schema.unit.clone(),
                    rows: store.rows,
                    score,
                });
            }
        }
    }
    if !empty_query {
        hits.sort_by(|left, right| {
            left.score
                .cmp(&right.score)
                .then_with(|| left.label.cmp(&right.label))
        });
    }
    hits.truncate(limit);
    hits
}

#[cfg(test)]
pub(crate) use tests::snapshot_two_sources;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::{IdentityRegistry, SourceId, TopicId};
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;

    use super::*;

    fn imu_topic(
        identity: &mut IdentityRegistry,
        source: SourceId,
        name: &str,
    ) -> (TopicId, Arc<TopicStore>) {
        let topic = identity.add_topic(source, name).unwrap();
        identity.add_field(topic, "AccX").unwrap();
        identity.add_field(topic, "name").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                name,
                [
                    FieldSchema::new("AccX", DataType::Float64, Some("m/s^2"), 1.0).unwrap(),
                    FieldSchema::new("name", DataType::Utf8, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![100, 200, 300]),
                vec![
                    Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0])) as ArrayRef,
                    Arc::new(StringArray::from(vec!["a", "b", "c"])) as ArrayRef,
                ],
                &schema,
            )
            .unwrap(),
        );
        (
            topic,
            Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
        )
    }

    fn gps_topic(identity: &mut IdentityRegistry, source: SourceId) -> (TopicId, Arc<TopicStore>) {
        let topic = identity.add_topic(source, "GPS").unwrap();
        identity.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![100, 200, 300]),
                vec![Arc::new(Float64Array::from(vec![10.0, 11.0, 12.0])) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        (
            topic,
            Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
        )
    }

    pub(crate) fn snapshot_two_sources() -> StoreSnapshot {
        let mut identity = IdentityRegistry::new();
        let flight_01 = identity.add_source("flight_01");
        let flight_02 = identity.add_source("flight_02");
        let stores = [
            imu_topic(&mut identity, flight_01, "IMU[0]"),
            imu_topic(&mut identity, flight_01, "IMU[1]"),
            gps_topic(&mut identity, flight_01),
            imu_topic(&mut identity, flight_02, "IMU[0]"),
            gps_topic(&mut identity, flight_02),
        ];
        StoreSnapshot::from_registry(&identity, stores, 5).unwrap()
    }

    #[test]
    fn a_dotted_query_scopes_the_search_to_a_topics_fields() {
        let snapshot = snapshot_two_sources();

        let hits = search_fields(&snapshot, "gps.alt", 10);

        assert!(
            !hits.is_empty(),
            "topic.field should match like the browser"
        );
        assert!(
            hits.iter()
                .all(|hit| hit.selector.topic == "GPS" && hit.selector.field == "Alt"),
            "the part after the dot must scope to fields, got {hits:?}"
        );
    }

    #[test]
    fn searches_across_source_topic_field_and_unit() {
        let snapshot = snapshot_two_sources();
        assert!(
            search_fields(&snapshot, "accx", 10)
                .iter()
                .any(|hit| hit.label.contains("AccX"))
        );
        assert!(
            search_fields(&snapshot, "flight_02 alt", 10)
                .iter()
                .all(|hit| hit.label.starts_with("flight_02"))
        );
        assert!(
            search_fields(&snapshot, "m/s", 10)
                .iter()
                .any(|hit| hit.label.contains("AccX"))
        );
    }

    #[test]
    fn string_fields_are_excluded_and_selector_round_trips() {
        let snapshot = snapshot_two_sources();
        let hits = search_fields(&snapshot, "", 100);
        assert!(hits.iter().all(|hit| !hit.label.contains("name")));
        let hit = &search_fields(&snapshot, "flight_01 accx", 1)[0];
        assert!(delog_flow::resolve::resolve_field(&snapshot, &hit.selector).is_ok());
    }

    fn imu_selector() -> FieldSelector {
        FieldSelector {
            source: Some("flight_01".to_owned()),
            topic: "IMU".to_owned(),
            instance: Some(0),
            field: "AccX".to_owned(),
        }
    }

    #[test]
    fn field_options_offer_every_source_and_the_selected_topics_numeric_fields() {
        let snapshot = snapshot_two_sources();
        let options = field_options(&snapshot, &imu_selector());

        assert_eq!(options.sources, ["flight_01", "flight_02"]);
        assert_eq!(options.topics, ["GPS", "IMU[0]", "IMU[1]"]);
        assert_eq!(
            options.fields,
            ["AccX"],
            "string fields must not be offered"
        );
    }

    #[test]
    fn field_options_follow_the_selected_source() {
        let snapshot = snapshot_two_sources();
        let mut selector = imu_selector();
        selector.source = Some("flight_02".to_owned());

        let options = field_options(&snapshot, &selector);

        assert_eq!(options.topics, ["GPS", "IMU[0]"]);
    }

    #[test]
    fn retargeting_a_topic_keeps_a_shared_field_and_otherwise_takes_the_first() {
        let snapshot = snapshot_two_sources();

        let mut selector = imu_selector();
        retarget_topic(&snapshot, &mut selector, "IMU[1]");
        assert_eq!(selector.instance, Some(1));
        assert_eq!(selector.field, "AccX");

        retarget_topic(&snapshot, &mut selector, "GPS");
        assert_eq!(selector.topic, "GPS");
        assert_eq!(selector.instance, None);
        assert_eq!(selector.field, "Alt");
    }
}
