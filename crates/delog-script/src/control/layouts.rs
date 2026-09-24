use delog_api::control::{validate_layout_name, validate_layout_path};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};

use super::{
    ControlRequest, ControlResponse, LayoutRequest, LoadReport, call_immediate_detached,
    control_call_error,
};

#[pyclass(unsendable, name = "Layouts", skip_from_py_object)]
#[derive(Clone, Default)]
pub struct LayoutsPy;

#[pymethods]
impl LayoutsPy {
    fn list(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        request(py, LayoutRequest::List)?
            .into_names()
            .map_err(crate::errors::control)
    }

    fn save(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        request_unit(
            py,
            LayoutRequest::Save {
                name: validate_layout_name(name).map_err(crate::errors::value)?,
            },
        )
    }

    fn load(&self, py: Python<'_>, name: &str) -> PyResult<LoadReportPy> {
        request_report(
            py,
            LayoutRequest::Load {
                name: validate_layout_name(name).map_err(crate::errors::value)?,
            },
        )
    }

    fn delete(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        request_unit(
            py,
            LayoutRequest::Delete {
                name: validate_layout_name(name).map_err(crate::errors::value)?,
            },
        )
    }

    fn rename(&self, py: Python<'_>, old: &str, new: &str) -> PyResult<()> {
        request_unit(
            py,
            LayoutRequest::Rename {
                from: validate_layout_name(old).map_err(crate::errors::value)?,
                to: validate_layout_name(new).map_err(crate::errors::value)?,
            },
        )
    }

    fn duplicate(&self, py: Python<'_>, old: &str, new: &str) -> PyResult<()> {
        request_unit(
            py,
            LayoutRequest::Duplicate {
                from: validate_layout_name(old).map_err(crate::errors::value)?,
                to: validate_layout_name(new).map_err(crate::errors::value)?,
            },
        )
    }

    fn import_file(&self, py: Python<'_>, path: &str) -> PyResult<LoadReportPy> {
        request_report(
            py,
            LayoutRequest::ImportFile {
                path: validate_layout_path(path).map_err(crate::errors::value)?,
            },
        )
    }

    fn export_file(&self, py: Python<'_>, name: &str, path: &str) -> PyResult<()> {
        request_unit(
            py,
            LayoutRequest::ExportFile {
                name: validate_layout_name(name).map_err(crate::errors::value)?,
                path: validate_layout_path(path).map_err(crate::errors::value)?,
            },
        )
    }

    fn clear(&self, py: Python<'_>) -> PyResult<()> {
        request_unit(py, LayoutRequest::Clear)
    }

    fn current(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let json = request(py, LayoutRequest::Current)?
            .into_layout()
            .map_err(crate::errors::control)?;
        let value = py
            .import("json")
            .and_then(|module| module.call_method1("loads", (json,)))
            .map_err(|error| {
                PyRuntimeError::new_err(format!(
                    "the DeLOG window returned invalid layout JSON: {error}"
                ))
            })?;
        value
            .cast::<PyDict>()
            .map(|dict| dict.clone().unbind())
            .map_err(|_| PyRuntimeError::new_err("the DeLOG window returned a non-object layout"))
    }

    fn apply(&self, py: Python<'_>, doc: &Bound<'_, PyAny>) -> PyResult<LoadReportPy> {
        let doc = doc
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("layout apply() needs a dict"))?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("allow_nan", false)?;
        kwargs.set_item("separators", (",", ":"))?;
        let json = py
            .import("json")
            .and_then(|module| module.call_method("dumps", (doc,), Some(&kwargs)))
            .and_then(|value| value.extract::<String>())
            .map_err(|error| PyValueError::new_err(format!("layout is not valid JSON: {error}")))?;
        request_report(py, LayoutRequest::Apply { json })
    }
}

#[pyclass(unsendable, name = "LoadReport", skip_from_py_object)]
#[derive(Clone)]
pub struct LoadReportPy {
    report: LoadReport,
}

#[pymethods]
impl LoadReportPy {
    #[getter]
    fn ambiguous(&self, py: Python<'_>) -> PyResult<Vec<Py<PyDict>>> {
        self.report
            .ambiguous
            .iter()
            .map(|issue| {
                let item = PyDict::new(py);
                item.set_item("field", &issue.field)?;
                item.set_item("candidates", &issue.candidates)?;
                Ok(item.unbind())
            })
            .collect()
    }

    #[getter]
    fn unresolved(&self) -> Vec<String> {
        self.report.unresolved.clone()
    }

    #[getter]
    fn warnings(&self) -> Vec<String> {
        self.report.warnings.clone()
    }
}

fn request(py: Python<'_>, request: LayoutRequest) -> PyResult<ControlResponse> {
    call_immediate_detached(py, ControlRequest::Layouts(request)).map_err(control_call_error)
}

fn request_unit(py: Python<'_>, request_value: LayoutRequest) -> PyResult<()> {
    request(py, request_value)?
        .into_unit()
        .map_err(crate::errors::control)
}

fn request_report(py: Python<'_>, request_value: LayoutRequest) -> PyResult<LoadReportPy> {
    request(py, request_value)?
        .into_load_report()
        .map(|report| LoadReportPy { report })
        .map_err(crate::errors::control)
}
