use crate::handles::{ClientId, OwnerId};
use crate::leases::LeaseManager;
use crate::publications::PublicationRegistry;
use delog_core::identity::{FieldId, SourceId};
use delog_core::snapshot::StoreSnapshot;
use std::time::Instant;

use crate::handles::OpaqueId;
use crate::protocol::v1::error::ApiError;

#[derive(Debug, Clone)]
pub struct ResolvedControlField {
    pub id: FieldId,
    pub trace_path: String,
    pub vehicle_path: String,
    pub source_id: SourceId,
    pub source: String,
    pub topic: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedControlSource {
    pub id: SourceId,
    pub label: String,
}

/// Resolve snapshot-scoped and publication field handles against their live registries.
pub trait ControlFieldResolver {
    fn resolve(&self, handle: &OpaqueId) -> Result<ResolvedControlField, ApiError>;
    fn resolve_source(&self, _handle: &OpaqueId) -> Result<ResolvedControlSource, ApiError> {
        Err(ApiError::stale_handle("source handle is stale"))
    }
    fn describe(&self, _field: FieldId) -> Option<String> {
        None
    }
}

/// Backing resolver for the two existing field-handle lifetimes.
pub struct LiveFieldResolver<'a> {
    leases: &'a LeaseManager,
    publications: &'a PublicationRegistry,
    snapshot: &'a StoreSnapshot,
    client: &'a ClientId,
    owner: &'a OwnerId,
    now: Instant,
}

impl<'a> LiveFieldResolver<'a> {
    pub fn new(
        leases: &'a LeaseManager,
        publications: &'a PublicationRegistry,
        snapshot: &'a StoreSnapshot,
        client: &'a ClientId,
        owner: &'a OwnerId,
        now: Instant,
    ) -> Self {
        Self {
            leases,
            publications,
            snapshot,
            client,
            owner,
            now,
        }
    }
}

impl ControlFieldResolver for LiveFieldResolver<'_> {
    fn describe(&self, id: FieldId) -> Option<String> {
        let field = self
            .snapshot
            .fields
            .get(id.index())
            .filter(|field| !field.removed && field.id == id)?;
        let topic = self.snapshot.topic(field.topic)?;
        let source = self.snapshot.source(topic.entry.source)?;
        let external = source
            .entry
            .derived_provenance
            .as_ref()
            .and_then(|provenance| {
                provenance
                    .owner
                    .strip_prefix("external/")
                    .map(|owner| (owner, provenance.logical_topic.as_str()))
            });
        let (source_name, topic_name) =
            external.unwrap_or((source.entry.label.as_str(), topic.entry.name.as_str()));
        Some(format!("{source_name}/{topic_name}/{}", field.name))
    }

    fn resolve(&self, handle: &OpaqueId) -> Result<ResolvedControlField, ApiError> {
        let (id, publication) =
            if let Some(field) = self.leases.control_field(handle, self.client, self.now) {
                (field.field, false)
            } else {
                (
                    self.publications
                        .resolve_field_for_owner(self.owner, handle)?,
                    true,
                )
            };
        let field = self
            .snapshot
            .fields
            .get(id.index())
            .filter(|field| !field.removed && field.id == id)
            .ok_or_else(|| ApiError::stale_handle("field has been removed"))?;
        let topic = self
            .snapshot
            .topic(field.topic)
            .filter(|topic| !topic.entry.removed)
            .ok_or_else(|| ApiError::stale_handle("field topic has been removed"))?;
        let source = self
            .snapshot
            .source(topic.entry.source)
            .filter(|source| !source.entry.removed)
            .ok_or_else(|| ApiError::stale_handle("field source has been removed"))?;
        let (source_name, topic_name) = if publication {
            let provenance = source.entry.derived_provenance.as_ref().ok_or_else(|| {
                ApiError::internal("published field source has no publication provenance")
            })?;
            let owner = provenance.owner.strip_prefix("external/").ok_or_else(|| {
                ApiError::internal("published field source has an invalid owner namespace")
            })?;
            (owner.to_owned(), provenance.logical_topic.clone())
        } else {
            (source.entry.label.clone(), topic.entry.name.clone())
        };
        Ok(ResolvedControlField {
            id,
            trace_path: format!("{}.{}", topic.entry.name, field.name),
            vehicle_path: format!("{}/{}/{}", source.entry.label, topic.entry.name, field.name),
            source_id: source.entry.id,
            source: source_name,
            topic: topic_name,
            name: field.name.clone(),
        })
    }

    fn resolve_source(&self, handle: &OpaqueId) -> Result<ResolvedControlSource, ApiError> {
        let id = if let Some(id) = self.leases.control_source(handle, self.client, self.now) {
            id
        } else {
            self.publications
                .resolve_source_for_owner(self.owner, handle)?
        };
        let source = self
            .snapshot
            .source(id)
            .filter(|source| !source.entry.removed)
            .ok_or_else(|| ApiError::stale_handle("source has been removed"))?;
        Ok(ResolvedControlSource {
            id,
            label: source.entry.label.clone(),
        })
    }
}
