use std::collections::HashMap;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use delog_core::derived::{
    DerivedCommit, DerivedCommitError, DerivedCommitReceipt, DerivedProvenance,
    PreparedDerivedSource,
};
use delog_core::identity::{FieldId, SourceId};
use delog_core::ingest::IngestSender;
use delog_core::snapshot::{DataStore, StoreSnapshot};
use tokio_util::sync::CancellationToken;

use crate::auth::{OwnerRegistry, external_owner_name};
use crate::handles::{OpaqueId, OwnerId};
use crate::protocol::v1::catalog::FieldTypeDto;
use crate::protocol::v1::error::{ApiError, WireCompletion};
use crate::protocol::v1::publication::{PublicationDto, PublicationFieldDto, PublicationTopicDto};

pub(crate) const RECEIPT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
struct PublicationEntry {
    source: SourceId,
    generation: u64,
    commit_nonce: [u8; 16],
    publication: OpaqueId,
    fields: HashMap<OpaqueId, FieldId>,
    dto: PublicationDto,
}

#[derive(Debug, Default)]
struct RegistryState {
    by_key: HashMap<(OwnerId, String), PublicationEntry>,
    by_handle: HashMap<OpaqueId, (OwnerId, String)>,
    fields: HashMap<OpaqueId, FieldId>,
}

#[derive(Debug)]
pub struct OwnerRemoval {
    pub removed: usize,
    pub error: Option<ApiError>,
}

#[derive(Debug)]
pub struct PublicationRegistry {
    state: Arc<Mutex<RegistryState>>,
    receipt_timeout: Duration,
}

impl Default for PublicationRegistry {
    fn default() -> Self {
        Self::with_receipt_timeout(RECEIPT_TIMEOUT)
    }
}

fn install(
    state: &mut RegistryState,
    key: (OwnerId, String),
    previous: Option<&PublicationEntry>,
    entry: PublicationEntry,
) -> PublicationDto {
    if let Some(previous) = previous {
        state.by_handle.remove(&previous.publication);
        for field in previous.fields.keys() {
            state.fields.remove(field);
        }
    }
    state
        .by_handle
        .insert(entry.publication.clone(), key.clone());
    state.fields.extend(entry.fields.clone());
    let dto = entry.dto.clone();
    state.by_key.insert(key, entry);
    dto
}

