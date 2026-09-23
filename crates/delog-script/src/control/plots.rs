use pyo3::prelude::*;

use super::{ControlRequest, ControlResponse, PlotInfo, PlotRequest, call_immediate_detached};

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
}

#[pymethods]
impl PlotPy {
    fn __repr__(&self) -> String {
        format!("<Plot {} window={}>", self.label, self.window)
    }
}

pub fn list_plots(py: Python<'_>, window: Option<u64>) -> PyResult<Vec<PlotPy>> {
    let response = call_immediate_detached(py, ControlRequest::Plots(PlotRequest::List))
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
    let ControlResponse::Plots(infos) = response else {
        return Err(pyo3::exceptions::PyRuntimeError::new_err(
            "the DeLOG window answered with the wrong kind of result",
        ));
    };
    Ok(infos
        .into_iter()
        .filter(|info: &PlotInfo| window.is_none_or(|w| info.window == w))
        .map(|info| PlotPy {
            index: info.index,
            window: info.window,
            label: info.label,
            tile: info.tile,
        })
        .collect())
}
