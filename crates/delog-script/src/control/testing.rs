use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use delog_core::snapshot::StoreSnapshot;

use super::{ControlHost, install_host};

fn globals_with_delog(
    py: Python<'_>,
    snapshot: Arc<StoreSnapshot>,
) -> Result<Bound<'_, PyDict>, String> {
    let globals = PyDict::new(py);
    let delog = crate::api::Delog::new(
        snapshot,
        std::rc::Rc::default(),
        std::rc::Rc::default(),
        std::rc::Rc::default(),
        std::rc::Rc::default(),
        String::new(),
        0,
        crate::params::shared_empty(),
    );
    globals
        .set_item("delog", Bound::new(py, delog).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(globals)
}

fn eval(host: Option<Arc<dyn ControlHost>>, expression: &str) -> Result<Vec<String>, String> {
    let _guard = install_host(host);
    Python::attach(|py| {
        let globals = globals_with_delog(py, Arc::new(StoreSnapshot::empty()))?;
        let code = std::ffi::CString::new(expression).map_err(|e| e.to_string())?;
        let value = py
            .eval(&code, Some(&globals), None)
            .map_err(|e| e.to_string())?;
        value.extract::<Vec<String>>().or_else(|_| Ok(Vec::new()))
    })
}

pub fn eval_to_strings(
    host: Arc<dyn ControlHost>,
    expression: &str,
) -> Result<Vec<String>, String> {
    eval(Some(host), expression)
}

pub fn eval_without_host(expression: &str) -> Result<Vec<String>, String> {
    eval(None, expression)
}

pub fn eval_with_host(host: Arc<dyn ControlHost>, statement: &str) -> Result<(), String> {
    eval_with_host_and_snapshot(host, Arc::new(StoreSnapshot::empty()), statement)
}

pub fn eval_with_host_and_snapshot(
    host: Arc<dyn ControlHost>,
    snapshot: Arc<StoreSnapshot>,
    statement: &str,
) -> Result<(), String> {
    let _guard = install_host(Some(host));
    Python::attach(|py| {
        let globals = globals_with_delog(py, snapshot)?;
        let code = std::ffi::CString::new(statement).map_err(|e| e.to_string())?;
        py.run(&code, Some(&globals), None)
            .map_err(|e| e.to_string())
    })
}