impl PublicationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_receipt_timeout(receipt_timeout: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(RegistryState::default())),
            receipt_timeout,
        }
    }

    pub fn rebuild(
        snapshot: &StoreSnapshot,
        owners: &OwnerRegistry,
        receipt_timeout: Duration,
    ) -> Self {
        let registry = Self::with_receipt_timeout(receipt_timeout);
        {
            let mut state = registry
                .state
                .lock()
                .expect("publication registry poisoned");
            for source in snapshot
                .sources
                .iter()
                .filter(|source| !source.entry.removed)
            {
                let Some(name) = source
                    .entry
                    .derived_provenance
                    .as_ref()
                    .and_then(|provenance| provenance.owner.strip_prefix("external/"))
                else {
                    continue;
                };
                match owners.owner(name) {
                    Ok(owner) => adopt(&mut state, &owner, source.entry.id, snapshot),
                    Err(error) => {
                        tracing::warn!(%error, "could not restore a publication owner");
                    }
                }
            }
        }
        registry
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &self,
        owner: &OwnerId,
        owner_name: &str,
        topic_name: &str,
        replace: bool,
        source: PreparedDerivedSource,
        sender: &IngestSender,
        store: &Arc<DataStore>,
        work: &CancellationToken,
    ) -> Result<PublicationDto, ApiError> {
        validate_name(topic_name)?;
        let key = (owner.clone(), topic_name.to_owned());
        let mut state = self.state.lock().expect("publication registry poisoned");
        if work.is_cancelled() {
            return Err(ApiError::unavailable(
                "the client was revoked before its publication committed",
            )
            .with_completion(WireCompletion::NotStarted));
        }
        let previous = state.by_key.get(&key).cloned();
        if previous.is_some() && !replace {
            return Err(ApiError::conflict("publication already exists"));
        }
        let generation = previous.as_ref().map_or(Ok(1), |entry| {
            entry
                .generation
                .checked_add(1)
                .ok_or_else(|| ApiError::internal("publication generation overflow"))
        })?;
        let mut commit_nonce = [0_u8; 16];
        getrandom::fill(&mut commit_nonce).map_err(|error| {
            ApiError::internal(format!("could not generate commit nonce: {error}"))
        })?;
        let provenance = DerivedProvenance {
            owner: external_owner_name(owner_name),
            logical_topic: topic_name.to_owned(),
            generation,
            commit_nonce,
        };
        let commit = DerivedCommit {
            key: format!("external:{owner_name}/{topic_name}"),
            provenance: provenance.clone(),
            previous: previous.as_ref().map(|entry| entry.source),
            source,
        };
        let receipt = sender.commit_derived(commit).map_err(|_| {
            ApiError::unavailable("ingest writer is unavailable")
                .with_completion(WireCompletion::NotStarted)
        })?;
        let receipt = match receipt.recv_timeout(self.receipt_timeout) {
            Ok(Ok(receipt)) => receipt,
            Ok(Err(error)) => return Err(map_commit_error(error)),
            Err(RecvTimeoutError::Disconnected) => {
                reconcile_commit(store.load().as_ref(), &provenance, previous.as_ref())?
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(receipt) = committed(store.load().as_ref(), &provenance) {
                    receipt
                } else {
                    self.adopt_late_receipt(
                        receipt,
                        key,
                        previous,
                        topic_name.to_owned(),
                        generation,
                        Arc::clone(store),
                    );
                    return Err(
                        ApiError::unavailable("publication commit outcome is unknown")
                            .with_completion(WireCompletion::Unknown),
                    );
                }
            }
        };

        let snapshot = store.load();
        let entry = build_entry(topic_name, generation, receipt, snapshot.as_ref())?;
        Ok(install(&mut state, key, previous.as_ref(), entry))
    }

    fn adopt_late_receipt(
        &self,
        receipt: std::sync::mpsc::Receiver<Result<DerivedCommitReceipt, DerivedCommitError>>,
        key: (OwnerId, String),
        previous: Option<PublicationEntry>,
        topic_name: String,
        generation: u64,
        store: Arc<DataStore>,
    ) {
        let state = Arc::clone(&self.state);
        let spawned = std::thread::Builder::new()
            .name("delog-publication-receipt".into())
            .spawn(move || {
                let Ok(Ok(receipt)) = receipt.recv() else {
                    return;
                };
                let mut state = state.lock().expect("publication registry poisoned");
                let expected = previous.as_ref().map(|entry| entry.commit_nonce);
                if state.by_key.get(&key).map(|entry| entry.commit_nonce) != expected {
                    return;
                }
                if let Ok(entry) =
                    build_entry(&topic_name, generation, receipt, store.load().as_ref())
                {
                    install(&mut state, key, previous.as_ref(), entry);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not watch a late publication receipt");
        }
    }

    pub fn remove(
        &self,
        owner: &OwnerId,
        publication: &OpaqueId,
        sender: &IngestSender,
        store: &Arc<DataStore>,
    ) -> Result<(), ApiError> {
        let registry = Arc::clone(&self.state);
        let mut state = registry.lock().expect("publication registry poisoned");
        let key = state
            .by_handle
            .get(publication)
            .cloned()
            .ok_or_else(|| ApiError::stale_handle("publication handle is stale"))?;
        if &key.0 != owner {
            return Err(ApiError::forbidden("publication belongs to another owner"));
        }
        remove_key(
            &registry,
            &mut state,
            &key,
            sender,
            store,
            self.receipt_timeout,
        )
    }

    pub fn remove_owner(
        &self,
        owner: &OwnerId,
        owner_name: &str,
        sender: &IngestSender,
        store: &Arc<DataStore>,
    ) -> OwnerRemoval {
        let registry = Arc::clone(&self.state);
        let mut state = registry.lock().expect("publication registry poisoned");
        let snapshot = store.load();
        let namespace = external_owner_name(owner_name);
        for source in snapshot.sources.iter().filter(|source| {
            !source.entry.removed
                && source
                    .entry
                    .derived_provenance
                    .as_ref()
                    .is_some_and(|provenance| provenance.owner == namespace)
        }) {
            adopt(&mut state, owner, source.entry.id, snapshot.as_ref());
        }
        let mut keys: Vec<_> = state
            .by_key
            .keys()
            .filter(|key| &key.0 == owner)
            .cloned()
            .collect();
        keys.sort_by(|a, b| a.1.cmp(&b.1));
        let mut removal = OwnerRemoval {
            removed: 0,
            error: None,
        };
        for key in keys {
            match remove_key(
                &registry,
                &mut state,
                &key,
                sender,
                store,
                self.receipt_timeout,
            ) {
                Ok(()) => removal.removed += 1,
                Err(error) => {
                    removal.error.get_or_insert(error);
                }
            }
        }
        removal
    }

    pub fn resolve_field(&self, handle: &OpaqueId) -> Result<FieldId, ApiError> {
        self.state
            .lock()
            .expect("publication registry poisoned")
            .fields
            .get(handle)
            .copied()
            .ok_or_else(|| ApiError::stale_handle("publication field handle is stale"))
    }

    pub fn resolve_field_for_owner(
        &self,
        owner: &OwnerId,
        handle: &OpaqueId,
    ) -> Result<FieldId, ApiError> {
        let state = self.state.lock().expect("publication registry poisoned");
        state
            .by_key
            .iter()
            .find_map(|((candidate, _), entry)| {
                (candidate == owner)
                    .then(|| entry.fields.get(handle))
                    .flatten()
                    .copied()
            })
            .ok_or_else(|| ApiError::stale_handle("publication field handle is stale"))
    }

    pub fn resolve_source_for_owner(
        &self,
        owner: &OwnerId,
        handle: &OpaqueId,
    ) -> Result<SourceId, ApiError> {
        let state = self.state.lock().expect("publication registry poisoned");
        let key = state
            .by_handle
            .get(handle)
            .ok_or_else(|| ApiError::stale_handle("publication handle is stale"))?;
        if &key.0 != owner {
            return Err(ApiError::stale_handle("publication handle is stale"));
        }
        state
            .by_key
            .get(key)
            .map(|entry| entry.source)
            .ok_or_else(|| ApiError::stale_handle("publication handle is stale"))
    }
}

fn remove_key(
    registry: &Arc<Mutex<RegistryState>>,
    state: &mut RegistryState,
    key: &(OwnerId, String),
    sender: &IngestSender,
    store: &Arc<DataStore>,
    receipt_timeout: Duration,
) -> Result<(), ApiError> {
    let entry = state
        .by_key
        .get(key)
        .cloned()
        .ok_or_else(|| ApiError::stale_handle("publication handle is stale"))?;
    let receipt = sender.remove_source_wait(entry.source).map_err(|_| {
        ApiError::unavailable("ingest writer is unavailable")
            .with_completion(WireCompletion::NotStarted)
    })?;
    match receipt.recv_timeout(receipt_timeout) {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => return Err(map_commit_error(error)),
        Err(RecvTimeoutError::Timeout) => {
            if store.load().is_source_live(entry.source) {
                watch_late_removal(
                    Arc::clone(registry),
                    receipt,
                    key.clone(),
                    entry,
                    Arc::clone(store),
                );
                return Err(
                    ApiError::unavailable("publication removal outcome is unknown")
                        .with_completion(WireCompletion::Unknown),
                );
            }
        }
        Err(RecvTimeoutError::Disconnected) => {
            if store.load().is_source_live(entry.source) {
                return Err(
                    ApiError::unavailable("publication removal was not observed")
                        .with_completion(WireCompletion::NotStarted),
                );
            }
        }
    }
    uninstall(state, key, &entry);
    Ok(())
}

fn watch_late_removal(
    registry: Arc<Mutex<RegistryState>>,
    receipt: std::sync::mpsc::Receiver<Result<u64, DerivedCommitError>>,
    key: (OwnerId, String),
    entry: PublicationEntry,
    store: Arc<DataStore>,
) {
    let spawned = std::thread::Builder::new()
        .name("delog-publication-removal-receipt".into())
        .spawn(move || {
            let acknowledged = matches!(receipt.recv(), Ok(Ok(_)));
            if !acknowledged && store.load().is_source_live(entry.source) {
                return;
            }
            let mut state = registry.lock().expect("publication registry poisoned");
            uninstall(&mut state, &key, &entry);
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not watch a late publication removal receipt");
    }
}

fn uninstall(state: &mut RegistryState, key: &(OwnerId, String), expected: &PublicationEntry) {
    if !state.by_key.get(key).is_some_and(|entry| {
        entry.source == expected.source && entry.publication == expected.publication
    }) {
        return;
    }
    state.by_key.remove(key);
    state.by_handle.remove(&expected.publication);
    for field in expected.fields.keys() {
        state.fields.remove(field);
    }
}

fn adopt(state: &mut RegistryState, owner: &OwnerId, source: SourceId, snapshot: &StoreSnapshot) {
    let Some(provenance) = snapshot
        .source(source)
        .and_then(|source| source.entry.derived_provenance.clone())
    else {
        return;
    };
    let key = (owner.clone(), provenance.logical_topic.clone());
    if state
        .by_key
        .get(&key)
        .is_some_and(|entry| entry.source == source)
    {
        return;
    }
    let receipt = DerivedCommitReceipt {
        source,
        epoch: snapshot.epoch,
    };
    match build_entry(
        &provenance.logical_topic,
        provenance.generation,
        receipt,
        snapshot,
    ) {
        Ok(entry) => {
            let previous = state.by_key.get(&key).cloned();
            install(state, key, previous.as_ref(), entry);
        }
        Err(error) => tracing::warn!(%error, "could not restore a live publication"),
    }
}

fn validate_name(name: &str) -> Result<(), ApiError> {
    if name.trim().is_empty() || name.chars().count() > 128 || name.contains('/') {
        return Err(ApiError::invalid_input(
            "publication topic name must be 1-128 characters and contain no '/'",
        ));
    }
    Ok(())
}

fn current_source<'a>(
    snapshot: &'a StoreSnapshot,
    provenance: &DerivedProvenance,
) -> Option<&'a delog_core::snapshot::SourceSnapshot> {
    snapshot.sources.iter().find(|source| {
        !source.entry.removed
            && source
                .entry
                .derived_provenance
                .as_ref()
                .is_some_and(|candidate| {
                    candidate.owner == provenance.owner
                        && candidate.logical_topic == provenance.logical_topic
                })
    })
}

