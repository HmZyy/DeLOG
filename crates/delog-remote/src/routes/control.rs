use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use delog_api::control::{
    AuthorizedControlHost, ControlPrincipal, ControlRequest, ControlResponse, ResourceOwner,
};
use tokio::sync::{OwnedSemaphorePermit, TryAcquireError};
use tokio_util::sync::CancellationToken;

use super::RouterState;
use super::mutation::{
    Outcome, idempotent, json_body, optional_key, required_key, stored_response,
};
use crate::control::map::{ControlMapper, LiveFieldResolver};
use crate::idempotency::{RequestFingerprint, content_digest};
use crate::protocol::v1::control::ControlBatchDto;
use crate::{
    ApiError, ClientId, ClientSession, ControlCommandDto, ControlHandleRegistry, ControlResultDto,
    OwnerRemoval, RequestId, WireCompletion,
};

const EXTERNAL_GENERATION: u64 = 1;

enum ControlJob {
    State,
    Command(Box<ControlCommandDto>),
    Batch(Vec<ControlCommandDto>),
}

struct TimedHost<'a> {
    inner: &'a dyn AuthorizedControlHost,
    timeout: Duration,
    work: &'a CancellationToken,
}

impl AuthorizedControlHost for TimedHost<'_> {
    fn call_as(
        &self,
        principal: ControlPrincipal,
        request: ControlRequest,
    ) -> delog_api::Result<ControlResponse> {
        let work = self.work.clone();
        self.inner.call_as_cancellable(
            principal,
            request,
            self.timeout,
            Arc::new(move || work.is_cancelled()),
        )
    }
}

struct ControlPermit {
    _global: OwnedSemaphorePermit,
    state: Arc<RouterState>,
    client: ClientId,
}

impl Drop for ControlPermit {
    fn drop(&mut self) {
        let mut queued = self
            .state
            .queued_controls
            .lock()
            .expect("control queue poisoned");
        if let Some(count) = queued.get_mut(&self.client) {
            *count -= 1;
            if *count == 0 {
                queued.remove(&self.client);
            }
        }
    }
}

impl RouterState {
    fn owner_handles(&self, session: &ClientSession) -> Arc<Mutex<ControlHandleRegistry>> {
        Arc::clone(
            self.control_handles
                .lock()
                .expect("control handles poisoned")
                .entry(session.owner_id.clone())
                .or_default(),
        )
    }

    fn principal(&self, session: &ClientSession) -> ControlPrincipal {
        ControlPrincipal {
            owner: ResourceOwner {
                name: crate::external_owner_name(&session.owner_name),
                generation: EXTERNAL_GENERATION,
            },
            access: self.access_mode(),
        }
    }
}

fn acquire(state: &Arc<RouterState>, client: &ClientId) -> Result<ControlPermit, ApiError> {
    let global = state
        .controls
        .clone()
        .try_acquire_owned()
        .map_err(|error| {
            match error {
                TryAcquireError::NoPermits => ApiError::unavailable("the control queue is full"),
                TryAcquireError::Closed => ApiError::unavailable("server is stopping"),
            }
            .with_completion(WireCompletion::NotStarted)
        })?;
    let mut queued = state
        .queued_controls
        .lock()
        .expect("control queue poisoned");
    let count = queued.entry(client.clone()).or_default();
    if *count >= state.config.control.max_queued_per_client {
        return Err(
            ApiError::unavailable("this client has too many queued controls")
                .with_completion(WireCompletion::NotStarted),
        );
    }
    *count += 1;
    Ok(ControlPermit {
        _global: global,
        state: Arc::clone(state),
        client: client.clone(),
    })
}

fn execute(
    state: &RouterState,
    session: &ClientSession,
    job: ControlJob,
) -> Result<Bytes, ApiError> {
    if !state.client_is_active(&session.client_id) {
        return Err(ApiError::forbidden("client token is unknown or revoked")
            .with_completion(WireCompletion::NotStarted));
    }
    let work = state.client_work(&session.client_id);
    let host = TimedHost {
        inner: state.control.as_ref(),
        timeout: state.config.control.timeout,
        work: &work,
    };
    let handles = state.owner_handles(session);
    let mut handles = handles.lock().expect("control handles poisoned");
    let snapshot = state.store.load();
    let resolver = LiveFieldResolver::new(
        &state.leases,
        &state.publications,
        &snapshot,
        &session.client_id,
        &session.owner_id,
        Instant::now(),
    );
    let mut mapper = ControlMapper::new(&host, state.principal(session), &mut handles, &resolver);
    match job {
        ControlJob::State => json_body(&mapper.state()?),
        ControlJob::Command(command) if matches!(*command, ControlCommandDto::RemoveOwned) => {
            let ui = mapper.remove_owned();
            let publications = state.publications.remove_owner(
                &session.owner_id,
                &session.owner_name,
                &state.ingest,
                &state.store,
            );
            json_body(&removal_report(ui, publications)?)
        }
        ControlJob::Command(command) => json_body(&mapper.execute(*command)?),
        ControlJob::Batch(commands) => json_body(&mapper.execute_batch(commands)?),
    }
}

