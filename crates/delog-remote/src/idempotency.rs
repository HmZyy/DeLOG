use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::handles::{ClientId, RequestId};
use crate::protocol::v1::error::{ApiError, ErrorEnvelope, WireCompletion};
use crate::protocol::v1::request::{RequestStateDto, RequestStatusDto};

pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestFingerprint([u8; 32]);

impl RequestFingerprint {
    pub fn new(method: &str, target: &str, content: [u8; 32]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(method.as_bytes());
        hasher.update([0]);
        hasher.update(target.as_bytes());
        hasher.update([0]);
        hasher.update(content);
        Self(hasher.finalize().into())
    }
}

pub fn content_digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub enum RequestState {
    InFlight,
    Completed { status: StatusCode, body: Bytes },
    Unknown { error: ErrorEnvelope },
}

pub enum IdempotencyAction {
    Execute(RequestId),
    Replay {
        request: RequestId,
        status: StatusCode,
        body: Bytes,
    },
    Wait(Arc<Notify>),
    Unknown {
        request: RequestId,
        error: ErrorEnvelope,
    },
}

struct Record {
    client: ClientId,
    key: String,
    fingerprint: RequestFingerprint,
    state: RequestState,
    created: Instant,
    notify: Arc<Notify>,
}

#[derive(Default)]
struct Records {
    by_id: HashMap<RequestId, Record>,
    by_key: HashMap<(ClientId, String), RequestId>,
    order: HashMap<ClientId, VecDeque<RequestId>>,
}

impl Records {
    fn remove(&mut self, request: &RequestId) -> Option<Record> {
        let record = self.by_id.remove(request)?;
        self.by_key
            .remove(&(record.client.clone(), record.key.clone()));
        if let Some(order) = self.order.get_mut(&record.client) {
            order.retain(|id| id != request);
            if order.is_empty() {
                self.order.remove(&record.client);
            }
        }
        record.notify.notify_waiters();
        Some(record)
    }

    fn purge(&mut self, client: &ClientId, now: Instant, ttl: Duration) {
        let expired: Vec<RequestId> = self
            .order
            .get(client)
            .into_iter()
            .flatten()
            .filter(|id| {
                self.by_id
                    .get(*id)
                    .is_some_and(|record| record.expired(now, ttl))
            })
            .cloned()
            .collect();
        for id in expired {
            self.remove(&id);
        }
    }
}

impl Record {
    fn expired(&self, now: Instant, ttl: Duration) -> bool {
        !matches!(self.state, RequestState::InFlight)
            && now.saturating_duration_since(self.created) >= ttl
    }

    fn status(&self, request: &RequestId) -> RequestStatusDto {
        let (state, status, response, error) = match &self.state {
            RequestState::InFlight => (RequestStateDto::InFlight, None, None, None),
            RequestState::Completed { status, body } => (
                RequestStateDto::Completed,
                Some(status.as_u16()),
                serde_json::from_slice(body).ok(),
                None,
            ),
            RequestState::Unknown { error } => {
                (RequestStateDto::Unknown, None, None, Some(error.clone()))
            }
        };
        RequestStatusDto {
            request_id: request.clone(),
            state,
            status,
            response,
            error,
        }
    }
}

pub struct IdempotencyRegistry {
    ttl: Duration,
    max_records_per_client: usize,
    records: Mutex<Records>,
}

pub fn validate_idempotency_key(key: &str) -> Result<(), ApiError> {
    if key.is_empty()
        || key.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || !key.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(ApiError::invalid_input(
            "Idempotency-Key must be 1 to 128 visible ASCII characters",
        ));
    }
    Ok(())
}

