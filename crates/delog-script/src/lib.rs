//! With the `python` feature off, only the feature-independent script library
//! (file persistence) is compiled; the interpreter engine lives behind `python`.

pub mod library;

#[cfg(feature = "python")]
pub mod parser_library;

#[cfg(feature = "python")]
pub mod custom_parser;

#[cfg(feature = "python")]
pub mod api;

#[cfg(feature = "python")]
pub mod params;

#[cfg(feature = "python")]
pub mod emit;

#[cfg(feature = "python")]
pub mod engine;

#[cfg(feature = "python")]
pub mod live;

#[cfg(feature = "python")]
pub use engine::{ParserEvent, ScriptCommand, ScriptEngine, ScriptEvent};

#[cfg(feature = "python")]
pub fn check_numpy() -> Result<(String, String), String> {
    use pyo3::prelude::*;
    use pyo3::types::PyAnyMethods;

    Python::attach(|py| {
        let sys = py.import("sys").map_err(|e| e.to_string())?;
        let py_version: String = sys
            .getattr("version")
            .and_then(|v| v.extract())
            .map_err(|e| e.to_string())?;
        let np = py.import("numpy").map_err(|e| e.to_string())?;
        let np_version: String = np
            .getattr("__version__")
            .and_then(|v| v.extract())
            .map_err(|e| e.to_string())?;
        Ok((py_version, np_version))
    })
}

#[cfg(all(test, feature = "python"))]
mod check_tests {
    use super::*;

    #[test]
    fn check_numpy_reports_versions() {
        let (py, np) = check_numpy().expect("numpy import should succeed in dev env");
        assert!(!py.is_empty());
        assert!(!np.is_empty());
    }
}