fn committed(
    snapshot: &StoreSnapshot,
    provenance: &DerivedProvenance,
) -> Option<DerivedCommitReceipt> {
    current_source(snapshot, provenance)
        .filter(|source| {
            source
                .entry
                .derived_provenance
                .as_ref()
                .is_some_and(|candidate| {
                    candidate.generation == provenance.generation
                        && candidate.commit_nonce == provenance.commit_nonce
                })
        })
        .map(|source| DerivedCommitReceipt {
            source: source.entry.id,
            epoch: snapshot.epoch,
        })
}

fn reconcile_commit(
    snapshot: &StoreSnapshot,
    provenance: &DerivedProvenance,
    previous: Option<&PublicationEntry>,
) -> Result<DerivedCommitReceipt, ApiError> {
    if let Some(receipt) = committed(snapshot, provenance) {
        return Ok(receipt);
    }
    let current = current_source(snapshot, provenance);
    let previous_is_current = previous.is_some_and(|previous| {
        current.is_some_and(|source| {
            source.entry.id == previous.source
                && source
                    .entry
                    .derived_provenance
                    .as_ref()
                    .is_some_and(|candidate| {
                        candidate.generation == previous.generation
                            && candidate.commit_nonce == previous.commit_nonce
                    })
        })
    });
    if previous_is_current || (previous.is_none() && current.is_none()) {
        return Err(ApiError::unavailable("publication commit was not observed")
            .with_completion(WireCompletion::NotStarted));
    }
    Err(
        ApiError::unavailable("publication commit outcome is unknown")
            .with_completion(WireCompletion::Unknown),
    )
}

