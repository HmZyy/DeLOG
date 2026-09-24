use delog_api::catalog::resolve_field_path;
use delog_api::color::{format_hex_color, parse_hex_color};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;
use pyo3::prelude::*;
use pyo3::types::{PyIterator, PyList};

use super::{
    ControlRequest, PlotContext, TraceInfo, TraceMode, TraceRequest, call_immediate_detached,
    control_call_error,
};

#[pyclass(unsendable, name = "TraceCollection", skip_from_py_object)]
#[derive(Clone)]
pub struct TraceCollectionPy {
    window: u64,
    tile: u64,
    context: PlotContext,
}

impl TraceCollectionPy {
    pub(crate) fn new(window: u64, tile: u64, context: PlotContext) -> Self {
        Self {
            window,
            tile,
            context,
        }
    }
}

#[pymethods]
impl TraceCollectionPy {
    #[pyo3(signature = (field, *, color=None, width_px=None, mode="line"))]
    fn add(
        &self,
        py: Python<'_>,
        field: Bound<'_, PyAny>,
        color: Option<String>,
        width_px: Option<f32>,
        mode: &str,
    ) -> PyResult<()> {
        let (field_id, field) = resolve_field(&self.context.snapshot, &field)?;
        let mode = parse_trace_mode(mode)?;
        let color = color
            .as_deref()
            .map(parse_hex_color)
            .transpose()
            .map_err(crate::errors::value)?;
        let request = TraceRequest::Add {
            window: self.window,
            tile: self.tile,
            field_id,
            field,
            color,
            width_px,
            mode,
            owner: self.context.owner.clone(),
        };
        call_immediate_detached(py, ControlRequest::Traces(request))
            .map_err(control_call_error)?
            .into_unit()
            .map_err(crate::errors::control)
    }

    fn extend(&self, py: Python<'_>, fields: Vec<Bound<'_, PyAny>>) -> PyResult<()> {
        for field in fields {
            self.add(py, field, None, None, "line")?;
        }
        Ok(())
    }

    #[pyo3(signature = (index=None, *, field=None))]
    fn remove(
        &self,
        py: Python<'_>,
        index: Option<usize>,
        field: Option<Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let resolved = field
            .as_ref()
            .map(|field| resolve_field(&self.context.snapshot, field))
            .transpose()?;
        if matches!((index, &resolved), (Some(_), Some(_)) | (None, None)) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "remove() needs exactly one of a position or field=",
            ));
        }
        let (field_id, field) = match resolved {
            Some((id, path)) => (Some(id), Some(path)),
            None => (None, None),
        };
        let request = TraceRequest::Remove {
            window: self.window,
            tile: self.tile,
            index,
            field_id,
            field,
        };
        call_immediate_detached(py, ControlRequest::Traces(request))
            .map_err(control_call_error)?
            .into_unit()
            .map_err(crate::errors::control)
    }

    fn clear(&self, py: Python<'_>) -> PyResult<()> {
        let request = TraceRequest::Clear {
            window: self.window,
            tile: self.tile,
        };
        call_immediate_detached(py, ControlRequest::Traces(request))
            .map_err(control_call_error)?
            .into_unit()
            .map_err(crate::errors::control)
    }

    fn list(&self, py: Python<'_>) -> PyResult<Vec<TracePy>> {
        Ok(request_traces(py, self.window, self.tile)?
            .into_iter()
            .map(|info| trace_from_info(self.window, self.tile, info))
            .collect())
    }

    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        Ok(self.list(py)?.len())
    }

    fn __getitem__(&self, py: Python<'_>, index: usize) -> PyResult<TracePy> {
        self.list(py)?
            .into_iter()
            .nth(index)
            .ok_or_else(|| pyo3::exceptions::PyIndexError::new_err("trace index out of range"))
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyIterator>> {
        let out = PyList::empty(py);
        for trace in self.list(py)? {
            out.append(Bound::new(py, trace)?)?;
        }
        out.try_iter()
    }
}

