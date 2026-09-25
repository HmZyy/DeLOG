use delog_api::catalog::{resolve_field_path, resolve_source};
use delog_api::color::{format_hex_color, parse_hex_color};
use delog_core::snapshot::StoreSnapshot;
use pyo3::exceptions::{PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyIterator, PyList};

use super::{
    ControlRequest, PlotContext, ResolvedVehicleField, VehicleFilter, VehicleInfo, VehicleModel,
    VehicleNedReference, VehicleOrientation, VehiclePatch, VehiclePosition, VehicleRequest,
    VehicleSpec, call_immediate_detached, control_call_error, stage_batch_request,
};

pub(crate) mod profiles;

#[pyclass(unsendable, name = "VehiclePositionMapping", skip_from_py_object)]
#[derive(Clone)]
pub struct VehiclePositionPy(pub(crate) VehiclePosition);

#[pyclass(unsendable, name = "VehicleOrientationMapping", skip_from_py_object)]
#[derive(Clone)]
pub struct VehicleOrientationPy(pub(crate) VehicleOrientation);

#[pyclass(unsendable, name = "GeoReference", skip_from_py_object)]
#[derive(Clone)]
pub struct GeoReferencePy(pub(crate) VehicleNedReference);

#[pyclass(unsendable, name = "VehicleCollection", skip_from_py_object)]
#[derive(Clone)]
pub struct VehicleCollectionPy {
    context: PlotContext,
}

impl VehicleCollectionPy {
    pub(crate) fn new(context: PlotContext) -> Self {
        Self { context }
    }
}

#[pyclass(unsendable, name = "Vehicle", skip_from_py_object)]
#[derive(Clone)]
pub struct VehiclePy {
    pub(crate) info: VehicleInfo,
}

#[pymethods]
impl VehiclePy {
    fn __repr__(&self) -> String {
        format!("<Vehicle {} id={}>", self.info.spec.label, self.info.id)
    }

    #[getter]
    fn id(&self) -> u64 {
        self.info.id
    }

    #[getter]
    fn index(&self) -> usize {
        self.info.index
    }

    #[getter]
    fn source(&self) -> String {
        self.info.spec.source.clone()
    }

    #[getter]
    fn owner(&self) -> Option<String> {
        self.info
            .spec
            .owner
            .as_ref()
            .map(|owner| owner.name.clone())
    }

    #[getter]
    fn label(&self) -> String {
        self.info.spec.label.clone()
    }

    #[setter]
    fn set_label(&mut self, py: Python<'_>, label: String) -> PyResult<()> {
        self.submit_patch(
            py,
            VehiclePatch {
                label: Some(label),
                ..VehiclePatch::default()
            },
        )
    }

    #[getter]
    fn show(&self) -> bool {
        self.info.spec.show
    }

    #[setter]
    fn set_show(&mut self, py: Python<'_>, show: bool) -> PyResult<()> {
        self.submit_patch(
            py,
            VehiclePatch {
                show: Some(show),
                ..VehiclePatch::default()
            },
        )
    }

    #[getter]
    fn show_path(&self) -> bool {
        self.info.spec.show_path
    }

    #[setter]
    fn set_show_path(&mut self, py: Python<'_>, show_path: bool) -> PyResult<()> {
        self.submit_patch(
            py,
            VehiclePatch {
                show_path: Some(show_path),
                ..VehiclePatch::default()
            },
        )
    }

    #[getter]
    fn pos(&self) -> VehiclePositionPy {
        VehiclePositionPy(self.info.spec.position.clone())
    }

    #[setter]
    fn set_pos(&mut self, py: Python<'_>, pos: PyRef<'_, VehiclePositionPy>) -> PyResult<()> {
        self.submit_patch(
            py,
            VehiclePatch {
                position: Some(pos.0.clone()),
                ..VehiclePatch::default()
            },
        )
    }

