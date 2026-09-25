#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidInput,
    NotFound,
    Ambiguous,
    StaleHandle,
    Forbidden,
    Conflict,
    Unavailable,
    Protocol,
    Execution,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationCompletion {
    NotStarted,
    Committed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    kind: ErrorKind,
    message: String,
    completion: Option<MutationCompletion>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidInput, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    pub fn ambiguous(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Ambiguous, message)
    }

    pub fn stale_handle(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::StaleHandle, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Forbidden, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Protocol, message)
    }

    pub fn execution(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Execution, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn with_context(mut self, context: impl AsRef<str>) -> Self {
        self.message = format!("{}: {}", context.as_ref(), self.message);
        self
    }

    pub fn with_completion(mut self, completion: MutationCompletion) -> Self {
        self.completion = Some(completion);
        self
    }

    pub fn completion(&self) -> Option<MutationCompletion> {
        self.completion
    }

    pub fn into_message(self) -> String {
        self.message
    }

    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            completion: None,
        }
    }
}
