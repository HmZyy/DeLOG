use delog_api::control::{ControlResponse, VehicleProfileRequest};
use delog_api::{Error, Result};

use super::{AppControl, mark_vehicles_changed, validate_source, vehicle_index, vehicle_info};

pub(crate) fn apply_vehicle_profile_request(
    control: &mut AppControl<'_>,
    request: VehicleProfileRequest,
) -> Result<ControlResponse> {
    use crate::scene3d::vehicle::VehicleOwner;
    use crate::session::vehicle_profiles::VehicleProfileDoc;

    let library = control.vehicle_profiles.ok_or_else(|| {
        Error::execution(
            "vehicle profile library is unavailable because the app has no configuration directory",
        )
    })?;
    match request {
        VehicleProfileRequest::List => library
            .list()
            .map(ControlResponse::Names)
            .map_err(|error| profile_io_error("list", None, library, error)),
        VehicleProfileRequest::Save { name, vehicle_id } => {
            crate::scene3d::vehicle::assign_runtime_ids(control.vehicles, control.next_vehicle_id)
                .map_err(Error::internal)?;
            let vehicle = &control.vehicles[vehicle_index(control.vehicles, vehicle_id)?];
            let doc = VehicleProfileDoc::from_config(&name, vehicle, control.snapshot)
                .ok_or_else(|| {
                    Error::stale_handle(format!(
                        "vehicle {vehicle_id} cannot be saved as profile '{name}' because its fields do not resolve"
                    ))
                })?;
            library
                .save(&name, &doc)
                .map_err(|error| profile_io_error("save", Some(&name), library, error))?;
            Ok(ControlResponse::Unit)
        }
        VehicleProfileRequest::Load { name } => {
            let doc = library
                .load(&name)
                .map_err(|error| profile_io_error("load", Some(&name), library, error))?;
            Ok(ControlResponse::VehicleProfile(Box::new(
                doc.to_script_info(),
            )))
        }
        VehicleProfileRequest::Apply {
            name,
            source_id,
            source,
            owner,
        } => {
            validate_source(control.snapshot, source_id, &source)?;
            let doc = library
                .load(&name)
                .map_err(|error| profile_io_error("apply", Some(&name), library, error))?;
            let mut vehicle = doc
                .to_config_for_source(control.snapshot, source_id)
                .ok_or_else(|| {
                    Error::not_found(format!(
                        "profile '{name}' does not resolve for source '{source}'"
                    ))
                })?;
            vehicle.runtime.owner = owner.map(|owner| VehicleOwner {
                name: owner.name,
                generation: owner.generation,
            });

            crate::scene3d::vehicle::assign_runtime_ids(control.vehicles, control.next_vehicle_id)
                .map_err(Error::internal)?;
            crate::scene3d::vehicle::assign_runtime_id(&mut vehicle, control.next_vehicle_id)
                .map_err(Error::internal)?;
            let index = control.vehicles.len();
            let info = vehicle_info(&vehicle, index, control.snapshot)?;
            control.vehicles.push(vehicle);
            mark_vehicles_changed(control);
            Ok(ControlResponse::Vehicles(vec![info]))
        }
        VehicleProfileRequest::Delete { name } => {
            library
                .delete(&name)
                .map_err(|error| profile_io_error("delete", Some(&name), library, error))?;
            Ok(ControlResponse::Unit)
        }
    }
}

fn profile_io_error(
    operation: &str,
    name: Option<&str>,
    library: &crate::session::vehicle_profiles::VehicleProfileLibrary,
    error: std::io::Error,
) -> Error {
    let invalid = if error.kind() == std::io::ErrorKind::InvalidData {
        "invalid profile: "
    } else {
        ""
    };
    let message = match name {
        Some(name) => format!(
            "could not {operation} vehicle profile '{name}' in '{}': {invalid}{error}",
            library.dir().display()
        ),
        None => format!(
            "could not {operation} vehicle profiles in '{}': {invalid}{error}",
            library.dir().display()
        ),
    };
    match error.kind() {
        std::io::ErrorKind::NotFound
            if name.is_some() && matches!(operation, "load" | "apply" | "delete") =>
        {
            Error::not_found(message)
        }
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => {
            Error::invalid_input(message)
        }
        _ => Error::execution(message),
    }
}
