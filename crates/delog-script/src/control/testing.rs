use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use super::{ControlHost, install_host};

fn eval(host: Option<Arc<dyn ControlHost>>, expression: &str) -> Result<Vec<String>, String> {
    let _guard = install_host(host);
    Python::attach(|py| {
        let globals = PyDict::new(py);
        let delog = crate::api::Delog::new(
            Arc::new(delog_core::snapshot::StoreSnapshot::empty()),
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
