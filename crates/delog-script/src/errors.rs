use delog_api::{Error, ErrorKind};
use pyo3::PyErr;

pub(crate) fn lookup(error: Error) -> PyErr {
    match error.kind() {
        ErrorKind::NotFound | ErrorKind::StaleHandle => {
            pyo3::exceptions::PyKeyError::new_err(error.into_message())
        }
        ErrorKind::InvalidInput
        | ErrorKind::Ambiguous
        | ErrorKind::Forbidden
        | ErrorKind::Conflict => pyo3::exceptions::PyValueError::new_err(error.into_message()),
        ErrorKind::Unavailable
        | ErrorKind::Protocol
        | ErrorKind::Execution
        | ErrorKind::Internal => pyo3::exceptions::PyRuntimeError::new_err(error.into_message()),
    }
}

pub(crate) fn value(error: Error) -> PyErr {
    match error.kind() {
        ErrorKind::InvalidInput
        | ErrorKind::NotFound
        | ErrorKind::Ambiguous
        | ErrorKind::StaleHandle
        | ErrorKind::Forbidden
        | ErrorKind::Conflict => pyo3::exceptions::PyValueError::new_err(error.into_message()),
        ErrorKind::Unavailable
        | ErrorKind::Protocol
        | ErrorKind::Execution
        | ErrorKind::Internal => pyo3::exceptions::PyRuntimeError::new_err(error.into_message()),
    }
}

pub(crate) fn control(error: Error) -> PyErr {
    match error.kind() {
        ErrorKind::InvalidInput | ErrorKind::Ambiguous => {
            pyo3::exceptions::PyValueError::new_err(error.into_message())
        }
        ErrorKind::NotFound
        | ErrorKind::StaleHandle
        | ErrorKind::Forbidden
        | ErrorKind::Conflict
        | ErrorKind::Unavailable
        | ErrorKind::Protocol
        | ErrorKind::Execution
        | ErrorKind::Internal => pyo3::exceptions::PyRuntimeError::new_err(error.into_message()),
    }
}
