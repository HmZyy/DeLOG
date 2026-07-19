use delog_core::identity::{FieldId, SourceId, TopicId, parse_topic_instance};
use delog_core::snapshot::StoreSnapshot;

use crate::graph::FieldSelector;

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedField {
    pub source: SourceId,
    pub topic: TopicId,
    pub field: FieldId,
    pub unit: Option<String>,
    pub multiplier: f64,
    pub is_string: bool,
}

pub fn resolve_field(
    snapshot: &StoreSnapshot,
    selector: &FieldSelector,
) -> Result<ResolvedField, String> {
    let mut candidates = Vec::new();
    for source in snapshot.sources.iter() {
        if source.entry.removed {
            continue;
        }
        if let Some(requested) = selector.source.as_deref()
            && source.entry.label != requested
        {
            continue;
        }
        for &topic_id in source.topics.iter() {
            let Some(topic) = snapshot.topic(topic_id) else {
                continue;
            };
            if topic.entry.removed || topic.store.is_none() {
                continue;
            }
            let (base, instance) = parse_topic_instance(&topic.entry.name);
            let matches_structured = base == selector.topic && instance == selector.instance;
            let matches_full = selector.instance.is_none() && topic.entry.name == selector.topic;
            if matches_structured || matches_full {
                candidates.push((source, topic));
            }
        }
    }

    if candidates.is_empty() {
        let topic = selector_topic(selector);
        return Err(match selector.source.as_deref() {
            Some(source) => format!("topic '{topic}' not found in source '{source}'"),
            None => format!("topic '{topic}' not found"),
        });
    }
    if candidates.len() > 1 {
        let paths = candidates
            .iter()
            .map(|(source, topic)| format!("{} › {}", source.entry.label, topic.entry.name))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "topic '{}' is ambiguous — specify a source; candidates: {paths}",
            selector_topic(selector)
        ));
    }

    let (source, topic) = candidates[0];
    let field = snapshot
        .fields
        .iter()
        .find(|field| {
            !field.removed && field.topic == topic.entry.id && field.name == selector.field
        })
        .ok_or_else(|| {
            format!(
                "field '{}' not found in topic '{}'",
                selector.field, topic.entry.name
            )
        })?;
    let store = topic.store.as_ref().expect("candidates require a store");
    let schema = store.schema.field_by_name(&selector.field).ok_or_else(|| {
        format!(
            "field '{}' is missing from topic '{}' schema",
            selector.field, topic.entry.name
        )
    })?;

    Ok(ResolvedField {
        source: source.entry.id,
        topic: topic.entry.id,
        field: field.id,
        unit: schema.unit.clone(),
        multiplier: schema.multiplier,
        is_string: schema.is_string(),
    })
}

fn selector_topic(selector: &FieldSelector) -> String {
    match selector.instance {
        Some(instance) => format!("{}[{instance}]", selector.topic),
        None => selector.topic.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{snapshot_scaled_i16, snapshot_two_sources};

    #[test]
    fn resolves_field_multiplier() {
        let snap = snapshot_scaled_i16();
        let sel = FieldSelector {
            source: Some("flight".into()),
            topic: "SCALED".into(),
            instance: None,
            field: "A".into(),
        };
        assert_eq!(resolve_field(&snap, &sel).unwrap().multiplier, 0.01);
    }

    #[test]
    fn resolves_by_base_name_and_instance() {
        let snap = snapshot_two_sources();
        let sel = FieldSelector {
            source: Some("flight_01".into()),
            topic: "IMU".into(),
            instance: Some(0),
            field: "AccX".into(),
        };
        let hit = resolve_field(&snap, &sel).unwrap();
        assert_eq!(hit.unit.as_deref(), Some("m/s^2"));
        assert!(!hit.is_string);
    }

    #[test]
    fn ambiguous_without_source_lists_candidates() {
        let snap = snapshot_two_sources();
        let sel = FieldSelector {
            source: None,
            topic: "IMU".into(),
            instance: Some(0),
            field: "AccX".into(),
        };
        let err = resolve_field(&snap, &sel).unwrap_err();
        assert!(err.contains("flight_01") && err.contains("flight_02"));
    }

    #[test]
    fn missing_topic_field_and_source_report_clearly() {
        let snap = snapshot_two_sources();
        let missing_field = FieldSelector {
            source: Some("flight_01".into()),
            topic: "GPS".into(),
            instance: None,
            field: "Speed".into(),
        };
        assert!(
            resolve_field(&snap, &missing_field)
                .unwrap_err()
                .contains("Speed")
        );
        let missing_src = FieldSelector {
            source: Some("flight_09".into()),
            topic: "GPS".into(),
            instance: None,
            field: "Alt".into(),
        };
        assert!(
            resolve_field(&snap, &missing_src)
                .unwrap_err()
                .contains("flight_09")
        );
    }
}
