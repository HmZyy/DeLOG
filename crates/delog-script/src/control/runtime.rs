use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use super::{
    AnnotationRequest, ControlHost, ControlRequest, ControlResponse, MarkerRequest, TraceRequest,
    VehicleRequest, WorkspaceRequest,
};

pub type DeferredControlBuffer = Rc<RefCell<Vec<Vec<ControlRequest>>>>;

const BATCH_ERROR_PREFIX: &str = "batch: ";

thread_local! {
    static HOST: RefCell<Option<Arc<dyn ControlHost>>> = const { RefCell::new(None) };
    static ACTIVE_BATCH: RefCell<Option<Vec<ControlRequest>>> = const { RefCell::new(None) };
}

pub struct HostGuard {
    previous: Option<Arc<dyn ControlHost>>,
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        HOST.with(|host| {
            *host.borrow_mut() = self.previous.take();
        });
    }
}

pub fn install_host(host: Option<Arc<dyn ControlHost>>) -> HostGuard {
    let previous = HOST.with(|current| std::mem::replace(&mut *current.borrow_mut(), host));
    HostGuard { previous }
}

pub fn current_host() -> Option<Arc<dyn ControlHost>> {
    HOST.with(|host| host.borrow().clone())
}

fn missing_host() -> String {
    "the DeLOG control API is not available here; it works in the scripting console \
     and in named script runs, not inside live transforms, parsers, or flow scripts"
        .into()
}

/// Whether a request can be applied without needing a response payload or
/// creating a handle that subsequent Python statements depend on.
pub fn request_is_batchable(request: &ControlRequest) -> bool {
    match request {
        ControlRequest::Markers(request) => matches!(
            request,
            MarkerRequest::Append { .. }
                | MarkerRequest::RemoveOwned { .. }
                | MarkerRequest::Set { .. }
                | MarkerRequest::Remove(_)
        ),
        ControlRequest::Plots(_) => false,
        ControlRequest::Traces(request) => !matches!(request, TraceRequest::List { .. }),
        ControlRequest::Annotations(request) => {
            matches!(
                request,
                AnnotationRequest::Remove { .. } | AnnotationRequest::Set { .. }
            )
        }
        ControlRequest::Generation(_) => true,
        ControlRequest::Workspace(request) => matches!(
            request,
            WorkspaceRequest::Close { .. }
                | WorkspaceRequest::Equalize
                | WorkspaceRequest::ShowScene { .. }
        ),
        ControlRequest::Playback(_) => true,
        ControlRequest::Vehicles(request) => {
            matches!(
                request,
                VehicleRequest::Set { .. } | VehicleRequest::Remove(_)
            )
        }
        ControlRequest::VehicleProfiles(_)
        | ControlRequest::Layouts(_)
        | ControlRequest::Batch(_) => false,
    }
}

/// Stages a request when executing inside `with delog.batch():`.
/// Returns `false` when there is no active batch.
pub fn stage_batch_request(request: &ControlRequest) -> Result<bool, String> {
    ACTIVE_BATCH.with(|active| {
        let mut active = active.borrow_mut();
        let Some(requests) = active.as_mut() else {
            return Ok(false);
        };
        if !request_is_batchable(request) {
            return Err(format!(
                "{BATCH_ERROR_PREFIX}this control operation cannot be used inside delog.batch()"
            ));
        }
        requests.push(request.clone());
        Ok(true)
    })
}

pub fn control_call_error(error: String) -> PyErr {
    if let Some(message) = error.strip_prefix(BATCH_ERROR_PREFIX) {
        PyValueError::new_err(message.to_owned())
    } else {
        PyRuntimeError::new_err(error)
    }
}

#[pyclass(unsendable, name = "Batch", skip_from_py_object)]
pub struct BatchPy {
    deferred: DeferredControlBuffer,
    entered: bool,
}

impl BatchPy {
    pub fn new(deferred: DeferredControlBuffer) -> Self {
        Self {
            deferred,
            entered: false,
        }
    }
}

#[pymethods]
impl BatchPy {
    fn __enter__(&mut self) -> PyResult<()> {
        if current_host().is_none() {
            return Err(PyRuntimeError::new_err(missing_host()));
        }
        let nested = ACTIVE_BATCH.with(|active| {
            let mut active = active.borrow_mut();
            if active.is_some() {
                true
            } else {
                *active = Some(Vec::new());
                false
            }
        });
        if nested {
            return Err(PyValueError::new_err(
                "nested delog.batch() blocks are not supported",
            ));
        }
        self.entered = true;
        Ok(())
    }

    fn __exit__(
        &mut self,
        exc_type: &Bound<'_, PyAny>,
        _exc_value: &Bound<'_, PyAny>,
        _traceback: &Bound<'_, PyAny>,
    ) -> bool {
        let requests = ACTIVE_BATCH.with(|active| active.borrow_mut().take());
        self.entered = false;
        if exc_type.is_none()
            && let Some(requests) = requests
            && !requests.is_empty()
        {
            self.deferred.borrow_mut().push(requests);
        }
        false
    }
}

impl Drop for BatchPy {
    fn drop(&mut self) {
        if self.entered {
            ACTIVE_BATCH.with(|active| {
                active.borrow_mut().take();
            });
        }
    }
}

/// Round-trips a request to the host with the interpreter detached, so other
/// Python threads keep running and a pending interrupt can unblock the wait.
pub fn call_immediate_detached(
    py: Python<'_>,
    request: ControlRequest,
) -> Result<ControlResponse, String> {
    if stage_batch_request(&request)? {
        return Ok(ControlResponse::Unit);
    }
    let host = current_host().ok_or_else(missing_host)?;
    py.detach(move || host.call(request))
}

#[derive(Default)]
pub struct RecordingHost {
    seen: Mutex<Vec<ControlRequest>>,
    error: Option<String>,
}

impl RecordingHost {
    pub fn failing(error: &str) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            error: Some(error.into()),
        }
    }

    pub fn taken(&self) -> Vec<ControlRequest> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

impl ControlHost for RecordingHost {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse, String> {
        self.seen.lock().unwrap().push(request);
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(ControlResponse::Unit),
        }
    }
}
