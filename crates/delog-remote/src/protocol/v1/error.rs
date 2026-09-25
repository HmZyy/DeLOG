use serde::{Deserialize, Serialize};

use crate::handles::OpaqueId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireCode {
    InvalidInput,
    NotFound,
    Ambiguous,
    StaleHandle,
    Forbidden,
    SnapshotExpired,
    Conflict,
    Unavailable,
    Internal,
}

impl WireCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
            Self::StaleHandle => "stale_handle",
            Self::Forbidden => "forbidden",
            Self::SnapshotExpired => "snapshot_expired",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }

    fn is_retryable(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireCompletion {
    NotStarted,
    Committed,
    Unknown,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ApiError {
    code: WireCode,
    message: String,
    details: Option<serde_json::Value>,
    completion: Option<WireCompletion>,
}

impl ApiError {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(WireCode::InvalidInput, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(WireCode::NotFound, message)
    }

    pub fn ambiguous(message: impl Into<String>) -> Self {
        Self::new(WireCode::Ambiguous, message)
    }

    pub fn stale_handle(message: impl Into<String>) -> Self {
        Self::new(WireCode::StaleHandle, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(WireCode::Forbidden, message)
    }

    pub fn snapshot_expired(message: impl Into<String>) -> Self {
        Self::new(WireCode::SnapshotExpired, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(WireCode::Conflict, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(WireCode::Unavailable, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(WireCode::Internal, message)
    }

    pub fn code(&self) -> &'static str {
        self.code.as_str()
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn retryable(&self) -> bool {
        self.code.is_retryable()
    }

    pub fn details(&self) -> Option<&serde_json::Value> {
        self.details.as_ref()
    }

    pub fn completion(&self) -> Option<WireCompletion> {
        self.completion
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn with_completion(mut self, completion: WireCompletion) -> Self {
        self.completion = Some(completion);
        self
    }

    fn new(code: WireCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
            completion: None,
        }
    }
}

impl From<delog_api::Error> for ApiError {
    fn from(error: delog_api::Error) -> Self {
        let kind = error.kind();
        let completion = error.completion();
        let message = error.into_message();

        let code = match kind {
            delog_api::ErrorKind::InvalidInput => WireCode::InvalidInput,
            delog_api::ErrorKind::NotFound => WireCode::NotFound,
            delog_api::ErrorKind::Ambiguous => WireCode::Ambiguous,
            delog_api::ErrorKind::StaleHandle => WireCode::StaleHandle,
            delog_api::ErrorKind::Forbidden => WireCode::Forbidden,
            delog_api::ErrorKind::Conflict => WireCode::Conflict,
            delog_api::ErrorKind::Unavailable => WireCode::Unavailable,
            delog_api::ErrorKind::Protocol
            | delog_api::ErrorKind::Execution
            | delog_api::ErrorKind::Internal => WireCode::Internal,
        };

        let completion = completion.map(|completion| match completion {
            delog_api::MutationCompletion::NotStarted => WireCompletion::NotStarted,
            delog_api::MutationCompletion::Committed => WireCompletion::Committed,
            delog_api::MutationCompletion::Unknown => WireCompletion::Unknown,
        });

        Self {
            code,
            message,
            details: None,
            completion,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub request_id: OpaqueId,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub details: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub completion: Option<WireCompletion>,
}

impl ErrorEnvelope {
    pub fn from_error(request_id: OpaqueId, error: &ApiError) -> Self {
        Self {
            request_id,
            code: error.code().to_string(),
            message: error.message().to_string(),
            retryable: error.retryable(),
            details: error.details().cloned(),
            completion: error.completion(),
        }
    }
}