#[pyclass(unsendable, name = "Trace", skip_from_py_object)]
#[derive(Clone)]
pub struct TracePy {
    window: u64,
    tile: u64,
    #[pyo3(get)]
    index: usize,
    field_id: FieldId,
    #[pyo3(get)]
    field: String,
    color: [f32; 4],
    width_px: f32,
    mode: TraceMode,
    visible: bool,
}

#[pymethods]
impl TracePy {
    fn __repr__(&self) -> String {
        format!("<Trace {} index={}>", self.field, self.index)
    }

    #[getter]
    fn color(&self) -> String {
        format_hex_color(self.color)
    }

    #[setter]
    fn set_color(&mut self, py: Python<'_>, color: String) -> PyResult<()> {
        let parsed = parse_hex_color(&color).map_err(crate::errors::value)?;
        self.set_trace(py, Some(parsed), None, None, None)?;
        self.color = parsed;
        Ok(())
    }

    #[getter]
    fn width_px(&self) -> f32 {
        self.width_px
    }

    #[getter]
    fn mode(&self) -> String {
        trace_mode_name(self.mode).to_string()
    }

    #[setter]
    fn set_mode(&mut self, py: Python<'_>, mode: &str) -> PyResult<()> {
        let parsed = parse_trace_mode(mode)?;
        self.set_trace(py, None, None, Some(parsed), None)?;
        self.mode = parsed;
        Ok(())
    }

    #[getter]
    fn visible(&self) -> bool {
        self.visible
    }

    #[setter]
    fn set_visible(&mut self, py: Python<'_>, visible: bool) -> PyResult<()> {
        self.set_trace(py, None, None, None, Some(visible))?;
        self.visible = visible;
        Ok(())
    }
}

impl TracePy {
    fn set_trace(
        &self,
        py: Python<'_>,
        color: Option<[f32; 4]>,
        width_px: Option<f32>,
        mode: Option<TraceMode>,
        visible: Option<bool>,
    ) -> PyResult<()> {
        let request = TraceRequest::Set {
            window: self.window,
            tile: self.tile,
            index: self.index,
            field_id: self.field_id,
            color,
            width_px,
            mode,
            visible,
        };
        call_immediate_detached(py, ControlRequest::Traces(request))
            .map_err(control_call_error)?
            .into_unit()
            .map_err(crate::errors::control)
    }
}

fn request_traces(py: Python<'_>, window: u64, tile: u64) -> PyResult<Vec<TraceInfo>> {
    let response = call_immediate_detached(
        py,
        ControlRequest::Traces(TraceRequest::List { window, tile }),
    )
    .map_err(control_call_error)?;
    response.into_traces().map_err(crate::errors::control)
}

fn trace_from_info(window: u64, tile: u64, info: TraceInfo) -> TracePy {
    TracePy {
        window,
        tile,
        index: info.index,
        field_id: info.field_id,
        field: info.field,
        color: info.color,
        width_px: info.width_px,
        mode: info.mode,
        visible: info.visible,
    }
}

fn resolve_field(
    snapshot: &StoreSnapshot,
    field: &Bound<'_, PyAny>,
) -> PyResult<(FieldId, String)> {
    if let Ok(field_ref) = field.extract::<PyRef<'_, crate::api::FieldRefPy>>() {
        return Ok((field_ref.field_id(), field_ref.label()));
    }
    let path = field.extract::<String>().map_err(|_| {
        pyo3::exceptions::PyValueError::new_err(
            "trace field must be a string like 'topic.field' or a FieldRef",
        )
    })?;
    let field = resolve_field_path(snapshot, &path).map_err(crate::errors::value)?;
    Ok((
        field.field_id,
        format!("{}.{}", field.topic_name, field.field_name),
    ))
}

fn parse_trace_mode(name: &str) -> PyResult<TraceMode> {
    TraceMode::parse(name).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "trace mode must be 'line', 'scatter', or 'step', got {name:?}"
        ))
    })
}

fn trace_mode_name(mode: TraceMode) -> &'static str {
    match mode {
        TraceMode::Line => "line",
        TraceMode::Scatter => "scatter",
        TraceMode::Step => "step",
    }
}
