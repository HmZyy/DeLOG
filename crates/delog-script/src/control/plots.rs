use pyo3::prelude::*;

use super::traces::TraceCollectionPy;
use super::{
    ControlRequest, ControlResponse, PlotContext, PlotInfo, PlotRequest, call_immediate_detached,
};

#[pyclass(unsendable, name = "Plot", skip_from_py_object)]
#[derive(Clone)]
pub struct PlotPy {
    #[pyo3(get)]
    pub index: usize,
    #[pyo3(get)]
    pub window: u64,
    #[pyo3(get)]
    pub label: String,
    pub tile: u64,
    context: PlotContext,
}

#[pymethods]
impl PlotPy {
    fn __repr__(&self) -> String {
        format!("<Plot {} window={}>", self.label, self.window)
    }

    #[getter]
    fn traces(&self) -> TraceCollectionPy {
        TraceCollectionPy::new(self.window, self.tile, self.context.clone())
    }
}

pub fn list_plots(
    py: Python<'_>,
    window: Option<u64>,
    context: PlotContext,
) -> PyResult<Vec<PlotPy>> {
    let infos = request_plots(py, PlotRequest::List { window })?;
    Ok(infos
        .into_iter()
        .map(|info| plot_from_info(info, context.clone()))
        .collect())
}

pub fn focused_plot(py: Python<'_>, context: PlotContext) -> PyResult<Option<PlotPy>> {
    Ok(request_plots(py, PlotRequest::Focused)?
        .into_iter()
        .next()
        .map(|info| plot_from_info(info, context)))
}

fn request_plots(py: Python<'_>, request: PlotRequest) -> PyResult<Vec<PlotInfo>> {
    let response = call_immediate_detached(py, ControlRequest::Plots(request))
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
    match response {
        ControlResponse::Plots(infos) => Ok(infos),
        _ => Err(pyo3::exceptions::PyRuntimeError::new_err(
            "the DeLOG window answered with the wrong kind of result",
        )),
    }
}

pub(crate) fn plot_from_info(info: PlotInfo, context: PlotContext) -> PlotPy {
    PlotPy {
        index: info.index,
        window: info.window,
        label: info.label,
        tile: info.tile,
        context,
    }
}
