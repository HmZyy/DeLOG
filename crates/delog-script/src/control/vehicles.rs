use delog_api::catalog::resolve_field_path;
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
        model_name(&self.info.spec.model).to_owned()
    }

    #[setter]
    fn set_model(&mut self, py: Python<'_>, model: &str) -> PyResult<()> {
        self.submit_patch(
            py,
            VehiclePatch {
                model: Some(parse_model(model)?),
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
                scale: Some(positive_scale(scale)?),
                ..VehiclePatch::default()
            },
        )
    }
}

impl VehiclePy {
    fn submit_patch(&mut self, py: Python<'_>, patch: VehiclePatch) -> PyResult<()> {
        let request = VehicleRequest::Set {
            id: self.info.id,
            patch: patch.clone(),
        };
        if stage_batch_request(&ControlRequest::Vehicles(request.clone()))
            .map_err(control_call_error)?
        {
            apply_patch_to_spec(&mut self.info.spec, patch);
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

fn apply_patch_to_spec(spec: &mut VehicleSpec, patch: VehiclePatch) {
    if let Some(value) = patch.label {
        spec.label = value;
    }
    if let Some(value) = patch.show {
        spec.show = value;
    }
    if let Some(value) = patch.show_path {
        spec.show_path = value;
    }
    if let Some(value) = patch.position {
        spec.position = value;
    }
    if let Some(value) = patch.orientation {
        spec.orientation = value;
    }
    if let Some(value) = patch.model {
        spec.model = value;
    }
    if let Some(value) = patch.color {
        spec.color = value;
    }
    if let Some(value) = patch.path_color {
        spec.path_color = value;
    }
    if let Some(value) = patch.scale {
        spec.scale = value;
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
        let (source_id, source) = resolve_source(&self.context.snapshot, source)?;
        let spec = VehicleSpec {
            source_id,
            source,
            label: label.to_owned(),
            show,
            show_path,
            position: pos.0.clone(),
            orientation: ori
                .as_deref()
                .map(|orientation| orientation.0.clone())
                .unwrap_or(VehicleOrientation::Static),
            model: parse_model(model)?,
            color: parse_hex_color(color).map_err(crate::errors::value)?,
            path_color: parse_hex_color(path_color).map_err(crate::errors::value)?,
            scale: positive_scale(scale)?,
            owner: self.context.owner.clone(),
        };
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
            let (source_id, _) = resolve_source(&self.context.snapshot, source)?;
            VehicleFilter::Source(source_id)
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
    Ok(ResolvedVehicleField {
        id: field.field_id,
        path: format!(
            "{}/{}/{}",
            field.source_label, field.topic_name, field.field_name
        ),
    })
}

fn finite(name: &str, value: f64) -> PyResult<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(PyValueError::new_err(format!("{name} must be finite")))
    }
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
    Ok(VehiclePositionPy(VehiclePosition::Gps {
        lat: resolve_field(snapshot, lat)?,
        lon: resolve_field(snapshot, lon)?,
        alt: resolve_field(snapshot, alt)?,
        lat_lon_dege7: dege7,
        alt_mm,
        alt_offset_m: finite("alt_offset_m", alt_offset_m)?,
    }))
}

pub(crate) fn ned(
    snapshot: &StoreSnapshot,
    north: &Bound<'_, PyAny>,
    east: &Bound<'_, PyAny>,
    down: &Bound<'_, PyAny>,
    reference: Option<&GeoReferencePy>,
) -> PyResult<VehiclePositionPy> {
    Ok(VehiclePositionPy(VehiclePosition::Ned {
        north: resolve_field(snapshot, north)?,
        east: resolve_field(snapshot, east)?,
        down: resolve_field(snapshot, down)?,
        reference: reference.map(|reference| reference.0.clone()),
    }))
}

pub(crate) fn geo(lat_deg: f64, lon_deg: f64, alt_m: f64) -> PyResult<GeoReferencePy> {
    let lat_deg = finite("lat_deg", lat_deg)?;
    let lon_deg = finite("lon_deg", lon_deg)?;
    let alt_m = finite("alt_m", alt_m)?;
    if !(-90.0..=90.0).contains(&lat_deg) {
        return Err(PyValueError::new_err("lat_deg must be between -90 and 90"));
    }
    if !(-180.0..=180.0).contains(&lon_deg) {
        return Err(PyValueError::new_err(
            "lon_deg must be between -180 and 180",
        ));
    }
    Ok(GeoReferencePy(VehicleNedReference::Manual {
        lat_deg,
        lon_deg,
        alt_m,
    }))
}

pub(crate) fn geo_fields(
    snapshot: &StoreSnapshot,
    lat: &Bound<'_, PyAny>,
    lon: &Bound<'_, PyAny>,
    alt: &Bound<'_, PyAny>,
) -> PyResult<GeoReferencePy> {
    Ok(GeoReferencePy(VehicleNedReference::Fields {
        lat: resolve_field(snapshot, lat)?,
        lon: resolve_field(snapshot, lon)?,
        alt: resolve_field(snapshot, alt)?,
    }))
}

pub(crate) fn euler(
    snapshot: &StoreSnapshot,
    roll: &Bound<'_, PyAny>,
    pitch: &Bound<'_, PyAny>,
    yaw: &Bound<'_, PyAny>,
    degrees: bool,
) -> PyResult<VehicleOrientationPy> {
    Ok(VehicleOrientationPy(VehicleOrientation::Euler {
        roll: resolve_field(snapshot, roll)?,
        pitch: resolve_field(snapshot, pitch)?,
        yaw: resolve_field(snapshot, yaw)?,
        degrees,
    }))
}

pub(crate) fn quat(
    snapshot: &StoreSnapshot,
    w: &Bound<'_, PyAny>,
    x: &Bound<'_, PyAny>,
    y: &Bound<'_, PyAny>,
    z: &Bound<'_, PyAny>,
) -> PyResult<VehicleOrientationPy> {
    Ok(VehicleOrientationPy(VehicleOrientation::Quat {
        w: resolve_field(snapshot, w)?,
        x: resolve_field(snapshot, x)?,
        y: resolve_field(snapshot, y)?,
        z: resolve_field(snapshot, z)?,
    }))
}

pub(crate) fn static_orientation() -> VehicleOrientationPy {
    VehicleOrientationPy(VehicleOrientation::Static)
}

fn request_vehicles(py: Python<'_>, request: VehicleRequest) -> PyResult<Vec<VehicleInfo>> {
    let response = call_immediate_detached(py, ControlRequest::Vehicles(request))
        .map_err(control_call_error)?;
    response.into_vehicles().map_err(crate::errors::control)
}

fn request_unit(py: Python<'_>, request: VehicleRequest) -> PyResult<()> {
    let response = call_immediate_detached(py, ControlRequest::Vehicles(request))
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

fn resolve_source(
    snapshot: &StoreSnapshot,
    requested: &str,
) -> PyResult<(delog_core::identity::SourceId, String)> {
    let matches: Vec<_> = snapshot
        .sources
        .iter()
        .filter(|source| !source.entry.removed && source.entry.label == requested)
        .collect();
    match matches.as_slice() {
        [source] => Ok((source.entry.id, source.entry.label.clone())),
        [] => Err(PyValueError::new_err(format!(
            "source '{requested}' not found"
        ))),
        _ => Err(PyValueError::new_err(format!(
            "source '{requested}' is ambiguous; candidate IDs: {}",
            matches
                .iter()
                .map(|source| source.entry.id.0.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn parse_model(name: &str) -> PyResult<VehicleModel> {
    match name {
        "none" => Ok(VehicleModel::None),
        "quad" => Ok(VehicleModel::Quad),
        "fixedwing" => Ok(VehicleModel::FixedWing),
        "deltawing" => Ok(VehicleModel::DeltaWing),
        "cone" => Ok(VehicleModel::Cone),
        "sphere" => Ok(VehicleModel::Sphere),
        "cube" => Ok(VehicleModel::Cube),
        path if path.ends_with(".glb") => Ok(VehicleModel::CustomGlb(path.to_owned())),
        _ => Err(PyValueError::new_err(format!(
            "vehicle model must be 'none', 'quad', 'fixedwing', 'deltawing', 'cone', 'sphere', 'cube', or a .glb path, got {name:?}"
        ))),
    }
}

fn model_name(model: &VehicleModel) -> &str {
    match model {
        VehicleModel::None => "none",
        VehicleModel::Quad => "quad",
        VehicleModel::FixedWing => "fixedwing",
        VehicleModel::DeltaWing => "deltawing",
        VehicleModel::Cone => "cone",
        VehicleModel::Sphere => "sphere",
        VehicleModel::Cube => "cube",
        VehicleModel::CustomGlb(path) => path,
    }
}

fn positive_scale(scale: f32) -> PyResult<f32> {
    if scale.is_finite() && scale > 0.0 {
        Ok(scale)
    } else {
        Err(PyValueError::new_err(
            "vehicle scale must be finite and > 0",
        ))
    }
}
