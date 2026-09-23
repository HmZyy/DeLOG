use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use crate::control::{
    ControlRequest, ControlResponse, PlotContext, ProfileFieldRef, ProfileNedReference,
    ProfileOrientation, ProfilePosition, VehicleProfileInfo, VehicleProfileRequest,
    call_immediate_detached, control_call_error,
};

use super::{VehiclePy, format_color, model_name, resolve_source, vehicle_from_info};

#[pyclass(unsendable, name = "VehicleProfiles", skip_from_py_object)]
#[derive(Clone)]
pub struct VehicleProfilesPy {
    context: PlotContext,
}

impl VehicleProfilesPy {
    pub(crate) fn new(context: PlotContext) -> Self {
        Self { context }
    }
}

#[pyclass(unsendable, name = "VehicleProfile", skip_from_py_object)]
#[derive(Clone)]
pub struct VehicleProfilePy {
    info: VehicleProfileInfo,
}

#[pyclass(unsendable, name = "VehicleProfileField", skip_from_py_object)]
#[derive(Clone)]
pub struct ProfileFieldPy(ProfileFieldRef);

#[pyclass(
    unsendable,
    name = "VehicleProfilePositionMapping",
    skip_from_py_object
)]
#[derive(Clone)]
pub struct ProfilePositionPy(ProfilePosition);

#[pyclass(
    unsendable,
    name = "VehicleProfileOrientationMapping",
    skip_from_py_object
)]
#[derive(Clone)]
pub struct ProfileOrientationPy(ProfileOrientation);

#[pyclass(unsendable, name = "VehicleProfileNedReference", skip_from_py_object)]
#[derive(Clone)]
pub struct ProfileNedReferencePy(ProfileNedReference);

#[pymethods]
impl VehicleProfilesPy {
    fn list(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        match request_profile(py, VehicleProfileRequest::List)? {
            ControlResponse::Names(names) => Ok(names),
            other => Err(profile_response_error(other)),
        }
    }

    fn save(&self, py: Python<'_>, name: &str, vehicle: PyRef<'_, VehiclePy>) -> PyResult<()> {
        let name = validate_profile_name(name)?;
        request_profile_unit(
            py,
            VehicleProfileRequest::Save {
                name,
                vehicle_id: vehicle.info.id,
            },
        )
    }

    fn load(&self, py: Python<'_>, name: &str) -> PyResult<VehicleProfilePy> {
        let name = validate_profile_name(name)?;
        profile_from_response(request_profile(py, VehicleProfileRequest::Load { name })?)
    }

    #[pyo3(signature = (name, *, source))]
    fn apply(&self, py: Python<'_>, name: &str, source: &str) -> PyResult<VehiclePy> {
        let name = validate_profile_name(name)?;
        let (source_id, source) = resolve_source(&self.context.snapshot, source)?;
        match request_profile(
            py,
            VehicleProfileRequest::Apply {
                name,
                source_id,
                source,
                owner: self.context.owner.clone(),
            },
        )? {
            ControlResponse::Vehicles(mut infos) if infos.len() == 1 => {
                Ok(vehicle_from_info(infos.remove(0)))
            }
            other => Err(profile_response_error(other)),
        }
    }

    fn delete(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        let name = validate_profile_name(name)?;
        request_profile_unit(py, VehicleProfileRequest::Delete { name })
    }
}

#[pymethods]
impl VehicleProfilePy {
    #[getter]
    fn name(&self) -> String {
        self.info.name.clone()
    }

    #[getter]
    fn label(&self) -> String {
        self.info.label.clone()
    }

    #[getter]
    fn show(&self) -> bool {
        self.info.show
    }

    #[getter]
    fn show_path(&self) -> bool {
        self.info.show_path
    }

    #[getter]
    fn position(&self) -> ProfilePositionPy {
        ProfilePositionPy(self.info.position.clone())
    }

    #[getter]
    fn orientation(&self) -> ProfileOrientationPy {
        ProfileOrientationPy(self.info.orientation.clone())
    }

    #[getter]
    fn model(&self) -> String {
        model_name(&self.info.model).to_owned()
    }

    #[getter]
    fn color(&self) -> String {
        format_color(self.info.color)
    }

    #[getter]
    fn path_color(&self) -> String {
        format_color(self.info.path_color)
    }

    #[getter]
    fn scale(&self) -> f32 {
        self.info.scale
    }
}

#[pymethods]
impl ProfileFieldPy {
    #[getter]
    fn topic(&self) -> String {
        self.0.topic.clone()
    }

    #[getter]
    fn field(&self) -> String {
        self.0.field.clone()
    }
}