fn removal_report(
    ui: Result<usize, ApiError>,
    publications: OwnerRemoval,
) -> Result<ControlResultDto, ApiError> {
    let (ui_error, ui_resources) = match ui {
        Ok(count) => (None, Some(count)),
        Err(error) => (Some(error), None),
    };
    if ui_error.is_none() && publications.error.is_none() {
        return Ok(ControlResultDto::Removed {
            ui_resources: ui_resources.unwrap_or_default(),
            publications: publications.removed,
        });
    }
    let mut failed = Vec::new();
    if ui_error.is_some() {
        failed.push("ui");
    }
    if publications.error.is_some() {
        failed.push("publications");
    }
    let unknown = [&ui_error, &publications.error]
        .into_iter()
        .flatten()
        .any(|error| error.completion() == Some(WireCompletion::Unknown));
    let progressed = ui_resources.is_some() || publications.removed > 0;
    let Some(error) = ui_error.or(publications.error) else {
        return Err(ApiError::internal("owner cleanup failed without an error"));
    };
    let error = error.with_details(serde_json::json!({
        "ui_resources": ui_resources,
        "publications": publications.removed,
        "failed": failed,
    }));
    Err(if unknown {
        error.with_completion(WireCompletion::Unknown)
    } else if progressed {
        error.with_completion(WireCompletion::Committed)
    } else {
        error
    })
}

async fn run(state: Arc<RouterState>, session: ClientSession, job: ControlJob) -> Outcome {
    let permit = acquire(&state, &session.client_id)?;
    let body = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        execute(&state, &session, job)
    })
    .await
    .map_err(|_| {
        ApiError::internal("control task failed").with_completion(WireCompletion::Unknown)
    })??;
    Ok((StatusCode::OK, body))
}

fn is_query(command: &ControlCommandDto) -> bool {
    matches!(
        command,
        ControlCommandDto::LayoutList
            | ControlCommandDto::LayoutCurrent
            | ControlCommandDto::VehicleProfileList
            | ControlCommandDto::VehicleProfileLoad { .. }
    )
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|_| ApiError::invalid_input("invalid control JSON"))
}

async fn plain(
    state: &Arc<RouterState>,
    session: &ClientSession,
    job: ControlJob,
) -> Result<Response, ApiError> {
    let (status, body) = run(Arc::clone(state), session.clone(), job)
        .await
        .map_err(super::mutation::Failure::into_error)?;
    let request =
        RequestId::generate().map_err(|_| ApiError::internal("could not generate a request ID"))?;
    Ok(stored_response(&request, status, body))
}

pub(super) async fn route(
    state: &Arc<RouterState>,
    method: &Method,
    parts: &[&str],
    headers: &HeaderMap,
    session: &ClientSession,
    body: Bytes,
) -> Result<Option<Response>, ApiError> {
    let response = match (method.as_str(), parts) {
        ("GET", ["v1", "control", "state"]) => plain(state, session, ControlJob::State).await?,
        ("POST", ["v1", "control"]) => {
            let command: ControlCommandDto = parse(&body)?;
            let key = if is_query(&command) {
                optional_key(headers)?
            } else {
                Some(required_key(headers)?)
            };
            let Some(key) = key else {
                return plain(state, session, ControlJob::Command(Box::new(command)))
                    .await
                    .map(Some);
            };
            let fingerprint = RequestFingerprint::new("POST", "/v1/control", content_digest(&body));
            let work = run(
                Arc::clone(state),
                session.clone(),
                ControlJob::Command(Box::new(command)),
            );
            idempotent(state, &session.client_id, &key, fingerprint, work).await?
        }
        ("POST", ["v1", "control", "batch"]) => {
            let batch: ControlBatchDto = parse(&body)?;
            let key = required_key(headers)?;
            let fingerprint =
                RequestFingerprint::new("POST", "/v1/control/batch", content_digest(&body));
            let work = run(
                Arc::clone(state),
                session.clone(),
                ControlJob::Batch(batch.commands),
            );
            idempotent(state, &session.client_id, &key, fingerprint, work).await?
        }
        ("GET", ["v1", "requests", "by-key"]) => {
            let key = required_key(headers)?;
            let status = state.idempotency.get_by_key(&session.client_id, &key)?;
            axum::Json(status).into_response()
        }
        ("GET", ["v1", "requests", request]) => {
            let request: RequestId = serde_json::from_value(serde_json::json!(request))
                .map_err(|_| ApiError::invalid_input("invalid request ID"))?;
            let status = state.idempotency.get(&session.client_id, &request)?;
            axum::Json(status).into_response()
        }
        _ => return Ok(None),
    };
    Ok(Some(response))
}