    #[getter]
    fn ori(&self) -> VehicleOrientationPy {
        VehicleOrientationPy(self.info.spec.orientation.clone())
    }

    #[setter]
    fn set_ori(&mut self, py: Python<'_>, ori: PyRef<'_, VehicleOrientationPy>) -> PyResult<()> {
        self.submit_patch(
            py,
            VehiclePatch {
                orientation: Some(ori.0.clone()),
                ..VehiclePatch::default()
            },
        )
    }

    #[getter]
    fn model(&self) -> String {
        self.info.spec.model.as_str().to_owned()
    }

    #[setter]
    fn set_model(&mut self, py: Python<'_>, model: &str) -> PyResult<()> {
        self.submit_patch(
            py,
            VehiclePatch {
                model: Some(VehicleModel::parse(model).map_err(crate::errors::value)?),
                ..VehiclePatch::default()
            },
        )
    }

    #[getter]
    fn color(&self) -> String {
        format_hex_color(self.info.spec.color)
    }

    #[setter]
    fn set_color(&mut self, py: Python<'_>, color: &str) -> PyResult<()> {
        self.submit_patch(
            py,
            VehiclePatch {
                color: Some(parse_hex_color(color).map_err(crate::errors::value)?),
                ..VehiclePatch::default()
            },
        )
    }

    #[getter]
    fn path_color(&self) -> String {
        format_hex_color(self.info.spec.path_color)
    }

    #[setter]
    fn set_path_color(&mut self, py: Python<'_>, path_color: &str) -> PyResult<()> {
        self.submit_patch(
            py,
            VehiclePatch {
                path_color: Some(parse_hex_color(path_color).map_err(crate::errors::value)?),
                ..VehiclePatch::default()
            },
        )
    }

    #[getter]
    fn scale(&self) -> f32 {
        self.info.spec.scale
    }

    #[setter]
    fn set_scale(&mut self, py: Python<'_>, scale: f32) -> PyResult<()> {
        self.submit_patch(
            py,
            VehiclePatch {
                scale: Some(scale),
                ..VehiclePatch::default()
            },
        )
    }
}

impl VehiclePy {
    fn submit_patch(&mut self, py: Python<'_>, patch: VehiclePatch) -> PyResult<()> {
        patch.validate().map_err(crate::errors::value)?;
        let request = VehicleRequest::Set {
            id: self.info.id,
            patch: patch.clone(),
        };
        if stage_batch_request(&ControlRequest::Vehicles(Box::new(request.clone())))
            .map_err(control_call_error)?
        {
            self.info
                .spec
                .apply_patch(patch)
                .map_err(crate::errors::value)?;
            return Ok(());
        }
        let mut infos = request_vehicles(py, request)?;
        if infos.len() != 1 || infos[0].id != self.info.id {
            return Err(wrong_response());
        }
        self.info = infos.remove(0);
        Ok(())
    }
}