#[pymethods]
impl ProfilePositionPy {
    #[getter]
    fn kind(&self) -> &'static str {
        match self.0 {
            ProfilePosition::Ned { .. } => "ned",
            ProfilePosition::Gps { .. } => "gps",
        }
    }

    #[getter]
    fn north(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfilePosition::Ned { north, .. } => Some(profile_field(north)),
            ProfilePosition::Gps { .. } => None,
        }
    }

    #[getter]
    fn east(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfilePosition::Ned { east, .. } => Some(profile_field(east)),
            ProfilePosition::Gps { .. } => None,
        }
    }

    #[getter]
    fn down(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfilePosition::Ned { down, .. } => Some(profile_field(down)),
            ProfilePosition::Gps { .. } => None,
        }
    }

    #[getter]
    fn reference(&self) -> Option<ProfileNedReferencePy> {
        match &self.0 {
            ProfilePosition::Ned { reference, .. } => reference.clone().map(ProfileNedReferencePy),
            ProfilePosition::Gps { .. } => None,
        }
    }

    #[getter]
    fn lat(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfilePosition::Gps { lat, .. } => Some(profile_field(lat)),
            ProfilePosition::Ned { .. } => None,
        }
    }

    #[getter]
    fn lon(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfilePosition::Gps { lon, .. } => Some(profile_field(lon)),
            ProfilePosition::Ned { .. } => None,
        }
    }

    #[getter]
    fn alt(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfilePosition::Gps { alt, .. } => Some(profile_field(alt)),
            ProfilePosition::Ned { .. } => None,
        }
    }

    #[getter]
    fn dege7(&self) -> Option<bool> {
        match self.0 {
            ProfilePosition::Gps { lat_lon_dege7, .. } => Some(lat_lon_dege7),
            ProfilePosition::Ned { .. } => None,
        }
    }

    #[getter]
    fn alt_mm(&self) -> Option<bool> {
        match self.0 {
            ProfilePosition::Gps { alt_mm, .. } => Some(alt_mm),
            ProfilePosition::Ned { .. } => None,
        }
    }

    #[getter]
    fn alt_offset_m(&self) -> Option<f64> {
        match self.0 {
            ProfilePosition::Gps { alt_offset_m, .. } => Some(alt_offset_m),
            ProfilePosition::Ned { .. } => None,
        }
    }
}

#[pymethods]
impl ProfileOrientationPy {
    #[getter]
    fn kind(&self) -> &'static str {
        match self.0 {
            ProfileOrientation::Static => "static",
            ProfileOrientation::Euler { .. } => "euler",
            ProfileOrientation::Quat { .. } => "quat",
        }
    }

    #[getter]
    fn roll(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfileOrientation::Euler { roll, .. } => Some(profile_field(roll)),
            _ => None,
        }
    }

    #[getter]
    fn pitch(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfileOrientation::Euler { pitch, .. } => Some(profile_field(pitch)),
            _ => None,
        }
    }

    #[getter]
    fn yaw(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfileOrientation::Euler { yaw, .. } => Some(profile_field(yaw)),
            _ => None,
        }
    }

    #[getter]
    fn degrees(&self) -> Option<bool> {
        match self.0 {
            ProfileOrientation::Euler { degrees, .. } => Some(degrees),
            _ => None,
        }
    }

    #[getter]
    fn w(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfileOrientation::Quat { w, .. } => Some(profile_field(w)),
            _ => None,
        }
    }

    #[getter]
    fn x(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfileOrientation::Quat { x, .. } => Some(profile_field(x)),
            _ => None,
        }
    }

    #[getter]
    fn y(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfileOrientation::Quat { y, .. } => Some(profile_field(y)),
            _ => None,
        }
    }

    #[getter]
    fn z(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfileOrientation::Quat { z, .. } => Some(profile_field(z)),
            _ => None,
        }
    }
}

#[pymethods]
impl ProfileNedReferencePy {
    #[getter]
    fn kind(&self) -> &'static str {
        match self.0 {
            ProfileNedReference::Manual { .. } => "manual",
            ProfileNedReference::Fields { .. } => "fields",
        }
    }

    #[getter]
    fn lat_deg(&self) -> Option<f64> {
        match self.0 {
            ProfileNedReference::Manual { lat_deg, .. } => Some(lat_deg),
            ProfileNedReference::Fields { .. } => None,
        }
    }

    #[getter]
    fn lon_deg(&self) -> Option<f64> {
        match self.0 {
            ProfileNedReference::Manual { lon_deg, .. } => Some(lon_deg),
            ProfileNedReference::Fields { .. } => None,
        }
    }

    #[getter]
    fn alt_m(&self) -> Option<f64> {
        match self.0 {
            ProfileNedReference::Manual { alt_m, .. } => Some(alt_m),
            ProfileNedReference::Fields { .. } => None,
        }
    }

    #[getter]
    fn lat(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfileNedReference::Fields { lat, .. } => Some(profile_field(lat)),
            ProfileNedReference::Manual { .. } => None,
        }
    }

    #[getter]
    fn lon(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfileNedReference::Fields { lon, .. } => Some(profile_field(lon)),
            ProfileNedReference::Manual { .. } => None,
        }
    }

    #[getter]
    fn alt(&self) -> Option<ProfileFieldPy> {
        match &self.0 {
            ProfileNedReference::Fields { alt, .. } => Some(profile_field(alt)),
            ProfileNedReference::Manual { .. } => None,
        }
    }
}

fn request_profile(py: Python<'_>, request: VehicleProfileRequest) -> PyResult<ControlResponse> {
    call_immediate_detached(py, ControlRequest::VehicleProfiles(request))
        .map_err(control_call_error)
}

fn request_profile_unit(py: Python<'_>, request: VehicleProfileRequest) -> PyResult<()> {
    match request_profile(py, request)? {
        ControlResponse::Unit => Ok(()),
        other => Err(profile_response_error(other)),
    }
}

fn profile_from_response(response: ControlResponse) -> PyResult<VehicleProfilePy> {
    match response {
        ControlResponse::VehicleProfile(info) => Ok(VehicleProfilePy { info }),
        other => Err(profile_response_error(other)),
    }
}

fn profile_response_error(response: ControlResponse) -> PyErr {
    PyRuntimeError::new_err(format!("vehicle profile request returned {response:?}"))
}

fn validate_profile_name(name: &str) -> PyResult<String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(PyValueError::new_err(
            "vehicle profile name must not be empty or contain path separators/traversal",
        ));
    }
    Ok(name.to_owned())
}

fn profile_field(field: &ProfileFieldRef) -> ProfileFieldPy {
    ProfileFieldPy(field.clone())
}
