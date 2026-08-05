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
                    "{} {} {} {}",
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

    fn imu_topic(identity: &mut IdentityRegistry, source: SourceId) -> (TopicId, Arc<TopicStore>) {
        let topic = identity.add_topic(source, "IMU[0]").unwrap();
        identity.add_field(topic, "AccX").unwrap();
        identity.add_field(topic, "name").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "IMU[0]",
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

    fn snapshot_two_sources() -> StoreSnapshot {
        let mut identity = IdentityRegistry::new();
        let flight_01 = identity.add_source("flight_01");
        let flight_02 = identity.add_source("flight_02");
        let stores = [
            imu_topic(&mut identity, flight_01),
            gps_topic(&mut identity, flight_01),
            imu_topic(&mut identity, flight_02),
            gps_topic(&mut identity, flight_02),
        ];
        StoreSnapshot::from_registry(&identity, stores, 5).unwrap()
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
}
