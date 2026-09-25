mod control;
mod mutation;
mod publication;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::extract::{Query, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio_util::sync::CancellationToken;

use crate::idempotency::IdempotencyRegistry;
use crate::server::{RemoteConfig, RemoteServices, ServerStatus, StartError};
use crate::{
    ApiError, ArrowStreamStats, ClientId, ClientRegistry, ControlHandleRegistry,
    DiscoveryDescriptor, DiscoveryError, DiscoveryRegistration, ErrorEnvelope, InstanceDto,
    LeaseId, LeaseManager, OpaqueId, OwnerId, ReadQuery, RegisterClientRequest,
    RegisterClientResponse, SecretToken, TopicBatchIter, UploadLimits, arrow_body_with_stats,
};
use delog_api::control::{AccessMode, AuthorizedControlHost};
use delog_core::ingest::IngestSender;
use delog_core::snapshot::DataStore;

const MAX_CONCURRENT_PREPARATIONS: usize = 2;

pub struct RouterState {
    pub(crate) config: RemoteConfig,
    pub(crate) endpoint: SocketAddr,
    bootstrap: SecretToken,
    instance_id: OpaqueId,
    instance: Mutex<InstanceDto>,
    discovery: Mutex<Option<DiscoveryRegistration>>,
    store: Arc<DataStore>,
    ingest: IngestSender,
    publications: Arc<crate::PublicationRegistry>,
    clients: Mutex<ClientRegistry>,
    leases: LeaseManager,
    downloads: Arc<Semaphore>,
    preparations: Arc<Semaphore>,
    uploads: Arc<Semaphore>,
    upload_staging: tempfile::TempDir,
    upload_limits: UploadLimits,
    pub(crate) cancellation: CancellationToken,
    shutdown_deadline: OnceLock<Instant>,
    producers: Mutex<Vec<Arc<ArrowStreamStats>>>,
    control: Arc<dyn AuthorizedControlHost>,
    control_handles: Mutex<HashMap<OwnerId, Arc<Mutex<ControlHandleRegistry>>>>,
    controls: Arc<Semaphore>,
    queued_controls: Mutex<HashMap<ClientId, usize>>,
    idempotency: Arc<IdempotencyRegistry>,
    access: Mutex<AccessMode>,
    client_work: Mutex<HashMap<ClientId, CancellationToken>>,
}

impl RouterState {
    pub fn new(
        config: RemoteConfig,
        services: RemoteServices,
        endpoint: SocketAddr,
    ) -> Result<Arc<Self>, StartError> {
        let RemoteServices {
            store,
            ingest,
            control,
            owners,
        } = services;
        let limits = config.control;
        let receipt_timeout =
            (config.request_timeout / 2).min(crate::publications::RECEIPT_TIMEOUT);
        if config.max_concurrent_downloads == 0
            || config.max_concurrent_downloads > Semaphore::MAX_PERMITS
            || limits.max_queued == 0
            || limits.max_queued > Semaphore::MAX_PERMITS
            || limits.max_queued_per_client == 0
            || limits.idempotency_ttl.is_zero()
            || limits.max_idempotency_records_per_client == 0
            || limits.timeout.is_zero()
            || config.uploads.max_concurrent == 0
            || config.uploads.max_concurrent > Semaphore::MAX_PERMITS
            || config.uploads.limits.max_bytes == 0
            || config.uploads.limits.max_rows == 0
            || config.uploads.limits.max_fields == 0
            || config.request_timeout.is_zero()
            || config.lease_idle_timeout.is_zero()
            || !endpoint.ip().is_loopback()
            || !endpoint.is_ipv4()
        {
            return Err(StartError::InvalidConfig);
        }
        let bootstrap = SecretToken::generate().map_err(|_| StartError::Random)?;
        let instance_id = OpaqueId::generate().map_err(|_| StartError::Random)?;
        let instance = InstanceDto::current(
            instance_id.clone(),
            config.label.clone(),
            session_description(config.loaded_file.as_deref()),
        );
        let upload_staging = tempfile::Builder::new()
            .prefix("delog-upload-")
            .tempdir()
            .map_err(StartError::Io)?;
        let publications = crate::PublicationRegistry::rebuild(
            store.load().as_ref(),
            owners.as_ref(),
            receipt_timeout,
        );
        Ok(Arc::new(Self {
            leases: LeaseManager::new(config.lease_idle_timeout),
            downloads: Arc::new(Semaphore::new(config.max_concurrent_downloads)),
            preparations: Arc::new(Semaphore::new(MAX_CONCURRENT_PREPARATIONS)),
            uploads: Arc::new(Semaphore::new(config.uploads.max_concurrent)),
            upload_staging,
            upload_limits: config.uploads.limits,
            config,
            endpoint,
            bootstrap,
            instance_id,
            instance: Mutex::new(instance),
            discovery: Mutex::new(None),
            store,
            ingest,
            publications: Arc::new(publications),
            clients: Mutex::new(ClientRegistry::with_owners(owners)),
            cancellation: CancellationToken::new(),
            shutdown_deadline: OnceLock::new(),
            producers: Mutex::new(Vec::new()),
            control,
            control_handles: Mutex::new(HashMap::new()),
            controls: Arc::new(Semaphore::new(limits.max_queued)),
            queued_controls: Mutex::new(HashMap::new()),
            idempotency: Arc::new(IdempotencyRegistry::new(
                limits.idempotency_ttl,
                limits.max_idempotency_records_per_client,
            )),
            access: Mutex::new(AccessMode::Safe),
            client_work: Mutex::new(HashMap::new()),
        }))
    }

