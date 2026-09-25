use std::sync::Arc;

use axum::extract::{Query, Request};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::Response;
use serde::Deserialize;
use tokio::sync::TryAcquireError;

use super::mutation::{Failure, idempotent, json_body, required_key};
use super::{RouterState, error_response_with_status};
use crate::idempotency::{RequestFingerprint, content_digest};
use crate::{ApiError, ClientSession, OpaqueId, WireCompletion, decode_staged_upload, stage_body};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationQuery {
    #[serde(default)]
    replace: bool,
}

pub(super) fn decode_topic_name(segment: &str) -> Result<String, ApiError> {
    let invalid =
        || ApiError::invalid_input("publication topic name is not valid percent-encoded UTF-8");
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3).ok_or_else(invalid)?;
            let hex = std::str::from_utf8(hex).map_err(|_| invalid())?;
            decoded.push(u8::from_str_radix(hex, 16).map_err(|_| invalid())?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let name = String::from_utf8(decoded).map_err(|_| invalid())?;
    if name.contains('/') || name == "." || name == ".." {
        return Err(ApiError::invalid_input(
            "publication topic name must not contain '/' or be '.' or '..'",
        ));
    }
    Ok(name)
}

fn target(uri: &Uri) -> &str {
    uri.path_and_query()
        .map_or(uri.path(), |target| target.as_str())
}

pub(super) async fn put(
    state: Arc<RouterState>,
    request: Request,
    caller: ClientSession,
    topic_name: &str,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if content_type != Some("application/vnd.apache.arrow.stream") {
        return Err(ApiError::invalid_input(
            "publication requires application/vnd.apache.arrow.stream",
        ));
    }
    let key = required_key(request.headers())?;
    let query = Query::<PublicationQuery>::try_from_uri(uri)
        .map_err(|_| ApiError::invalid_input("invalid publication query"))?
        .0;
    let permit = state.uploads.clone().try_acquire_owned().map_err(|error| {
        match error {
            TryAcquireError::NoPermits => ApiError::unavailable("upload capacity is saturated"),
            TryAcquireError::Closed => ApiError::unavailable("server is stopping"),
        }
        .with_completion(WireCompletion::NotStarted)
    })?;
    let work = state.client_work(&caller.client_id);
    let staged = match stage_body(
        request.into_body(),
        state.upload_limits,
        state.upload_staging.path(),
        work.clone(),
    )
    .await
    {
        Ok(staged) => staged,
        Err(error) if error.code() == "invalid_input" => {
            return Ok(error_response_with_status(
                error,
                StatusCode::UNPROCESSABLE_ENTITY,
            ));
        }
        Err(error) => return Err(error),
    };
    let fingerprint = RequestFingerprint::new("PUT", target(uri), staged.digest());
    let limits = state.upload_limits;
    let registry = Arc::clone(&state.publications);
    let ingest = state.ingest.clone();
    let store = Arc::clone(&state.store);
    let topic_name = topic_name.to_owned();
    let client = caller.client_id.clone();
    idempotent(&state, &client, &key, fingerprint, async move {
        let _permit = permit;
        let decoded = tokio::select! {
            biased;
            _ = work.cancelled() => return Err(Failure::from(revoked())),
            decoded = decode_staged_upload(staged, topic_name.clone(), limits) => decoded,
        };
        let source = decoded.map_err(|error| {
            if error.code() == "invalid_input" {
                Failure::with_status(error, StatusCode::UNPROCESSABLE_ENTITY)
            } else {
                Failure::from(error)
            }
        })?;
        let publication = tokio::task::spawn_blocking(move || {
            registry.publish(
                &caller.owner_id,
                &caller.owner_name,
                &topic_name,
                query.replace,
                source,
                &ingest,
                &store,
                &work,
            )
        })
        .await
        .map_err(|_| {
            ApiError::internal("publication commit task failed")
                .with_completion(WireCompletion::Unknown)
        })??;
        let status = if publication.generation == 1 {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        Ok((status, json_body(&publication)?))
    })
    .await
}

fn revoked() -> ApiError {
    ApiError::unavailable("the client was revoked before its publication committed")
        .with_completion(WireCompletion::NotStarted)
}

pub(super) async fn delete(
    state: Arc<RouterState>,
    headers: &HeaderMap,
    uri: &Uri,
    caller: ClientSession,
    publication: &str,
    body: &[u8],
) -> Result<Response, ApiError> {
    let key = required_key(headers)?;
    let fingerprint = RequestFingerprint::new("DELETE", target(uri), content_digest(body));
    let publication = OpaqueId::new(publication);
    let registry = Arc::clone(&state.publications);
    let ingest = state.ingest.clone();
    let store = Arc::clone(&state.store);
    let client = caller.client_id.clone();
    idempotent(&state, &client, &key, fingerprint, async move {
        tokio::task::spawn_blocking(move || {
            registry.remove(&caller.owner_id, &publication, &ingest, &store)
        })
        .await
        .map_err(|_| {
            ApiError::internal("publication removal task failed")
                .with_completion(WireCompletion::Unknown)
        })??;
        Ok((StatusCode::OK, json_body(&serde_json::json!({}))?))
    })
    .await
}
