use std::sync::Arc;

use delog_api::control::{ControlHost, ControlRequest, VehicleOrientation, VehiclePosition};
use delog_core::snapshot::StoreSnapshot;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use super::install_host;

fn globals_with_delog(
    py: Python<'_>,
    snapshot: Arc<StoreSnapshot>,
) -> Result<Bound<'_, PyDict>, String> {
    globals_with_delog_named(py, snapshot, String::new(), 0)
}

fn globals_with_delog_named(
    py: Python<'_>,
    snapshot: Arc<StoreSnapshot>,
    script_name: String,
    generation: u64,
) -> Result<Bound<'_, PyDict>, String> {
    globals_with_delog_named_and_markers(
        py,
        snapshot,
        script_name,
        generation,
        std::rc::Rc::default(),
    )
}

fn globals_with_delog_named_and_markers(
    py: Python<'_>,
    snapshot: Arc<StoreSnapshot>,
    script_name: String,
    generation: u64,
    markers: crate::staging::MarkerBuffer,
) -> Result<Bound<'_, PyDict>, String> {
    let globals = PyDict::new(py);
    let delog = crate::api::Delog::new(
        snapshot,
        std::rc::Rc::default(),
        std::rc::Rc::default(),
        std::rc::Rc::default(),
        markers,
        std::rc::Rc::default(),
        script_name,
        generation,
        delog_api::params::shared_empty(),
    );
    globals
        .set_item("delog", Bound::new(py, delog).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(globals)
}

pub fn eval_with_host_and_staged_batches(
    host: Arc<dyn ControlHost>,
    statement: &str,
) -> Result<Vec<Vec<ControlRequest>>, String> {
    let _guard = install_host(Some(host));
    Python::attach(|py| {
        let batches: super::DeferredControlBuffer = std::rc::Rc::default();
        let globals = PyDict::new(py);
        let delog = crate::api::Delog::new(
            Arc::new(StoreSnapshot::empty()),
            std::rc::Rc::default(),
            std::rc::Rc::default(),
            std::rc::Rc::default(),
            std::rc::Rc::default(),
            std::rc::Rc::clone(&batches),
            "flight.py".into(),
            1,
            delog_api::params::shared_empty(),
        );
        globals
            .set_item("delog", Bound::new(py, delog).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let code = std::ffi::CString::new(statement).map_err(|e| e.to_string())?;
        py.run(&code, Some(&globals), None)
            .map_err(|e| e.to_string())?;
        let staged = batches.borrow().clone();
        Ok(staged)
    })
}

pub fn eval_with_host_and_staged_markers(
    host: Arc<dyn ControlHost>,
    statement: &str,
) -> Result<Vec<delog_api::markers::PendingMarker>, String> {
    let _guard = install_host(Some(host));
    Python::attach(|py| {
        let markers: crate::staging::MarkerBuffer = std::rc::Rc::default();
        let globals = globals_with_delog_named_and_markers(
            py,
            Arc::new(StoreSnapshot::empty()),
            "flight.py".into(),
            1,
            std::rc::Rc::clone(&markers),
        )?;
        let code = std::ffi::CString::new(statement).map_err(|e| e.to_string())?;
        py.run(&code, Some(&globals), None)
            .map_err(|e| e.to_string())?;
        let staged = markers.borrow().clone();
        Ok(staged)
    })
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

pub fn eval_vehicle_mappings_with_snapshot(
    snapshot: Arc<StoreSnapshot>,
    statement: &str,
) -> Result<(VehiclePosition, VehicleOrientation), String> {
    Python::attach(|py| {
        let globals = globals_with_delog(py, snapshot)?;
        let code = std::ffi::CString::new(statement).map_err(|e| e.to_string())?;
        py.run(&code, Some(&globals), None)
            .map_err(|e| e.to_string())?;
        let position = globals
            .get_item("pos")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "test script did not define 'pos'".to_string())?;
        let orientation = globals
            .get_item("ori")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "test script did not define 'ori'".to_string())?;
        let position = position
            .extract::<PyRef<'_, super::vehicles::VehiclePositionPy>>()
            .map_err(|e| e.to_string())?
            .0
            .clone();
        let orientation = orientation
            .extract::<PyRef<'_, super::vehicles::VehicleOrientationPy>>()
            .map_err(|e| e.to_string())?
            .0
            .clone();
        Ok((position, orientation))
    })
}

pub fn eval_named_with_host(
    host: Arc<dyn ControlHost>,
    script_name: &str,
    generation: u64,
    statement: &str,
) -> Result<(), String> {
    let _guard = install_host(Some(host));
    Python::attach(|py| {
        let globals = globals_with_delog_named(
            py,
            Arc::new(StoreSnapshot::empty()),
            script_name.to_string(),
            generation,
        )?;
        let code = std::ffi::CString::new(statement).map_err(|e| e.to_string())?;
        py.run(&code, Some(&globals), None)
            .map_err(|e| e.to_string())
    })
}