    pub fn bootstrap_token(&self) -> &SecretToken {
        &self.bootstrap
    }

    pub(crate) fn instance_id(&self) -> &OpaqueId {
        &self.instance_id
    }

    pub(crate) fn instance(&self) -> InstanceDto {
        self.instance.lock().expect("instance poisoned").clone()
    }

    pub(crate) fn attach_discovery(&self, registration: DiscoveryRegistration) {
        *self.discovery.lock().expect("discovery poisoned") = Some(registration);
    }

    pub(crate) fn detach_discovery(&self) -> Option<DiscoveryRegistration> {
        self.discovery.lock().expect("discovery poisoned").take()
    }

    pub fn update_session(
        &self,
        label: &str,
        loaded_file: Option<&str>,
    ) -> Result<bool, DiscoveryError> {
        let description = session_description(loaded_file);
        let mut instance = self.instance.lock().expect("instance poisoned");
        if instance.label == label && instance.session_description == description {
            return Ok(false);
        }
        instance.label = label.to_owned();
        instance.session_description = description;
        let descriptor = DiscoveryDescriptor::new(&instance, self.endpoint, &self.bootstrap);
        drop(instance);
        let mut discovery = self.discovery.lock().expect("discovery poisoned");
        if let Some(registration) = discovery.as_mut() {
            registration.update(descriptor)?;
        }
        Ok(true)
    }

    pub fn status(&self) -> ServerStatus {
        let clients = self.clients.lock().expect("client registry poisoned");
        let queued_controls = self
            .queued_controls
            .lock()
            .expect("control queue poisoned")
            .values()
            .sum();
        ServerStatus {
            clients: clients.list(),
            leases: self.leases.list_active(),
            access: self.access_mode(),
            active_uploads: in_use(&self.uploads, self.config.uploads.max_concurrent),
            active_downloads: in_use(&self.downloads, self.config.max_concurrent_downloads),
            queued_controls,
        }
    }

    pub fn access_mode(&self) -> AccessMode {
        *self.access.lock().expect("access mode poisoned")
    }

    pub(crate) fn upload_staging_dir(&self) -> &std::path::Path {
        self.upload_staging.path()
    }

    pub fn set_access_mode(&self, access: AccessMode) {
        *self.access.lock().expect("access mode poisoned") = access;
    }

