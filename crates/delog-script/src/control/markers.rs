use std::rc::Rc;

use delog_api::color::{format_hex_color, parse_hex_color};
use delog_api::markers::PendingMarker;
use pyo3::exceptions::{PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyIterator, PyList};

use crate::staging::{MarkerBuffer, active_marker_buffer};

use super::{
    ControlRequest, MarkerFilter, MarkerInfo, MarkerOrigin, MarkerPatch, MarkerRequest,
    call_immediate_detached, control_call_error, stage_batch_request,
};

#[pyclass(unsendable, name = "MarkerCollection", skip_from_py_object)]
#[derive(Clone)]
pub struct MarkerCollectionPy {
    markers: MarkerBuffer,
    owner: String,
    generation: u64,
}

impl MarkerCollectionPy {
    pub(crate) fn new(markers: MarkerBuffer, owner: String, generation: u64) -> Self {
        Self {
            markers,
            owner,
            generation,
        }
    }

    fn append(&self, pending: Vec<PendingMarker>) -> PyResult<()> {
        let request = ControlRequest::Markers(MarkerRequest::Append {
            owner: self.owner.clone(),
            generation: self.generation,
            markers: pending.clone(),
        });
        if stage_batch_request(&request).map_err(control_call_error)? {
            return Ok(());
        }
        let markers = active_marker_buffer().unwrap_or_else(|| Rc::clone(&self.markers));
        markers.borrow_mut().extend(pending);
        Ok(())
    }
}

#[pymethods]
impl MarkerCollectionPy {
    #[pyo3(signature = (t_us, label, *, color=None, note=None))]
    fn add(
        &self,
        t_us: i64,
        label: String,
        color: Option<String>,
        note: Option<String>,
    ) -> PyResult<()> {
        let marker = PendingMarker::new(t_us, label, color.as_deref(), note)
            .map_err(crate::errors::value)?;
        self.append(vec![marker])
    }

    fn extend(&self, items: Vec<(i64, String)>) -> PyResult<()> {
        let pending = items
            .into_iter()
            .map(|(t_us, label)| PendingMarker::new(t_us, label, None, None))
            .collect::<delog_api::Result<Vec<_>>>()
            .map_err(crate::errors::value)?;
        self.append(pending)
    }

    fn list(&self, py: Python<'_>) -> PyResult<Vec<MarkerPy>> {
        Ok(request_markers(py)?
            .into_iter()
            .map(marker_from_info)
            .collect())
    }

    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        Ok(self.list(py)?.len())
    }

    fn __getitem__(&self, py: Python<'_>, index: usize) -> PyResult<MarkerPy> {
        self.list(py)?
            .into_iter()
            .nth(index)
            .ok_or_else(|| PyIndexError::new_err("marker index out of range"))
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyIterator>> {
        let out = PyList::empty(py);
        for marker in self.list(py)? {
            out.append(Bound::new(py, marker)?)?;
        }
        out.try_iter()
    }

    #[pyo3(signature = (target=None, *, owner=None, origin=None, label=None, after=None, before=None))]
    #[allow(clippy::too_many_arguments)]
    fn remove(
        &self,
        py: Python<'_>,
        target: Option<Bound<'_, PyAny>>,
        owner: Option<String>,
        origin: Option<&str>,
        label: Option<String>,
        after: Option<i64>,
        before: Option<i64>,
    ) -> PyResult<()> {
        let has_range = after.is_some() || before.is_some();
        let axes = usize::from(target.is_some())
            + usize::from(owner.is_some())
            + usize::from(origin.is_some())
            + usize::from(label.is_some())
            + usize::from(has_range);
        if axes != 1 {
            return Err(PyValueError::new_err(
                "remove() needs exactly one of an index/Marker handle, owner=, origin=, label=, or a time range",
            ));
        }

        let filter = if let Some(target) = target {
            if let Ok(index) = target.extract::<usize>() {
                MarkerFilter::Index(index)
            } else if let Ok(handle) = target.extract::<PyRef<'_, MarkerPy>>() {
                MarkerFilter::Id(handle.info.id)
            } else {
                return Err(PyValueError::new_err(
                    "remove() position must be an index or a Marker handle",
                ));
            }
        } else if let Some(owner) = owner {
            if owner.is_empty() {
                return Err(PyValueError::new_err("marker owner must not be empty"));
            }
            MarkerFilter::Owner(owner)
        } else if let Some(origin) = origin {
            MarkerFilter::Origin(parse_origin(origin)?)
        } else if let Some(label) = label {
            if label.is_empty() {
                return Err(PyValueError::new_err("marker label must not be empty"));
            }
            MarkerFilter::ScriptLabel(label)
        } else {
            if matches!((after, before), (Some(after), Some(before)) if after > before) {
                return Err(PyValueError::new_err(
                    "marker time range requires after <= before",
                ));
            }
            MarkerFilter::ScriptTimeRange { after, before }
        };
        request_unit(py, MarkerRequest::Remove(filter))
    }

    #[pyo3(signature = (*, manual=false))]
    fn clear(&self, py: Python<'_>, manual: bool) -> PyResult<()> {
        request_unit(
            py,
            MarkerRequest::Remove(if manual {
                MarkerFilter::All
            } else {
                MarkerFilter::ScriptAll
            }),
        )
    }
}

