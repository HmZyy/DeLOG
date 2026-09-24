use std::cell::RefCell;
use std::rc::Rc;

use delog_api::markers::PendingMarker;
use delog_api::operations::OperationSpec;
use delog_core::derived::PendingTopic;
use pyo3::prelude::*;

use crate::live::LiveTransformSpec;

pub type EmitBuffer = Rc<RefCell<Vec<PendingTopic>>>;
pub type OperationBuffer = Rc<RefCell<Vec<OperationSpec>>>;
pub type MarkerBuffer = Rc<RefCell<Vec<PendingMarker>>>;

pub struct PendingLiveTransform {
    pub spec: LiveTransformSpec,
    pub callable: Py<PyAny>,
    pub markers: MarkerBuffer,
}

pub type LiveTransformBuffer = Rc<RefCell<Vec<PendingLiveTransform>>>;

thread_local! {
    static MARKER_BUFFER_OVERRIDE: RefCell<Option<MarkerBuffer>> = const { RefCell::new(None) };
}

pub(crate) struct MarkerBufferOverride {
    previous: Option<MarkerBuffer>,
}

pub(crate) fn override_marker_buffer(markers: MarkerBuffer) -> MarkerBufferOverride {
    let previous = MARKER_BUFFER_OVERRIDE.with(|current| current.replace(Some(markers)));
    MarkerBufferOverride { previous }
}

impl Drop for MarkerBufferOverride {
    fn drop(&mut self) {
        MARKER_BUFFER_OVERRIDE.with(|current| {
            current.replace(self.previous.take());
        });
    }
}

pub(crate) fn active_marker_buffer() -> Option<MarkerBuffer> {
    MARKER_BUFFER_OVERRIDE.with(|current| current.borrow().clone())
}