    pub(crate) fn client_work(&self, client: &ClientId) -> CancellationToken {
        let clients = self.clients.lock().expect("client registry poisoned");
        if !clients.contains(client) {
            let revoked = CancellationToken::new();
            revoked.cancel();
            return revoked;
        }
        self.client_work
            .lock()
            .expect("client work poisoned")
            .entry(client.clone())
            .or_insert_with(|| self.cancellation.child_token())
            .clone()
    }

    pub(crate) fn client_is_active(&self, client: &ClientId) -> bool {
        self.clients
            .lock()
            .expect("client registry poisoned")
            .contains(client)
    }

    fn release_client(&self, client: &ClientId) {
        self.leases.revoke_client(client);
        if let Some(work) = self
            .client_work
            .lock()
            .expect("client work poisoned")
            .remove(client)
        {
            work.cancel();
        }
    }

    pub fn revoke_client(&self, client: &ClientId) -> bool {
        let mut clients = self.clients.lock().expect("client registry poisoned");
        let removed = clients.revoke(client);
        self.release_client(client);
        removed
    }

    pub fn revoke_lease(&self, lease: &LeaseId) -> bool {
        let _clients = self.clients.lock().expect("client registry poisoned");
        if let Some(status) = self
            .leases
            .list_active()
            .into_iter()
            .find(|s| &s.id == lease)
        {
            self.leases.close(lease, &status.client).is_ok()
        } else {
            false
        }
    }

    pub fn reap(&self, now: Instant) {
        let mut clients = self.clients.lock().expect("client registry poisoned");
        for id in clients.reap(now, self.config.lease_idle_timeout) {
            self.release_client(&id);
        }
        self.leases.reap(now);
        self.idempotency.reap(now);
        self.producers
            .lock()
            .expect("producer registry poisoned")
            .retain(|stats| !stats.is_finished());
    }

    pub(crate) fn request_stop(&self, deadline: Instant) {
        let _ = self.shutdown_deadline.set(deadline);
        self.cancellation.cancel();
        self.downloads.close();
        self.preparations.close();
        self.uploads.close();
        self.controls.close();
    }

    pub(crate) fn shutdown_deadline(&self) -> Instant {
        *self
            .shutdown_deadline
            .get_or_init(|| Instant::now() + self.config.request_timeout)
    }

    pub(crate) fn stop(&self) {
        self.request_stop(Instant::now() + self.config.request_timeout);
        let mut clients = self.clients.lock().expect("client registry poisoned");
        for client in clients.list() {
            clients.revoke(&client.client_id);
            self.release_client(&client.client_id);
        }
    }