#[pyclass(unsendable, name = "Marker", skip_from_py_object)]
#[derive(Clone)]
pub struct MarkerPy {
    info: MarkerInfo,
}

#[pymethods]
impl MarkerPy {
    fn __repr__(&self) -> String {
        format!("<Marker {} id={}>", self.info.label, self.info.id)
    }

    #[getter]
    fn id(&self) -> u64 {
        self.info.id
    }

    #[getter]
    fn index(&self) -> usize {
        self.info.index
    }

    #[getter]
    fn origin(&self) -> &'static str {
        origin_name(self.info.origin)
    }

    #[getter]
    fn owner(&self) -> Option<String> {
        self.info.owner.clone()
    }

    #[getter]
    fn t_us(&self) -> i64 {
        self.info.t_us
    }

    #[setter]
    fn set_t_us(&mut self, py: Python<'_>, t_us: i64) -> PyResult<()> {
        self.submit_patch(
            py,
            MarkerPatch {
                t_us: Some(t_us),
                ..Default::default()
            },
        )?;
        self.info.t_us = t_us;
        Ok(())
    }

    #[getter]
    fn label(&self) -> String {
        self.info.label.clone()
    }

    #[setter]
    fn set_label(&mut self, py: Python<'_>, label: String) -> PyResult<()> {
        if label.is_empty() {
            return Err(PyValueError::new_err("marker label must not be empty"));
        }
        self.submit_patch(
            py,
            MarkerPatch {
                label: Some(label.clone()),
                ..Default::default()
            },
        )?;
        self.info.label = label;
        Ok(())
    }

    #[getter]
    fn color(&self) -> String {
        format_hex_color(self.info.color)
    }

    #[setter]
    fn set_color(&mut self, py: Python<'_>, color: &str) -> PyResult<()> {
        let color = parse_hex_color(color).map_err(crate::errors::value)?;
        self.submit_patch(
            py,
            MarkerPatch {
                color: Some(color),
                ..Default::default()
            },
        )?;
        self.info.color = color;
        Ok(())
    }

    #[getter]
    fn note(&self) -> String {
        self.info.note.clone()
    }

    #[setter]
    fn set_note(&mut self, py: Python<'_>, note: String) -> PyResult<()> {
        self.submit_patch(
            py,
            MarkerPatch {
                note: Some(note.clone()),
                ..Default::default()
            },
        )?;
        self.info.note = note;
        Ok(())
    }
}

impl MarkerPy {
    fn submit_patch(&self, py: Python<'_>, patch: MarkerPatch) -> PyResult<()> {
        request_unit(
            py,
            MarkerRequest::Set {
                id: self.info.id,
                patch,
            },
        )
    }
}

fn request_markers(py: Python<'_>) -> PyResult<Vec<MarkerInfo>> {
    let response = call_immediate_detached(py, ControlRequest::Markers(MarkerRequest::List))
        .map_err(control_call_error)?;
    response.into_markers().map_err(crate::errors::control)
}

fn request_unit(py: Python<'_>, request: MarkerRequest) -> PyResult<()> {
    let response = call_immediate_detached(py, ControlRequest::Markers(request))
        .map_err(control_call_error)?;
    response.into_unit().map_err(crate::errors::control)
}

fn marker_from_info(info: MarkerInfo) -> MarkerPy {
    MarkerPy { info }
}

fn parse_origin(origin: &str) -> PyResult<MarkerOrigin> {
    match origin {
        "manual" => Ok(MarkerOrigin::Manual),
        "script" => Ok(MarkerOrigin::Script),
        _ => Err(PyValueError::new_err(format!(
            "marker origin must be 'manual' or 'script', got {origin:?}"
        ))),
    }
}

fn origin_name(origin: MarkerOrigin) -> &'static str {
    match origin {
        MarkerOrigin::Manual => "manual",
        MarkerOrigin::Script => "script",
    }
}
