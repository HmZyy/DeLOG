use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use delog_core::identity::{SourceId, TopicId};
use delog_core::snapshot::StoreSnapshot;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::catalog::{self, FieldHandle, LeaseHandles};
use crate::handles::{ClientId, OpaqueId};
use crate::protocol::v1::catalog::CatalogDto;
use crate::protocol::v1::error::ApiError;

pub const MAX_LEASES_PER_CLIENT: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseId(OpaqueId);

impl LeaseId {
    fn generate() -> Result<Self, getrandom::Error> {
        Ok(Self(OpaqueId::generate()?))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for LeaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

pub struct PreparedLease {
    id: LeaseId,
    snapshot: Arc<StoreSnapshot>,
    catalog: Arc<CatalogDto>,
    handles: Arc<LeaseHandles>,
}

impl fmt::Debug for PreparedLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedLease")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

pub struct CreatedLease {
    pub id: LeaseId,
    pub client: ClientId,
}

#[derive(Debug)]
pub struct LeaseStatus {
    pub id: LeaseId,
    pub client: ClientId,
    pub active_readers: usize,
}

struct LeaseSlot {
    client: ClientId,
    snapshot: Arc<StoreSnapshot>,
    catalog: Arc<CatalogDto>,
    handles: Arc<LeaseHandles>,
    cancellation: CancellationToken,
    last_activity: Instant,
    active_readers: Arc<AtomicUsize>,
}

enum LeaseState {
    Active(LeaseSlot),
    Expired {
        client: ClientId,
        expired_at: Instant,
    },
}

pub struct LeaseManager {
    ttl: Duration,
    registry: Mutex<HashMap<LeaseId, LeaseState>>,
}

#[derive(Debug)]
pub struct LeaseReadGuard {
    snapshot: Arc<StoreSnapshot>,
    catalog: Arc<CatalogDto>,
    handles: Arc<LeaseHandles>,
    cancellation: CancellationToken,
    activity: Arc<AtomicUsize>,
}

impl LeaseManager {
    /// Resolve a catalog field only while its owning lease is live for this client.
    pub fn control_field(
        &self,
        handle: &OpaqueId,
        client: &ClientId,
        now: Instant,
    ) -> Option<FieldHandle> {
        let registry = self.registry.lock().expect("lease registry poisoned");
        registry.values().find_map(|state| match state {
            LeaseState::Active(slot)
                if &slot.client == client
                    && now.saturating_duration_since(slot.last_activity) < self.ttl =>
            {
                slot.handles.field(handle).cloned()
            }
            LeaseState::Active(_) | LeaseState::Expired { .. } => None,
        })
    }

    pub fn control_source(
        &self,
        handle: &OpaqueId,
        client: &ClientId,
        now: Instant,
    ) -> Option<SourceId> {
        let registry = self.registry.lock().expect("lease registry poisoned");
        registry.values().find_map(|state| match state {
            LeaseState::Active(slot)
                if &slot.client == client
                    && now.saturating_duration_since(slot.last_activity) < self.ttl =>
            {
                slot.handles.source_id(handle)
            }
            LeaseState::Active(_) | LeaseState::Expired { .. } => None,
        })
    }

    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            registry: Mutex::new(HashMap::new()),
        }
    }

    pub fn create(
        &self,
        client: ClientId,
        snapshot: Arc<StoreSnapshot>,
        now: Instant,
    ) -> Result<CreatedLease, ApiError> {
        let prepared = Self::prepare(snapshot)?;
        self.insert(prepared, client, now)
    }

    pub fn prepare(snapshot: Arc<StoreSnapshot>) -> Result<PreparedLease, ApiError> {
        let id = LeaseId::generate().map_err(random_generation_failed)?;
        let (catalog, handles) = catalog::build(&snapshot)?;
        Ok(PreparedLease {
            id,
            snapshot,
            catalog: Arc::new(catalog),
            handles: Arc::new(handles),
        })
    }

    pub fn ensure_capacity(&self, client: &ClientId, now: Instant) -> Result<(), ApiError> {
        let registry = self.registry.lock().expect("lease registry poisoned");
        self.check_capacity(&registry, client, now)
    }

    fn check_capacity(
        &self,
        registry: &HashMap<LeaseId, LeaseState>,
        client: &ClientId,
        now: Instant,
    ) -> Result<(), ApiError> {
        let active = registry
            .values()
            .filter(|state| match state {
                LeaseState::Active(slot) => {
                    &slot.client == client
                        && now.saturating_duration_since(slot.last_activity) < self.ttl
                }
                LeaseState::Expired { .. } => false,
            })
            .count();
        if active >= MAX_LEASES_PER_CLIENT {
            return Err(ApiError::unavailable(
                "the client holds the maximum number of active snapshot leases",
            ));
        }
        Ok(())
    }

    pub fn insert(
        &self,
        prepared: PreparedLease,
        client: ClientId,
        now: Instant,
    ) -> Result<CreatedLease, ApiError> {
        let PreparedLease {
            id,
            snapshot,
            catalog,
            handles,
        } = prepared;
        let slot = LeaseSlot {
            client: client.clone(),
            snapshot,
            catalog,
            handles,
            cancellation: CancellationToken::new(),
            last_activity: now,
            active_readers: Arc::new(AtomicUsize::new(0)),
        };

        let mut registry = self.registry.lock().expect("lease registry poisoned");
        if let Err(error) = self.check_capacity(&registry, &client, now) {
            drop(registry);
            drop(slot);
            return Err(error);
        }
        registry.insert(id.clone(), LeaseState::Active(slot));

        Ok(CreatedLease { id, client })
    }