#[pymethods]
impl VehicleCollectionPy {
    fn list(&self, py: Python<'_>) -> PyResult<Vec<VehiclePy>> {
        Ok(request_vehicles(py, VehicleRequest::List)?
            .into_iter()
            .map(vehicle_from_info)
            .collect())
    }

    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        Ok(self.list(py)?.len())
    }

    fn __getitem__(&self, py: Python<'_>, index: usize) -> PyResult<VehiclePy> {
        self.list(py)?
            .into_iter()
            .nth(index)
            .ok_or_else(|| PyIndexError::new_err("vehicle index out of range"))
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyIterator>> {
        let out = PyList::empty(py);
        for vehicle in self.list(py)? {
            out.append(Bound::new(py, vehicle)?)?;
        }
        out.try_iter()
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (source, *, pos, label="Vehicle", ori=None, model="quad", color="#5AAAFFFF", path_color="#FFAA3CFF", scale=1.0, show=true, show_path=true))]
    fn add(
        &self,
        py: Python<'_>,
        source: &str,
        pos: PyRef<'_, VehiclePositionPy>,
        label: &str,
        ori: Option<PyRef<'_, VehicleOrientationPy>>,
        model: &str,
        color: &str,
        path_color: &str,
        scale: f32,
        show: bool,
        show_path: bool,
    ) -> PyResult<VehiclePy> {
        let source =
            resolve_source(&self.context.snapshot, source).map_err(crate::errors::value)?;
        let spec = VehicleSpec {
            source_id: source.source_id,
            source: source.source_label,
            label: label.to_owned(),
            show,
            show_path,
            position: pos.0.clone(),
            orientation: ori
                .as_deref()
                .map(|orientation| orientation.0.clone())
                .unwrap_or_else(VehicleOrientation::static_orientation),
            model: VehicleModel::parse(model).map_err(crate::errors::value)?,
            color: parse_hex_color(color).map_err(crate::errors::value)?,
            path_color: parse_hex_color(path_color).map_err(crate::errors::value)?,
            scale,
            owner: self.context.owner.clone(),
        };
        spec.validate().map_err(crate::errors::value)?;
        let mut infos = request_vehicles(py, VehicleRequest::Add(spec))?;
        if infos.len() != 1 {
            return Err(wrong_response());
        }
        Ok(vehicle_from_info(infos.remove(0)))
    }

    #[pyo3(signature = (target=None, *, label=None, source=None))]
    fn remove(
        &self,
        py: Python<'_>,
        target: Option<Bound<'_, PyAny>>,
        label: Option<String>,
        source: Option<&str>,
    ) -> PyResult<()> {
        let axis_count = usize::from(target.is_some())
            + usize::from(label.is_some())
            + usize::from(source.is_some());
        if axis_count != 1 {
            return Err(PyValueError::new_err(
                "remove() needs exactly one of a position/handle, label=, or source=",
            ));
        }
        let filter = if let Some(target) = target {
            if let Ok(index) = target.extract::<usize>() {
                VehicleFilter::Index(index)
            } else if let Ok(handle) = target.extract::<PyRef<'_, VehiclePy>>() {
                VehicleFilter::Id(handle.info.id)
            } else {
                return Err(PyValueError::new_err(
                    "remove() position must be an index or a Vehicle handle",
                ));
            }
        } else if let Some(label) = label {
            VehicleFilter::Label(label)
        } else {
            let source = source.ok_or_else(|| {
                PyValueError::new_err(
                    "remove() needs exactly one of a position/handle, label=, or source=",
                )
            })?;
            let source =
                resolve_source(&self.context.snapshot, source).map_err(crate::errors::value)?;
            VehicleFilter::Source(source.source_id)
        };
        request_unit(py, VehicleRequest::Remove(filter))
    }

    fn clear(&self, py: Python<'_>) -> PyResult<()> {
        request_unit(py, VehicleRequest::Remove(VehicleFilter::All))
    }
}

fn resolve_field(
    snapshot: &StoreSnapshot,
    value: &Bound<'_, PyAny>,
) -> PyResult<ResolvedVehicleField> {
    if let Ok(field) = value.extract::<PyRef<'_, crate::api::FieldRefPy>>() {
        return Ok(ResolvedVehicleField {
            id: field.field_id(),
            path: field.qualified_path().to_owned(),
        });
    }
    let path = value.extract::<String>().map_err(|_| {
        PyValueError::new_err("vehicle fields must be a string like 'topic.field' or a FieldRef")
    })?;
    let field = resolve_field_path(snapshot, &path).map_err(crate::errors::value)?;
    Ok(field.into())
}