fn build_entry(
    topic_name: &str,
    generation: u64,
    receipt: DerivedCommitReceipt,
    snapshot: &StoreSnapshot,
) -> Result<PublicationEntry, ApiError> {
    let source = snapshot
        .source(receipt.source)
        .filter(|source| !source.entry.removed)
        .ok_or_else(|| ApiError::internal("committed publication source is not visible"))?;
    let native_topic = source
        .topics
        .iter()
        .copied()
        .find(|topic| snapshot.is_topic_live(*topic))
        .ok_or_else(|| ApiError::internal("committed publication topic is not visible"))?;
    let store = snapshot
        .topic_store(native_topic)
        .ok_or_else(|| ApiError::internal("committed publication data is not visible"))?;
    let publication = OpaqueId::generate().map_err(|error| {
        ApiError::internal(format!("could not generate publication handle: {error}"))
    })?;
    let topic = OpaqueId::generate()
        .map_err(|error| ApiError::internal(format!("could not generate topic handle: {error}")))?;
    let mut fields = HashMap::new();
    let mut field_dtos = Vec::new();
    for field in snapshot
        .fields
        .iter()
        .filter(|field| !field.removed && field.topic == native_topic)
    {
        let schema = store
            .schema
            .field_by_name(&field.name)
            .ok_or_else(|| ApiError::internal("committed publication field schema is missing"))?;
        let handle = OpaqueId::generate().map_err(|error| {
            ApiError::internal(format!("could not generate field handle: {error}"))
        })?;
        fields.insert(handle.clone(), field.id);
        field_dtos.push(PublicationFieldDto {
            handle,
            name: field.name.clone(),
            arrow_type: FieldTypeDto::from_arrow(&schema.dtype)?,
            unit: schema.unit.clone(),
            description: schema.description.clone(),
            multiplier: schema.multiplier,
        });
    }
    let dto = PublicationDto {
        handle: publication.clone(),
        generation,
        topic: PublicationTopicDto {
            handle: topic.clone(),
            name: topic_name.to_owned(),
            row_count: store.rows,
            fields: field_dtos,
        },
    };
    Ok(PublicationEntry {
        source: receipt.source,
        generation,
        commit_nonce: source
            .entry
            .derived_provenance
            .as_ref()
            .ok_or_else(|| ApiError::internal("committed publication provenance is missing"))?
            .commit_nonce,
        publication,
        fields,
        dto,
    })
}

