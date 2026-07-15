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
pub mod operations;

#[cfg(feature = "python")]
pub use api::PendingMarker;

#[cfg(feature = "python")]
pub use engine::{
    LiveBatchInput, MarkerCommand, ParserEvent, ScriptCommand, ScriptEngine, ScriptEvent,
};

#[cfg(feature = "python")]
pub const SCRIPTING_PACKAGES: &[(&str, &str)] = &[
    ("numpy", "numpy"),
    ("scipy.spatial.transform", "scipy"),
    ("bottleneck", "bottleneck"),
    ("cffi", "cffi"),
];

#[cfg(feature = "python")]
pub fn check_scripting() -> Result<(String, Vec<(String, String)>), String> {
    use pyo3::prelude::*;
    use pyo3::types::PyAnyMethods;

    Python::attach(|py| {
        let sys = py.import("sys").map_err(|e| e.to_string())?;
        let py_version: String = sys
            .getattr("version")
            .and_then(|v| v.extract())
            .map_err(|e| e.to_string())?;

        let mut versions = Vec::with_capacity(SCRIPTING_PACKAGES.len());
        for (import_module, version_module) in SCRIPTING_PACKAGES {
            py.import(*import_module)
                .map_err(|e| format!("import {import_module}: {e}"))?;
            let version: String = py
                .import(*version_module)
                .and_then(|m| m.getattr("__version__"))
                .and_then(|v| v.extract())
                .map_err(|e| format!("{version_module}.__version__: {e}"))?;
            versions.push((version_module.to_string(), version));
        }
        Ok((py_version, versions))
    })
}

#[cfg(all(test, feature = "python"))]
mod check_tests {
    use super::*;

    #[test]
    fn check_scripting_reports_versions() {
        match check_scripting() {
            Ok((py, packages)) => {
                assert!(!py.is_empty());
                for (name, version) in &packages {
                    assert!(!version.is_empty(), "{name} reported an empty version");
                }
            }
            Err(err) => assert!(
                !err.contains("import numpy") && !err.starts_with("numpy"),
                "numpy must be importable: {err}"
            ),
        }
    }
}