pub(crate) fn gps(
    snapshot: &StoreSnapshot,
    lat: &Bound<'_, PyAny>,
    lon: &Bound<'_, PyAny>,
    alt: &Bound<'_, PyAny>,
    dege7: bool,
    alt_mm: bool,
    alt_offset_m: f64,
) -> PyResult<VehiclePositionPy> {
    VehiclePosition::gps(
        resolve_field(snapshot, lat)?,
        resolve_field(snapshot, lon)?,
        resolve_field(snapshot, alt)?,
        dege7,
        alt_mm,
        alt_offset_m,
    )
    .map(VehiclePositionPy)
    .map_err(crate::errors::value)
}

pub(crate) fn ned(
    snapshot: &StoreSnapshot,
    north: &Bound<'_, PyAny>,
    east: &Bound<'_, PyAny>,
    down: &Bound<'_, PyAny>,
    reference: Option<&GeoReferencePy>,
) -> PyResult<VehiclePositionPy> {
    VehiclePosition::ned(
        resolve_field(snapshot, north)?,
        resolve_field(snapshot, east)?,
        resolve_field(snapshot, down)?,
        reference.map(|reference| reference.0.clone()),
    )
    .map(VehiclePositionPy)
    .map_err(crate::errors::value)
}

pub(crate) fn geo(lat_deg: f64, lon_deg: f64, alt_m: f64) -> PyResult<GeoReferencePy> {
    VehicleNedReference::manual(lat_deg, lon_deg, alt_m)
        .map(GeoReferencePy)
        .map_err(crate::errors::value)
}

pub(crate) fn geo_fields(
    snapshot: &StoreSnapshot,
    lat: &Bound<'_, PyAny>,
    lon: &Bound<'_, PyAny>,
    alt: &Bound<'_, PyAny>,
) -> PyResult<GeoReferencePy> {
    Ok(GeoReferencePy(VehicleNedReference::fields(
        resolve_field(snapshot, lat)?,
        resolve_field(snapshot, lon)?,
        resolve_field(snapshot, alt)?,
    )))
}

pub(crate) fn euler(
    snapshot: &StoreSnapshot,
    roll: &Bound<'_, PyAny>,
    pitch: &Bound<'_, PyAny>,
    yaw: &Bound<'_, PyAny>,
    degrees: bool,
) -> PyResult<VehicleOrientationPy> {
    Ok(VehicleOrientationPy(VehicleOrientation::euler(
        resolve_field(snapshot, roll)?,
        resolve_field(snapshot, pitch)?,
        resolve_field(snapshot, yaw)?,
        degrees,
    )))
}

pub(crate) fn quat(
    snapshot: &StoreSnapshot,
    w: &Bound<'_, PyAny>,
    x: &Bound<'_, PyAny>,
    y: &Bound<'_, PyAny>,
    z: &Bound<'_, PyAny>,
) -> PyResult<VehicleOrientationPy> {
    Ok(VehicleOrientationPy(VehicleOrientation::quaternion(
        resolve_field(snapshot, w)?,
        resolve_field(snapshot, x)?,
        resolve_field(snapshot, y)?,
        resolve_field(snapshot, z)?,
    )))
}

pub(crate) fn static_orientation() -> VehicleOrientationPy {
    VehicleOrientationPy(VehicleOrientation::static_orientation())
}

fn request_vehicles(py: Python<'_>, request: VehicleRequest) -> PyResult<Vec<VehicleInfo>> {
    let response = call_immediate_detached(py, ControlRequest::Vehicles(Box::new(request)))
        .map_err(control_call_error)?;
    response.into_vehicles().map_err(crate::errors::control)
}

fn request_unit(py: Python<'_>, request: VehicleRequest) -> PyResult<()> {
    let response = call_immediate_detached(py, ControlRequest::Vehicles(Box::new(request)))
        .map_err(control_call_error)?;
    response.into_unit().map_err(crate::errors::control)
}

fn wrong_response() -> PyErr {
    crate::errors::control(delog_api::Error::protocol(
        "the DeLOG window answered with the wrong kind of result",
    ))
}

fn vehicle_from_info(info: VehicleInfo) -> VehiclePy {
    VehiclePy { info }
}
