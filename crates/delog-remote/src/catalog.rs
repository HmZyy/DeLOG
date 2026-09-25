use std::collections::HashMap;

use delog_core::identity::{FieldId, SourceId, TopicId, parse_topic_instance};
use delog_core::snapshot::StoreSnapshot;

use crate::handles::OpaqueId;
use crate::protocol::v1::catalog::{
    CatalogDto, FieldDto, FieldSelectorDto, FieldTypeDto, SourceDto, TimeRangeDto, TopicDto,
};
use crate::protocol::v1::error::ApiError;

#[derive(Debug, Clone)]
pub struct FieldHandle {
    pub topic: TopicId,
    pub field: FieldId,
    pub dto: FieldDto,
}

#[derive(Debug, Default)]
pub struct LeaseHandles {
    sources: HashMap<OpaqueId, SourceId>,
    topics: HashMap<OpaqueId, TopicId>,
    fields: HashMap<OpaqueId, FieldHandle>,
}

impl LeaseHandles {
    pub fn source_id(&self, handle: &OpaqueId) -> Option<SourceId> {
        self.sources.get(handle).copied()
    }
    pub fn topic_id(&self, handle: &OpaqueId) -> Option<TopicId> {
        self.topics.get(handle).copied()
    }

    pub fn field(&self, handle: &OpaqueId) -> Option<&FieldHandle> {
        self.fields.get(handle)
    }
}

pub fn build(snapshot: &StoreSnapshot) -> Result<(CatalogDto, LeaseHandles), ApiError> {
    let mut handles = LeaseHandles::default();
    let mut sources = Vec::new();

    for source in snapshot.sources.iter() {
        if source.entry.removed {
            continue;
        }

        let offset_ns = checked_us_to_ns(source.entry.offset_us)?;
        let mut topics = Vec::new();

        for &topic_id in source.topics.iter() {
            let Some(topic) = snapshot.topic(topic_id) else {
                continue;
            };
            if topic.entry.removed {
                continue;
            }

            let (base_name, instance) = parse_topic_instance(&topic.entry.name);
            let topic_handle = OpaqueId::generate().map_err(random_generation_failed)?;

            let mut fields = Vec::new();
            let mut row_count = 0u64;
            let mut time_range_ns = None;

            if let Some(store) = topic.store.as_ref() {
                row_count = store.rows;
                if let Some(raw_range) = store.time_range() {
                    let effective = raw_range.offset(source.entry.offset_us).ok_or_else(|| {
                        ApiError::internal("source offset overflows the effective time range")
                    })?;
                    time_range_ns = Some(TimeRangeDto {
                        start_ns: checked_us_to_ns(effective.min_us)?,
                        end_ns: checked_us_to_ns(effective.max_us)?,
                    });
                }

                for field in snapshot.fields.iter() {
                    if field.removed || field.topic != topic_id {
                        continue;
                    }
                    let Some(schema_field) = store.schema.field_by_name(&field.name) else {
                        continue;
                    };

                    let field_handle = OpaqueId::generate().map_err(random_generation_failed)?;
                    let arrow_type = FieldTypeDto::from_arrow(&schema_field.dtype)?;
                    let selector = FieldSelectorDto {
                        source: source.entry.label.clone(),
                        topic: topic.entry.name.clone(),
                        instance,
                        field: field.name.clone(),
                    };
                    let dto = FieldDto {
                        handle: field_handle.clone(),
                        name: field.name.clone(),
                        selector,
                        arrow_type,
                        unit: schema_field.unit.clone(),
                        description: schema_field.description.clone(),
                        multiplier: schema_field.multiplier,
                    };

                    handles.fields.insert(
                        field_handle.clone(),
                        FieldHandle {
                            topic: topic_id,
                            field: field.id,
                            dto: dto.clone(),
                        },
                    );
                    fields.push(dto);
                }
            }

            handles.topics.insert(topic_handle.clone(), topic_id);
            topics.push(TopicDto {
                handle: topic_handle,
                name: topic.entry.name.clone(),
                base_name,
                instance,
                row_count,
                time_range_ns,
                fields,
            });
        }

        let source_handle = OpaqueId::generate().map_err(random_generation_failed)?;
        handles
            .sources
            .insert(source_handle.clone(), source.entry.id);
        sources.push(SourceDto {
            handle: source_handle,
            label: source.entry.label.clone(),
            kind: source.entry.kind.into(),
            offset_ns,
            topics,
        });
    }

    Ok((
        CatalogDto {
            snapshot_epoch: snapshot.epoch,
            sources,
        },
        handles,
    ))
}

fn checked_us_to_ns(us: i64) -> Result<i64, ApiError> {
    us.checked_mul(1000)
        .ok_or_else(|| ApiError::internal("timestamp overflows wire nanoseconds"))
}

fn random_generation_failed(error: getrandom::Error) -> ApiError {
    ApiError::internal(format!("failed to generate random bytes: {error}"))
}