    pub(crate) fn producers_finished(&self) -> bool {
        self.producers
            .lock()
            .expect("producer registry poisoned")
            .iter()
            .all(|stats| stats.is_finished())
    }
}

pub fn router(state: Arc<RouterState>) -> Router {
    Router::new()
        .fallback(dispatch)
        .layer(middleware::from_fn_with_state(state.clone(), boundary))
        .with_state(state)
}

fn in_use(semaphore: &Semaphore, capacity: usize) -> usize {
    if semaphore.is_closed() {
        0
    } else {
        capacity.saturating_sub(semaphore.available_permits())
    }
}

fn session_description(loaded_file: Option<&str>) -> Option<String> {
    loaded_file
        .and_then(|file| std::path::Path::new(file).file_name())
        .map(|name| name.to_string_lossy().into_owned())
}

struct StreamActivity {
    state: Weak<RouterState>,
    lease: LeaseId,
    client: ClientId,
    progress: Arc<Mutex<Instant>>,
}

impl StreamActivity {
    fn record(&self) {
        let now = Instant::now();
        {
            let mut progress = self.progress.lock().expect("stream progress poisoned");
            *progress = (*progress).max(now);
        }
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut clients = state.clients.lock().expect("client registry poisoned");
        if clients.touch(&self.client, now) {
            state.leases.touch(&self.lease, &self.client, now);
        }
    }
}

async fn watch_stream(
    progress: Arc<Mutex<Instant>>,
    idle: Duration,
    body_finished: CancellationToken,
    cancellation: CancellationToken,
    permit: Option<OwnedSemaphorePermit>,
) {
    loop {
        let last = *progress.lock().expect("stream progress poisoned");
        let deadline = tokio::time::Instant::from_std(last + idle);
        tokio::select! {
            biased;
            _ = body_finished.cancelled() => break,
            _ = cancellation.cancelled() => break,
            _ = tokio::time::sleep_until(deadline) => {
                let last = *progress.lock().expect("stream progress poisoned");
                if Instant::now().saturating_duration_since(last) >= idle {
                    cancellation.cancel();
                    break;
                }
            }
        }
    }
    drop(permit);
}

fn status_for(error: &ApiError) -> StatusCode {
    status_for_code(error.code())
}

fn status_for_code(code: &str) -> StatusCode {
    match code {
        "invalid_input" => StatusCode::BAD_REQUEST,
        "forbidden" => StatusCode::FORBIDDEN,
        "not_found" => StatusCode::NOT_FOUND,
        "snapshot_expired" => StatusCode::GONE,
        "conflict" | "ambiguous" | "stale_handle" => StatusCode::CONFLICT,
        "unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn error_response(error: ApiError) -> Response {
    let status = status_for(&error);
    error_response_with_status(error, status)
}

fn error_response_with_status(error: ApiError, status: StatusCode) -> Response {
    let id = OpaqueId::generate().unwrap_or_else(|_| {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        OpaqueId::new(format!(
            "request-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    });
    (status, Json(ErrorEnvelope::from_error(id, &error))).into_response()
}

async fn boundary(State(state): State<Arc<RouterState>>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if headers.contains_key(header::ORIGIN)
        || headers.get_all(header::HOST).iter().count() != 1
        || headers.get(header::HOST).and_then(|h| h.to_str().ok())
            != Some(state.endpoint.to_string().as_str())
    {
        return error_response(ApiError::forbidden(
            "request is not from an allowed loopback client",
        ));
    }
    if state.cancellation.is_cancelled() {
        return error_response(ApiError::unavailable("server is stopping"));
    }
    match tokio::time::timeout(state.config.request_timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => error_response(ApiError::unavailable("request deadline exceeded")),
    }
}

fn bearer(request: &Request) -> Result<SecretToken, ApiError> {
    if request
        .headers()
        .get_all(header::AUTHORIZATION)
        .iter()
        .count()
        != 1
    {
        return Err(ApiError::forbidden("a bearer token is required"));
    }
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .and_then(|s| SecretToken::parse(s).ok())
        .ok_or_else(|| ApiError::forbidden("a valid bearer token is required"))
}

async fn dispatch(State(state): State<Arc<RouterState>>, request: Request) -> Response {
    match handle(state, request).await {
        Ok(response) => response,
        Err(error) => error_response(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataQuery {
    fields: Option<String>,
    start_ns: Option<i64>,
    end_ns: Option<i64>,
}

async fn handle(state: Arc<RouterState>, request: Request) -> Result<Response, ApiError> {
    let token = bearer(&request)?;
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_owned();
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    if method == Method::PUT
        && let ["v1", "publications", topic_name] = parts.as_slice()
    {
        let caller = state
            .clients
            .lock()
            .expect("client registry poisoned")
            .authenticate(&token, Instant::now())?;
        let topic_name = publication::decode_topic_name(topic_name)?;
        return publication::put(state, request, caller, &topic_name, &uri).await;
    }
    let (request_parts, body) = request.into_parts();
    let bytes = to_bytes(body, 64 * 1024)
        .await
        .map_err(|_| ApiError::invalid_input("request body exceeds 64 KiB or could not be read"))?;
    let request = Request::from_parts(request_parts, Body::empty());
    if method == Method::POST && parts == ["v1", "clients"] {
        if !state.bootstrap.matches(&token) {
            return Err(ApiError::forbidden("bootstrap authentication required"));
        }
        let input: RegisterClientRequest = serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::invalid_input("invalid registration JSON"))?;
        let mut clients = state.clients.lock().expect("client registry poisoned");
        if state.cancellation.is_cancelled() {
            return Err(ApiError::unavailable("server is stopping"));
        }
        let registered = clients.register(&state.bootstrap, &token, input)?;
        if let Some(replaced) = &registered.replaced {
            state.release_client(replaced);
        }
        return Ok(Json(RegisterClientResponse::from(&registered)).into_response());
    }
    if parts == ["v1", "instance"] {
        if !state.bootstrap.matches(&token) {
            state
                .clients
                .lock()
                .expect("client registry poisoned")
                .authenticate(&token, Instant::now())?;
        }
        if method != Method::GET {
            let mut response = error_response(ApiError::invalid_input("method is not allowed"));
            *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
            return Ok(response);
        }
        return Ok(Json(state.instance()).into_response());
    }
    let caller = state
        .clients
        .lock()
        .expect("client registry poisoned")
        .authenticate(&token, Instant::now())?;
    if method == Method::DELETE
        && let ["v1", "publications", publication] = parts.as_slice()
    {
        return publication::delete(state, request.headers(), &uri, caller, publication, &bytes)
            .await;
    }
    if let Some(response) = control::route(
        &state,
        &method,
        &parts,
        request.headers(),
        &caller,
        bytes.clone(),
    )
    .await?
    {
        return Ok(response);
    }
    if method == Method::POST && parts == ["v1", "snapshots"] {
        state
            .leases
            .ensure_capacity(&caller.client_id, Instant::now())?;
        let permit =
            state
                .preparations
                .clone()
                .try_acquire_owned()
                .map_err(|error| match error {
                    TryAcquireError::NoPermits => {
                        ApiError::unavailable("snapshot preparation capacity is saturated")
                    }
                    TryAcquireError::Closed => ApiError::unavailable("server is stopping"),
                })?;
        let snapshot = state.store.load();
        let prepared = tokio::task::spawn_blocking(move || {
            let prepared = LeaseManager::prepare(snapshot);
            drop(permit);
            prepared
        })
        .await
        .map_err(|_| ApiError::internal("snapshot preparation failed"))??;
        let mut clients = state.clients.lock().expect("client registry poisoned");
        if state.cancellation.is_cancelled() {
            return Err(ApiError::unavailable("server is stopping"));
        }
        let session = clients.authenticate(&token, Instant::now())?;
        let lease = state
            .leases
            .insert(prepared, session.client_id, Instant::now())?;
        return Ok(Json(json!({"lease_id": lease.id})).into_response());
    }
    let is_data = method == Method::GET
        && matches!(
            parts.as_slice(),
            ["v1", "snapshots", _, "topics", _, "data"]
        );
    let permit = if is_data {
        Some(
            tokio::time::timeout(
                state.config.request_timeout,
                state.downloads.clone().acquire_owned(),
            )
            .await
            .map_err(|_| ApiError::unavailable("download queue deadline exceeded"))?
            .map_err(|_| ApiError::unavailable("server is stopping"))?,
        )
    } else {
        None
    };
    let mut clients = state.clients.lock().expect("client registry poisoned");
    let session = clients.authenticate(&token, Instant::now())?;
    match (method.as_str(), parts.as_slice()) {
        ("DELETE", ["v1", "clients", client]) => {
            if *client != session.client_id.as_str() {
                return Err(ApiError::forbidden("only self-disconnect is permitted"));
            }
            clients.revoke(&session.client_id);
            state.release_client(&session.client_id);
            Ok(Json(json!({})).into_response())
        }
        ("DELETE", ["v1", "snapshots", lease]) => {
            let lease: LeaseId = serde_json::from_value(json!(lease))
                .map_err(|_| ApiError::invalid_input("invalid lease handle"))?;
            state.leases.close(&lease, &session.client_id)?;
            Ok(Json(json!({})).into_response())
        }
        ("GET", ["v1", "snapshots", lease, tail @ ..])
            if *tail == ["catalog"] || matches!(tail, ["topics", _, "data"]) =>
        {
            let lease: LeaseId = serde_json::from_value(json!(lease))
                .map_err(|_| ApiError::invalid_input("invalid lease handle"))?;
            let mut guard = state
                .leases
                .read(&lease, &session.client_id, Instant::now())
                .map_err(|error| match error.code() {
                    "not_found" => ApiError::not_found("snapshot lease not found"),
                    "snapshot_expired" => ApiError::snapshot_expired("snapshot lease expired"),
                    _ => error,
                })?;
            if *tail == ["catalog"] {
                return Ok(Json(guard.catalog().as_ref().clone()).into_response());
            }
            let query = Query::<DataQuery>::try_from_uri(request.uri())
                .map_err(|_| ApiError::invalid_input("invalid data query"))?
                .0;
            let topic = guard
                .topic_id(&OpaqueId::new(tail[1]))
                .ok_or_else(|| ApiError::not_found("topic handle not found"))?;
            let cancellation = guard.stream_cancellation();
            let iter = TopicBatchIter::new(
                guard,
                topic,
                ReadQuery {
                    fields: query
                        .fields
                        .map(|f| f.split(',').map(OpaqueId::new).collect()),
                    start_ns: query.start_ns,
                    end_ns: query.end_ns,
                },
            )
            .map_err(|error| match error.code() {
                "not_found" => ApiError::not_found("selected field not found"),
                "invalid_input" => ApiError::invalid_input("invalid field selection or time range"),
                _ => error,
            })?;
            let (body, stats) = arrow_body_with_stats(iter);
            let mut producers = state.producers.lock().expect("producer registry poisoned");
            producers.retain(|s| !s.is_finished());
            producers.push(stats);
            drop(producers);
            drop(clients);
            let activity = StreamActivity {
                state: Arc::downgrade(&state),
                lease,
                client: session.client_id,
                progress: Arc::new(Mutex::new(Instant::now())),
            };
            let body_finished = CancellationToken::new();
            let body_lifetime = body_finished.clone().drop_guard();
            tokio::spawn(watch_stream(
                Arc::clone(&activity.progress),
                state.config.lease_idle_timeout,
                body_finished,
                cancellation.clone(),
                permit,
            ));
            let chunks = stream::unfold(
                (
                    body.into_data_stream(),
                    body_lifetime,
                    cancellation,
                    activity,
                    false,
                ),
                |(mut stream, body_lifetime, cancellation, activity, ended)| async move {
                    if ended {
                        return None;
                    }
                    activity.record();
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => Some((Err(std::io::Error::other("snapshot stream cancelled")), (stream, body_lifetime, cancellation, activity, true))),
                        next = stream.next() => next.map(|chunk| (chunk.map_err(std::io::Error::other), (stream, body_lifetime, cancellation, activity, false))),
                    }
                },
            );
            Ok((
                [(header::CONTENT_TYPE, "application/vnd.apache.arrow.stream")],
                Body::from_stream(chunks),
            )
                .into_response())
        }
        (
            _,
            ["v1", "instance"]
            | ["v1", "clients"]
            | ["v1", "snapshots"]
            | ["v1", "clients", _]
            | ["v1", "snapshots", _]
            | ["v1", "snapshots", _, "catalog"]
            | ["v1", "snapshots", _, "topics", _, "data"]
            | ["v1", "publications", _]
            | ["v1", "control"]
            | ["v1", "control", "state" | "batch"]
            | ["v1", "requests", _],
        ) => {
            let mut response = error_response(ApiError::invalid_input("method is not allowed"));
            *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
            Ok(response)
        }
        _ => Err(ApiError::not_found("route not found")),
    }
}
