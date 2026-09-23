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
        match request(py, LayoutRequest::List)? {
            ControlResponse::Names(names) => Ok(names),
            _ => Err(wrong_response()),
        }
    }

    fn save(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        request_unit(
            py,
            LayoutRequest::Save {
                name: layout_name(name)?,
            },
        )
    }

    fn load(&self, py: Python<'_>, name: &str) -> PyResult<LoadReportPy> {
        request_report(
            py,
            LayoutRequest::Load {
                name: layout_name(name)?,
            },
        )
    }

    fn delete(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        request_unit(
            py,
            LayoutRequest::Delete {
                name: layout_name(name)?,
            },
        )
    }

    fn rename(&self, py: Python<'_>, old: &str, new: &str) -> PyResult<()> {
        request_unit(
            py,
            LayoutRequest::Rename {
                from: layout_name(old)?,
                to: layout_name(new)?,
            },
        )
    }

    fn duplicate(&self, py: Python<'_>, old: &str, new: &str) -> PyResult<()> {
        request_unit(
            py,
            LayoutRequest::Duplicate {
                from: layout_name(old)?,
                to: layout_name(new)?,
            },
        )
    }

    fn import_file(&self, py: Python<'_>, path: &str) -> PyResult<LoadReportPy> {
        request_report(
            py,
            LayoutRequest::ImportFile {
                path: layout_path(path)?,
            },
        )
    }

    fn export_file(&self, py: Python<'_>, name: &str, path: &str) -> PyResult<()> {
        request_unit(
            py,
            LayoutRequest::ExportFile {
                name: layout_name(name)?,
                path: layout_path(path)?,
            },
        )
    }

    fn clear(&self, py: Python<'_>) -> PyResult<()> {
        request_unit(py, LayoutRequest::Clear)
    }

    fn current(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let ControlResponse::Layout(json) = request(py, LayoutRequest::Current)? else {
            return Err(wrong_response());
        };
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
    match request(py, request_value)? {
        ControlResponse::Unit => Ok(()),
        _ => Err(wrong_response()),
    }
}

fn request_report(py: Python<'_>, request_value: LayoutRequest) -> PyResult<LoadReportPy> {
    match request(py, request_value)? {
        ControlResponse::LoadReport(report) => Ok(LoadReportPy { report }),
        _ => Err(wrong_response()),
    }
}

fn layout_name(name: &str) -> PyResult<String> {
    let name = name.trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(PyValueError::new_err(
            "layout names may contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    Ok(name.to_owned())
}

fn layout_path(path: &str) -> PyResult<String> {
    if path.trim().is_empty() {
        return Err(PyValueError::new_err("layout path must not be empty"));
    }
    Ok(path.to_owned())
}

fn wrong_response() -> PyErr {
    PyRuntimeError::new_err("the DeLOG window answered with the wrong kind of result")
}