impl IdempotencyRegistry {
    pub fn new(ttl: Duration, max_records_per_client: usize) -> Self {
        Self {
            ttl,
            max_records_per_client,
            records: Mutex::new(Records::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Records> {
        self.records.lock().expect("idempotency registry poisoned")
    }

    pub fn begin(
        &self,
        client: &ClientId,
        key: &str,
        fingerprint: RequestFingerprint,
        now: Instant,
    ) -> Result<IdempotencyAction, ApiError> {
        validate_idempotency_key(key)?;
        let mut records = self.lock();
        records.purge(client, now, self.ttl);
        if let Some(id) = records.by_key.get(&(client.clone(), key.to_owned())) {
            let record = &records.by_id[id];
            if record.fingerprint != fingerprint {
                return Err(ApiError::conflict(
                    "Idempotency-Key was already used for a different request",
                ));
            }
            return Ok(match &record.state {
                RequestState::InFlight => IdempotencyAction::Wait(Arc::clone(&record.notify)),
                RequestState::Completed { status, body } => IdempotencyAction::Replay {
                    request: id.clone(),
                    status: *status,
                    body: body.clone(),
                },
                RequestState::Unknown { error } => IdempotencyAction::Unknown {
                    request: id.clone(),
                    error: error.clone(),
                },
            });
        }
        let count = records.order.get(client).map_or(0, VecDeque::len);
        if count >= self.max_records_per_client {
            let evictable = records.order.get(client).and_then(|order| {
                order
                    .iter()
                    .find(|id| {
                        records.by_id.get(*id).is_some_and(|record| {
                            matches!(record.state, RequestState::Completed { .. })
                        })
                    })
                    .cloned()
            });
            let Some(evicted) = evictable else {
                return Err(ApiError::unavailable(
                    "too many unsettled requests are recorded for this client",
                )
                .with_completion(WireCompletion::NotStarted));
            };
            records.remove(&evicted);
        }
        let request = RequestId::generate()
            .map_err(|_| ApiError::internal("could not generate a request ID"))?;
        records.by_id.insert(
            request.clone(),
            Record {
                client: client.clone(),
                key: key.to_owned(),
                fingerprint,
                state: RequestState::InFlight,
                created: now,
                notify: Arc::new(Notify::new()),
            },
        );
        records
            .by_key
            .insert((client.clone(), key.to_owned()), request.clone());
        records
            .order
            .entry(client.clone())
            .or_default()
            .push_back(request.clone());
        Ok(IdempotencyAction::Execute(request))
    }

    pub async fn begin_or_wait(
        &self,
        client: &ClientId,
        key: &str,
        fingerprint: RequestFingerprint,
    ) -> Result<IdempotencyAction, ApiError> {
        loop {
            let notify = match self.begin(client, key, fingerprint, Instant::now())? {
                IdempotencyAction::Wait(notify) => notify,
                action => return Ok(action),
            };
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.begin(client, key, fingerprint, Instant::now())? {
                IdempotencyAction::Wait(_) => notified.await,
                action => return Ok(action),
            }
        }
    }

    fn settle(&self, request: &RequestId, state: RequestState) {
        let mut records = self.lock();
        if let Some(record) = records.by_id.get_mut(request) {
            record.state = state;
            record.notify.notify_waiters();
        }
    }

    pub fn complete(&self, request: &RequestId, status: StatusCode, body: Bytes) {
        self.settle(request, RequestState::Completed { status, body });
    }

    pub fn unknown(&self, request: &RequestId, error: ErrorEnvelope) {
        self.settle(request, RequestState::Unknown { error });
    }

    pub fn abandon(&self, request: &RequestId) {
        self.lock().remove(request);
    }

    pub fn get(
        &self,
        client: &ClientId,
        request: &RequestId,
    ) -> Result<RequestStatusDto, ApiError> {
        let records = self.lock();
        records
            .by_id
            .get(request)
            .filter(|record| &record.client == client && !record.expired(Instant::now(), self.ttl))
            .map(|record| record.status(request))
            .ok_or_else(|| ApiError::not_found("request not found"))
    }

    pub fn get_by_key(&self, client: &ClientId, key: &str) -> Result<RequestStatusDto, ApiError> {
        validate_idempotency_key(key)?;
        let request = self
            .lock()
            .by_key
            .get(&(client.clone(), key.to_owned()))
            .cloned()
            .ok_or_else(|| ApiError::not_found("request not found"))?;
        self.get(client, &request)
    }

    pub fn reap(&self, now: Instant) {
        let mut records = self.lock();
        let clients: Vec<ClientId> = records.order.keys().cloned().collect();
        for client in clients {
            records.purge(&client, now, self.ttl);
        }
    }
}