fn map_commit_error(error: DerivedCommitError) -> ApiError {
    match error {
        DerivedCommitError::SourceKeyInUse(_)
        | DerivedCommitError::PreviousSourceNotLive(_)
        | DerivedCommitError::PreviousSourceNotDerived(_)
        | DerivedCommitError::PreviousSourceMismatch(_)
        | DerivedCommitError::SourceNotLive(_)
        | DerivedCommitError::SourceNotDerived(_) => ApiError::conflict(error.to_string()),
        DerivedCommitError::SnapshotBuild(_) | DerivedCommitError::StorePublish(_) => {
            ApiError::internal(error.to_string()).with_completion(WireCompletion::Unknown)
        }
        _ => ApiError::invalid_input(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::ingest::ingest_channel;
    use delog_core::ingestor::{Ingestor, NullObserver};
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;

    use super::*;

    fn prepared_source() -> PreparedDerivedSource {
        let schema = Arc::new(
            TopicSchema::new(
                "error",
                [FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let columns: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![1.0]))];
        let chunk =
            Arc::new(Chunk::try_new(Int64Array::from(vec![1_i64]), columns, &schema).unwrap());
        let topic = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        PreparedDerivedSource::try_new([topic]).unwrap()
    }

    #[test]
    fn a_late_removal_receipt_eventually_invalidates_publication_handles() {
        let mut ingestor = Ingestor::new(NullObserver);
        let store = ingestor.store();
        let (sender, receiver) = ingest_channel();
        let registry = Arc::new(PublicationRegistry::with_receipt_timeout(
            Duration::from_millis(20),
        ));
        let owner = OwnerId::generate().unwrap();

        let publish_registry = Arc::clone(&registry);
        let publish_owner = owner.clone();
        let publish_sender = sender.clone();
        let publish_store = Arc::clone(&store);
        let publish = thread::spawn(move || {
            publish_registry.publish(
                &publish_owner,
                "diagnosis",
                "error",
                false,
                prepared_source(),
                &publish_sender,
                &publish_store,
                &CancellationToken::new(),
            )
        });
        ingestor.process(receiver.recv().unwrap());
        let publication = publish.join().unwrap().unwrap();
        let source = registry
            .resolve_source_for_owner(&owner, &publication.handle)
            .unwrap();

        let remove_registry = Arc::clone(&registry);
        let remove_owner = owner.clone();
        let remove_handle = publication.handle.clone();
        let remove_sender = sender.clone();
        let remove_store = Arc::clone(&store);
        let remove = thread::spawn(move || {
            remove_registry.remove(&remove_owner, &remove_handle, &remove_sender, &remove_store)
        });
        let error = remove.join().unwrap().unwrap_err();
        assert_eq!(error.completion(), Some(WireCompletion::Unknown));
        assert!(
            registry
                .resolve_source_for_owner(&owner, &publication.handle)
                .is_ok()
        );

        ingestor.process(receiver.recv().unwrap());
        let deadline = Instant::now() + Duration::from_secs(1);
        while registry
            .resolve_source_for_owner(&owner, &publication.handle)
            .is_ok()
        {
            assert!(Instant::now() < deadline, "late removal was not adopted");
            thread::yield_now();
        }
        assert!(!store.load().is_source_live(source));
        assert!(
            registry
                .resolve_field(&publication.topic.fields[0].handle)
                .is_err()
        );
    }
}
