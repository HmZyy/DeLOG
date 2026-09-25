use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::FutureExt;

use super::{RouterState, status_for, status_for_code};
use crate::idempotency::{IdempotencyAction, IdempotencyRegistry, RequestFingerprint};
use crate::{ApiError, ClientId, ErrorEnvelope, OpaqueId, RequestId, WireCompletion};

pub(super) const IDEMPOTENCY_KEY: &str = "idempotency-key";
pub(super) const REQUEST_ID: &str = "x-request-id";

pub(super) struct Failure {
    error: ApiError,
    status: StatusCode,
}

impl Failure {
    pub(super) fn with_status(error: ApiError, status: StatusCode) -> Self {
        Self { error, status }
    }

    pub(super) fn into_error(self) -> ApiError {
        self.error
    }
}

impl From<ApiError> for Failure {
    fn from(error: ApiError) -> Self {
        let status = status_for(&error);
        Self { error, status }
    }
}

pub(super) type Outcome = Result<(StatusCode, Bytes), Failure>;

pub(super) fn optional_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let mut values = headers.get_all(IDEMPOTENCY_KEY).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(ApiError::invalid_input(
            "exactly one Idempotency-Key header is allowed",
        ));
    }
    let key = value
        .to_str()
        .map_err(|_| ApiError::invalid_input("Idempotency-Key must be visible ASCII"))?;
    crate::idempotency::validate_idempotency_key(key)?;
    Ok(Some(key.to_owned()))
}

pub(super) fn required_key(headers: &HeaderMap) -> Result<String, ApiError> {
    optional_key(headers)?
        .ok_or_else(|| ApiError::invalid_input("an Idempotency-Key header is required"))
}

pub(super) fn stored_response(request: &RequestId, status: StatusCode, body: Bytes) -> Response {
    let mut response = (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(request.as_str()) {
        response.headers_mut().insert(REQUEST_ID, value);
    }
    response
}

fn envelope_bytes(envelope: &ErrorEnvelope) -> Bytes {
    Bytes::from(serde_json::to_vec(envelope).unwrap_or_else(|_| b"{}".to_vec()))
}

fn settle(
    registry: &IdempotencyRegistry,
    request: &RequestId,
    outcome: Outcome,
) -> (StatusCode, Bytes) {
    match outcome {
        Ok((status, body)) => {
            registry.complete(request, status, body.clone());
            (status, body)
        }
        Err(Failure { error, status }) => {
            let envelope = ErrorEnvelope::from_error(OpaqueId::new(request.as_str()), &error);
            let body = envelope_bytes(&envelope);
            match (error.completion(), error.code()) {
                (Some(WireCompletion::NotStarted), _) => registry.abandon(request),
                (Some(WireCompletion::Unknown), _) | (None, "unavailable") => {
                    registry.unknown(request, envelope)
                }
                _ => registry.complete(request, status, body.clone()),
            }
            (status, body)
        }
    }
}

async fn finish<F>(state: &Arc<RouterState>, request: RequestId, work: F) -> Response
where
    F: Future<Output = Outcome> + Send + 'static,
{
    let registry = Arc::clone(&state.idempotency);
    let settled = request.clone();
    let task = tokio::spawn(async move {
        let outcome = match AssertUnwindSafe(work).catch_unwind().await {
            Ok(outcome) => outcome,
            Err(_) => Err(Failure::from(
                ApiError::internal("request task failed").with_completion(WireCompletion::Unknown),
            )),
        };
        settle(&registry, &settled, outcome)
    });
    match task.await {
        Ok((status, body)) => stored_response(&request, status, body),
        Err(_) => {
            let error =
                ApiError::internal("request task failed").with_completion(WireCompletion::Unknown);
            let envelope = ErrorEnvelope::from_error(OpaqueId::new(request.as_str()), &error);
            state.idempotency.unknown(&request, envelope.clone());
            stored_response(
                &request,
                StatusCode::INTERNAL_SERVER_ERROR,
                envelope_bytes(&envelope),
            )
        }
    }
}

pub(super) async fn idempotent<F>(
    state: &Arc<RouterState>,
    client: &ClientId,
    key: &str,
    fingerprint: RequestFingerprint,
    work: F,
) -> Result<Response, ApiError>
where
    F: Future<Output = Outcome> + Send + 'static,
{
    Ok(
        match state
            .idempotency
            .begin_or_wait(client, key, fingerprint)
            .await?
        {
            IdempotencyAction::Execute(request) => finish(state, request, work).await,
            IdempotencyAction::Replay {
                request,
                status,
                body,
            } => stored_response(&request, status, body),
            IdempotencyAction::Unknown { request, error } => {
                let status = status_for_code(&error.code);
                stored_response(&request, status, envelope_bytes(&error))
            }
            IdempotencyAction::Wait(_) => {
                return Err(ApiError::internal("idempotency wait did not settle"));
            }
        },
    )
}

pub(super) fn json_body<T: serde::Serialize>(value: &T) -> Result<Bytes, ApiError> {
    serde_json::to_vec(value)
        .map(Bytes::from)
        .map_err(|_| ApiError::internal("could not encode the response"))
}
