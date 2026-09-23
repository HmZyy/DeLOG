use delog_api::{Error, ErrorKind};
use pyo3::PyErr;

pub(crate) fn lookup(error: Error) -> PyErr {
    match error.kind() {
        ErrorKind::NotFound => pyo3::exceptions::PyKeyError::new_err(error.into_message()),
        ErrorKind::InvalidInput | ErrorKind::Ambiguous => {
            pyo3::exceptions::PyValueError::new_err(error.into_message())
        }
        ErrorKind::Unavailable | ErrorKind::Protocol | ErrorKind::Execution => {
            pyo3::exceptions::PyRuntimeError::new_err(error.into_message())
        }
    }
}

pub(crate) fn value(error: Error) -> PyErr {
    match error.kind() {
        ErrorKind::InvalidInput | ErrorKind::NotFound | ErrorKind::Ambiguous => {
            pyo3::exceptions::PyValueError::new_err(error.into_message())
        }
        ErrorKind::Unavailable | ErrorKind::Protocol | ErrorKind::Execution => {
            pyo3::exceptions::PyRuntimeError::new_err(error.into_message())
        }
    }
}

#[allow(dead_code)]
pub(crate) fn control(error: Error) -> PyErr {
    match error.kind() {
        ErrorKind::InvalidInput | ErrorKind::Ambiguous => {
            pyo3::exceptions::PyValueError::new_err(error.into_message())
        }
        ErrorKind::NotFound
        | ErrorKind::Unavailable
        | ErrorKind::Protocol
        | ErrorKind::Execution => pyo3::exceptions::PyRuntimeError::new_err(error.into_message()),
    }
}