    pub fn read(
        &self,
        id: &LeaseId,
        client: &ClientId,
        now: Instant,
    ) -> Result<LeaseReadGuard, ApiError> {
        let mut registry = self.registry.lock().expect("lease registry poisoned");
        let state = registry
            .get_mut(id)
            .ok_or_else(|| ApiError::not_found(format!("lease {id} not found")))?;

        match state {
            LeaseState::Expired { client: owner, .. } => {
                if owner != client {
                    return Err(ApiError::forbidden("lease belongs to a different client"));
                }
                Err(ApiError::snapshot_expired(format!("lease {id} expired")))
            }
            LeaseState::Active(slot) => {
                if &slot.client != client {
                    return Err(ApiError::forbidden("lease belongs to a different client"));
                }
                if now.saturating_duration_since(slot.last_activity) >= self.ttl {
                    slot.cancellation.cancel();
                    let expired = std::mem::replace(
                        state,
                        LeaseState::Expired {
                            client: client.clone(),
                            expired_at: now,
                        },
                    );
                    drop(registry);
                    drop(expired);
                    return Err(ApiError::snapshot_expired(format!("lease {id} expired")));
                }
                slot.last_activity = now;
                slot.active_readers.fetch_add(1, Ordering::SeqCst);
                Ok(LeaseReadGuard {
                    snapshot: Arc::clone(&slot.snapshot),
                    catalog: Arc::clone(&slot.catalog),
                    handles: Arc::clone(&slot.handles),
                    cancellation: slot.cancellation.clone(),
                    activity: Arc::clone(&slot.active_readers),
                })
            }
        }
    }

    pub fn touch(&self, id: &LeaseId, client: &ClientId, now: Instant) -> bool {
        let mut registry = self.registry.lock().expect("lease registry poisoned");
        match registry.get_mut(id) {
            Some(LeaseState::Active(slot))
                if &slot.client == client
                    && now.saturating_duration_since(slot.last_activity) < self.ttl =>
            {
                slot.last_activity = slot.last_activity.max(now);
                true
            }
            _ => false,
        }
    }

    pub fn close(&self, id: &LeaseId, client: &ClientId) -> Result<(), ApiError> {
        let mut registry = self.registry.lock().expect("lease registry poisoned");

        let Some(state) = registry.get_mut(id) else {
            return Ok(());
        };
        let owner = match state {
            LeaseState::Active(slot) => &slot.client,
            LeaseState::Expired { client, .. } => client,
        };
        if owner != client {
            return Err(ApiError::forbidden("lease belongs to a different client"));
        }

        let released = expire(state, Instant::now());
        drop(registry);
        drop(released);
        Ok(())
    }

    pub fn revoke_client(&self, client: &ClientId) {
        let now = Instant::now();
        let mut registry = self.registry.lock().expect("lease registry poisoned");

        let released: Vec<LeaseState> = registry
            .values_mut()
            .filter(|state| matches!(state, LeaseState::Active(slot) if &slot.client == client))
            .filter_map(|state| expire(state, now))
            .collect();
        drop(registry);
        drop(released);
    }

    pub fn reap(&self, now: Instant) -> usize {
        let mut registry = self.registry.lock().expect("lease registry poisoned");
        let mut expired = 0usize;

        for state in registry.values_mut() {
            if let LeaseState::Active(slot) = state {
                let idle = now
                    .checked_duration_since(slot.last_activity)
                    .unwrap_or_default();
                if idle >= self.ttl {
                    slot.cancellation.cancel();
                    let client = slot.client.clone();
                    *state = LeaseState::Expired {
                        client,
                        expired_at: now,
                    };
                    expired += 1;
                }
            }
        }

        registry.retain(|_, state| match state {
            LeaseState::Active(_) => true,
            LeaseState::Expired { expired_at, .. } => {
                now.checked_duration_since(*expired_at).unwrap_or_default() < self.ttl
            }
        });

        expired
    }

    pub fn list_active(&self) -> Vec<LeaseStatus> {
        let registry = self.registry.lock().expect("lease registry poisoned");
        registry
            .iter()
            .filter_map(|(id, state)| match state {
                LeaseState::Active(slot) => Some(LeaseStatus {
                    id: id.clone(),
                    client: slot.client.clone(),
                    active_readers: slot.active_readers.load(Ordering::SeqCst),
                }),
                LeaseState::Expired { .. } => None,
            })
            .collect()
    }
}

impl LeaseReadGuard {
    pub fn snapshot(&self) -> &Arc<StoreSnapshot> {
        &self.snapshot
    }

    pub fn catalog(&self) -> &Arc<CatalogDto> {
        &self.catalog
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub fn stream_cancellation(&mut self) -> CancellationToken {
        self.cancellation = self.cancellation.child_token();
        self.cancellation.clone()
    }

    pub fn topic_id(&self, handle: &OpaqueId) -> Option<TopicId> {
        self.handles.topic_id(handle)
    }

    pub fn field(&self, handle: &OpaqueId) -> Option<&FieldHandle> {
        self.handles.field(handle)
    }
}

impl Drop for LeaseReadGuard {
    fn drop(&mut self) {
        self.activity.fetch_sub(1, Ordering::SeqCst);
    }
}

fn expire(state: &mut LeaseState, now: Instant) -> Option<LeaseState> {
    let LeaseState::Active(slot) = state else {
        return None;
    };
    slot.cancellation.cancel();
    let client = slot.client.clone();
    Some(std::mem::replace(
        state,
        LeaseState::Expired {
            client,
            expired_at: now,
        },
    ))
}

fn random_generation_failed(error: getrandom::Error) -> ApiError {
    ApiError::internal(format!("failed to generate random bytes: {error}"))
}
