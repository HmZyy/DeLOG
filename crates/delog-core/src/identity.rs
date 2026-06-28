//! Runtime IDs are dense `u32` indices, meaningful only within one process
//! session; persisted layouts use [`FieldKey`] instead.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::time::{TimestampUs, effective_time_us};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TopicId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldKey {
    pub source: String,
    pub topic: String,
    pub field: String,
}

/// `removed` is a tombstone: the slot is kept so existing runtime IDs stay
/// valid for the session, but readers and the cache GC skip it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    pub id: SourceId,
    pub label: String,
    pub offset_us: TimestampUs,
    pub meta: SourceMetadata,
    pub removed: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SourceMetadata {
    pub params: Vec<SourceParam>,
    pub auto_markers: Vec<AutoMarker>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceParam {
    pub name: String,
    pub ty: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMarker {
    pub time_us: TimestampUs,
    pub level: u8,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicEntry {
    pub id: TopicId,
    pub source: SourceId,
    pub name: String,
    pub removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldEntry {
    pub id: FieldId,
    pub topic: TopicId,
    pub name: String,
    pub removed: bool,
}

/// IDs orphaned by [`IdentityRegistry::remove_source`], handed to the writer to
/// drop their stores and the cache manager to GC their caches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedSource {
    pub source: SourceId,
    pub topics: Vec<TopicId>,
    pub fields: Vec<FieldId>,
}

#[derive(Debug, Default, Clone)]
pub struct IdentityRegistry {
    sources: Vec<SourceEntry>,
    topics: Vec<TopicEntry>,
    fields: Vec<FieldEntry>,
    source_base_counts: HashMap<String, u32>,
    source_labels: HashSet<String>,
    topic_by_source_name: HashMap<(SourceId, String), TopicId>,
    field_by_topic_name: HashMap<(TopicId, String), FieldId>,
    field_by_key: HashMap<FieldKey, FieldId>,
}

impl SourceId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl TopicId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl FieldId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl FieldKey {
    pub fn new(
        source: impl Into<String>,
        topic: impl Into<String>,
        field: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            topic: topic.into(),
            field: field.into(),
        }
    }
}

impl IdentityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A label already in use receives `#2`, `#3`, ... suffixes.
    pub fn add_source(&mut self, preferred_label: impl Into<String>) -> SourceId {
        let label = self.unique_source_label(preferred_label.into());
        let id = SourceId(next_id(self.sources.len(), "source"));
        self.sources.push(SourceEntry {
            id,
            label,
            offset_us: 0,
            meta: SourceMetadata::default(),
            removed: false,
        });
        id
    }

    pub fn add_source_from_path(&mut self, path: impl AsRef<Path>) -> SourceId {
        let label = path
            .as_ref()
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("source");
        self.add_source(label)
    }

    pub fn add_source_from_endpoint(&mut self, endpoint: impl Into<String>) -> SourceId {
        self.add_source(endpoint)
    }

    pub fn add_topic(&mut self, source: SourceId, name: impl Into<String>) -> Option<TopicId> {
        self.live_source(source)?;
        let name = name.into();
        let key = (source, name.clone());
        if let Some(&id) = self.topic_by_source_name.get(&key) {
            return Some(id);
        }

        let id = TopicId(next_id(self.topics.len(), "topic"));
        self.topics.push(TopicEntry {
            id,
            source,
            name,
            removed: false,
        });
        self.topic_by_source_name.insert(key, id);
        Some(id)
    }

    /// Surfaces multi-instance topics as `topic[N]` so each instance gets
    /// independent caches, styling and browser rows.
    pub fn add_topic_instance(
        &mut self,
        source: SourceId,
        base_name: impl AsRef<str>,
        instance: u32,
    ) -> Option<TopicId> {
        self.add_topic(source, topic_instance_name(base_name, instance))
    }

    pub fn add_field(&mut self, topic: TopicId, name: impl Into<String>) -> Option<FieldId> {
        let topic_entry = self.live_topic(topic)?.clone();
        let source_entry = self.live_source(topic_entry.source)?.clone();
        let name = name.into();
        let local_key = (topic, name.clone());
        if let Some(&id) = self.field_by_topic_name.get(&local_key) {
            return Some(id);
        }

        let id = FieldId(next_id(self.fields.len(), "field"));
        let portable_key = FieldKey {
            source: source_entry.label,
            topic: topic_entry.name,
            field: name.clone(),
        };
        self.fields.push(FieldEntry {
            id,
            topic,
            name,
            removed: false,
        });
        self.field_by_topic_name.insert(local_key, id);
        self.field_by_key.insert(portable_key, id);
        Some(id)
    }

    /// Tombstones the source and every topic/field it owns: IDs are preserved
    /// (session references stay valid) but dropped from the lookup maps, so they
    /// no longer resolve and a later re-add of the same name mints fresh IDs.
    /// `None` if the source is unknown or already removed.
    pub fn remove_source(&mut self, id: SourceId) -> Option<RemovedSource> {
        self.live_source(id)?;

        let label = self.sources[id.index()].label.clone();
        self.sources[id.index()].removed = true;
        self.source_labels.remove(&label);

        let mut topics = Vec::new();
        let mut fields = Vec::new();
        for topic_index in 0..self.topics.len() {
            if self.topics[topic_index].source != id || self.topics[topic_index].removed {
                continue;
            }
            let topic_id = self.topics[topic_index].id;
            self.topics[topic_index].removed = true;
            self.topic_by_source_name
                .remove(&(id, self.topics[topic_index].name.clone()));
            topics.push(topic_id);

            for field_index in 0..self.fields.len() {
                if self.fields[field_index].topic != topic_id || self.fields[field_index].removed {
                    continue;
                }
                let field_id = self.fields[field_index].id;
                self.fields[field_index].removed = true;
                let field_name = self.fields[field_index].name.clone();
                self.field_by_topic_name
                    .remove(&(topic_id, field_name.clone()));
                self.field_by_key.remove(&FieldKey {
                    source: label.clone(),
                    topic: self.topics[topic_index].name.clone(),
                    field: field_name,
                });
                fields.push(field_id);
            }
        }

        Some(RemovedSource {
            source: id,
            topics,
            fields,
        })
    }

    pub fn source(&self, id: SourceId) -> Option<&SourceEntry> {
        self.sources.get(id.index()).filter(|entry| entry.id == id)
    }

    pub fn live_source(&self, id: SourceId) -> Option<&SourceEntry> {
        self.source(id).filter(|entry| !entry.removed)
    }

    pub fn live_topic(&self, id: TopicId) -> Option<&TopicEntry> {
        self.topic(id).filter(|entry| !entry.removed)
    }

    pub fn live_field(&self, id: FieldId) -> Option<&FieldEntry> {
        self.field(id).filter(|entry| !entry.removed)
    }

    pub fn set_source_offset_us(
        &mut self,
        id: SourceId,
        offset_us: TimestampUs,
    ) -> Option<TimestampUs> {
        let source = self
            .sources
            .get_mut(id.index())
            .filter(|entry| entry.id == id)?;
        let old = source.offset_us;
        source.offset_us = offset_us;
        Some(old)
    }

    pub fn set_source_metadata(
        &mut self,
        id: SourceId,
        meta: SourceMetadata,
    ) -> Option<SourceMetadata> {
        let source = self
            .sources
            .get_mut(id.index())
            .filter(|entry| entry.id == id)?;
        Some(std::mem::replace(&mut source.meta, meta))
    }

    pub fn effective_source_time_us(
        &self,
        id: SourceId,
        raw_us: TimestampUs,
    ) -> Option<TimestampUs> {
        let source = self.source(id)?;
        effective_time_us(raw_us, source.offset_us)
    }

    pub fn topic(&self, id: TopicId) -> Option<&TopicEntry> {
        self.topics.get(id.index()).filter(|entry| entry.id == id)
    }

    pub fn field(&self, id: FieldId) -> Option<&FieldEntry> {
        self.fields.get(id.index()).filter(|entry| entry.id == id)
    }

    pub fn sources(&self) -> &[SourceEntry] {
        &self.sources
    }

    pub fn topics(&self) -> &[TopicEntry] {
        &self.topics
    }

    pub fn fields(&self) -> &[FieldEntry] {
        &self.fields
    }

    pub fn field_key(&self, id: FieldId) -> Option<FieldKey> {
        let field = self.field(id)?;
        let topic = self.topic(field.topic)?;
        let source = self.source(topic.source)?;
        Some(FieldKey {
            source: source.label.clone(),
            topic: topic.name.clone(),
            field: field.name.clone(),
        })
    }

    pub fn resolve(&self, key: &FieldKey) -> Option<FieldId> {
        self.field_by_key.get(key).copied()
    }

    fn unique_source_label(&mut self, preferred_label: String) -> String {
        let base = if preferred_label.is_empty() {
            "source".to_owned()
        } else {
            preferred_label
        };
        // Reuse the bare label when free (e.g. after a remove freed it), so
        // reopening a closed source keeps its original label.
        if self.source_labels.insert(base.clone()) {
            return base;
        }
        let count = self.source_base_counts.entry(base.clone()).or_insert(1);
        loop {
            *count += 1;
            let candidate = format!("{base}#{count}");
            if self.source_labels.insert(candidate.clone()) {
                return candidate;
            }
        }
    }
}

impl SourceMetadata {
    pub fn is_empty(&self) -> bool {
        self.params.is_empty() && self.auto_markers.is_empty()
    }
}

fn next_id(len: usize, kind: &str) -> u32 {
    u32::try_from(len).unwrap_or_else(|_| panic!("too many {kind} IDs"))
}

pub fn topic_instance_name(base_name: impl AsRef<str>, instance: u32) -> String {
    format!("{}[{instance}]", base_name.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_labels_use_file_stem_and_collision_suffixes() {
        let mut ids = IdentityRegistry::new();

        let first = ids.add_source_from_path("/logs/flight_0042.BIN");
        let second = ids.add_source("flight_0042");
        let third = ids.add_source("flight_0042");

        assert_eq!(ids.source(first).unwrap().label, "flight_0042");
        assert_eq!(ids.source(second).unwrap().label, "flight_0042#2");
        assert_eq!(ids.source(third).unwrap().label, "flight_0042#3");
    }

    #[test]
    fn source_suffixing_never_reuses_existing_label() {
        let mut ids = IdentityRegistry::new();

        let explicit_suffix = ids.add_source("flight#2");
        let first = ids.add_source("flight");
        let second = ids.add_source("flight");

        assert_eq!(ids.source(explicit_suffix).unwrap().label, "flight#2");
        assert_eq!(ids.source(first).unwrap().label, "flight");
        assert_eq!(ids.source(second).unwrap().label, "flight#3");
    }

    #[test]
    fn topics_and_fields_are_dense_and_resolvable_by_portable_key() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source_from_endpoint("mavlink:udp:14550");
        let topic = ids.add_topic(source, "BARO").unwrap();
        let field = ids.add_field(topic, "Alt").unwrap();

        assert_eq!(source, SourceId(0));
        assert_eq!(topic, TopicId(0));
        assert_eq!(field, FieldId(0));

        let key = FieldKey::new("mavlink:udp:14550", "BARO", "Alt");
        assert_eq!(ids.field_key(field).unwrap(), key);
        assert_eq!(ids.resolve(&key), Some(field));
        assert_eq!(ids.resolve(&FieldKey::new("missing", "BARO", "Alt")), None);
    }

    #[test]
    fn duplicate_topic_or_field_registration_returns_existing_id() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let topic = ids.add_topic(source, "GPS").unwrap();
        let field = ids.add_field(topic, "Lat").unwrap();

        assert_eq!(ids.add_topic(source, "GPS"), Some(topic));
        assert_eq!(ids.add_field(topic, "Lat"), Some(field));
        assert_eq!(ids.topics().len(), 1);
        assert_eq!(ids.fields().len(), 1);
    }

    #[test]
    fn same_topic_name_under_different_sources_gets_distinct_topics() {
        let mut ids = IdentityRegistry::new();
        let source_a = ids.add_source("a");
        let source_b = ids.add_source("b");

        let topic_a = ids.add_topic(source_a, "GPS").unwrap();
        let topic_b = ids.add_topic(source_b, "GPS").unwrap();

        assert_ne!(topic_a, topic_b);
        assert_eq!(topic_a, TopicId(0));
        assert_eq!(topic_b, TopicId(1));
    }

    #[test]
    fn multi_instance_topic_names_use_bracket_suffixes() {
        assert_eq!(topic_instance_name("GPS", 0), "GPS[0]");
        assert_eq!(
            topic_instance_name("vehicle_local_position", 3),
            "vehicle_local_position[3]"
        );
    }

    #[test]
    fn multi_instance_topics_register_as_distinct_topics() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");

        let gps0 = ids.add_topic_instance(source, "GPS", 0).unwrap();
        let gps1 = ids.add_topic_instance(source, "GPS", 1).unwrap();
        let gps0_again = ids.add_topic_instance(source, "GPS", 0).unwrap();

        assert_eq!(gps0, TopicId(0));
        assert_eq!(gps1, TopicId(1));
        assert_eq!(gps0_again, gps0);
        assert_eq!(ids.topic(gps0).unwrap().name, "GPS[0]");
        assert_eq!(ids.topic(gps1).unwrap().name, "GPS[1]");
    }

    #[test]
    fn invalid_parent_ids_are_rejected() {
        let mut ids = IdentityRegistry::new();

        assert_eq!(ids.add_topic(SourceId(99), "GPS"), None);
        assert_eq!(ids.add_field(TopicId(99), "Lat"), None);
    }

    #[test]
    fn remove_source_tombstones_its_topics_and_fields_and_returns_orphans() {
        let mut ids = IdentityRegistry::new();
        let keep = ids.add_source("keep");
        let drop = ids.add_source("drop");
        let keep_topic = ids.add_topic(keep, "BARO").unwrap();
        let keep_field = ids.add_field(keep_topic, "Alt").unwrap();
        let drop_topic = ids.add_topic(drop, "GPS").unwrap();
        let drop_lat = ids.add_field(drop_topic, "Lat").unwrap();
        let drop_lon = ids.add_field(drop_topic, "Lon").unwrap();

        let removed = ids.remove_source(drop).unwrap();
        assert_eq!(removed.source, drop);
        assert_eq!(removed.topics, vec![drop_topic]);
        assert_eq!(removed.fields, vec![drop_lat, drop_lon]);

        assert!(ids.source(drop).unwrap().removed);
        assert!(ids.live_source(drop).is_none());
        assert!(ids.live_topic(drop_topic).is_none());
        assert!(ids.live_field(drop_lat).is_none());

        assert_eq!(ids.live_source(keep).unwrap().id, keep);
        assert_eq!(ids.live_field(keep_field).unwrap().name, "Alt");

        assert_eq!(ids.resolve(&FieldKey::new("drop", "GPS", "Lat")), None);
        assert_eq!(
            ids.resolve(&FieldKey::new("keep", "BARO", "Alt")),
            Some(keep_field)
        );
        assert_eq!(ids.remove_source(drop), None);
        assert_eq!(ids.remove_source(SourceId(99)), None);
    }

    #[test]
    fn re_adding_a_removed_name_mints_fresh_ids_and_reuses_the_label() {
        let mut ids = IdentityRegistry::new();
        let first = ids.add_source("flight");
        let topic = ids.add_topic(first, "GPS").unwrap();
        ids.add_field(topic, "Lat").unwrap();
        ids.remove_source(first);

        let second = ids.add_source("flight");
        let new_topic = ids.add_topic(second, "GPS").unwrap();
        let new_field = ids.add_field(new_topic, "Lat").unwrap();

        assert_ne!(first, second);
        assert_ne!(topic, new_topic);
        assert_eq!(ids.source(second).unwrap().label, "flight");
        assert_eq!(
            ids.resolve(&FieldKey::new("flight", "GPS", "Lat")),
            Some(new_field)
        );
    }

    #[test]
    fn cannot_add_topics_or_fields_under_a_removed_parent() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let topic = ids.add_topic(source, "GPS").unwrap();
        ids.remove_source(source);

        assert_eq!(ids.add_topic(source, "NEW"), None);
        assert_eq!(ids.add_field(topic, "Lat"), None);
    }

    #[test]
    fn source_offsets_default_to_zero_and_apply_to_effective_time() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");

        assert_eq!(ids.source(source).unwrap().offset_us, 0);
        assert_eq!(ids.effective_source_time_us(source, 1_000), Some(1_000));

        assert_eq!(ids.set_source_offset_us(source, -250), Some(0));
        assert_eq!(ids.source(source).unwrap().offset_us, -250);
        assert_eq!(ids.effective_source_time_us(source, 1_000), Some(750));
    }

    #[test]
    fn source_metadata_can_be_replaced() {
        let mut ids = IdentityRegistry::new();
        let source = ids.add_source("flight");
        let meta = SourceMetadata {
            params: vec![SourceParam {
                name: "SYS_AUTOSTART".to_owned(),
                ty: "int32_t".to_owned(),
                value: "4001".to_owned(),
            }],
            auto_markers: vec![AutoMarker {
                time_us: 1_000,
                level: 6,
                text: "armed".to_owned(),
            }],
        };

        assert!(ids.source(source).unwrap().meta.is_empty());
        assert_eq!(
            ids.set_source_metadata(source, meta.clone()),
            Some(SourceMetadata::default())
        );
        assert_eq!(ids.source(source).unwrap().meta, meta);
        assert_eq!(ids.set_source_metadata(SourceId(99), meta), None);
    }
}
