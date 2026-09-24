mod annotations;
mod layouts;
mod markers;
mod owner;
mod plots;
mod traces;
mod vehicles;
mod workspace;

pub use annotations::{
    AnnotationFilter, AnnotationGeometry, AnnotationInfo, AnnotationKind, AnnotationRequest,
    AnnotationStylePatch,
};
pub use layouts::{
    LayoutFieldIssue, LayoutRequest, LoadReport, validate_layout_name, validate_layout_path,
};
pub use markers::{MarkerFilter, MarkerInfo, MarkerOrigin, MarkerPatch, MarkerRequest};
pub use owner::{GenerationRequest, PlotContext, ScriptOwner};
pub use plots::{PlotInfo, PlotRequest};
pub use traces::{TraceInfo, TraceMode, TraceRequest};
pub use vehicles::{
    ProfileFieldRef, ProfileNedReference, ProfileOrientation, ProfilePosition,
    ResolvedVehicleField, VehicleFilter, VehicleInfo, VehicleModel, VehicleNedReference,
    VehicleOrientation, VehiclePatch, VehiclePosition, VehicleProfileInfo, VehicleProfileRequest,
    VehicleRequest, VehicleSpec, validate_profile_name,
};
pub use workspace::{PlaybackRequest, SplitDirection, WorkspaceRequest};

use crate::{Error, Result};

const WRONG_RESPONSE: &str = "the DeLOG window answered with the wrong kind of result";

#[derive(Debug, Clone, PartialEq)]
pub enum ControlRequest {
    Markers(MarkerRequest),
    Plots(PlotRequest),
    Traces(TraceRequest),
    Annotations(AnnotationRequest),
    Generation(GenerationRequest),
    Workspace(WorkspaceRequest),
    Playback(PlaybackRequest),
    Vehicles(VehicleRequest),
    VehicleProfiles(VehicleProfileRequest),
    Layouts(LayoutRequest),
    Batch(Vec<ControlRequest>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlResponse {
    Unit,
    Plots(Vec<PlotInfo>),
    Traces(Vec<TraceInfo>),
    Annotations(Vec<AnnotationInfo>),
    Window(u64),
    Vehicles(Vec<VehicleInfo>),
    VehicleProfile(VehicleProfileInfo),
    Names(Vec<String>),
    Markers(Vec<MarkerInfo>),
    Layout(String),
    LoadReport(LoadReport),
}

impl ControlResponse {
    pub fn into_unit(self) -> Result<()> {
        match self {
            Self::Unit => Ok(()),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_plots(self) -> Result<Vec<PlotInfo>> {
        match self {
            Self::Plots(plots) => Ok(plots),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_traces(self) -> Result<Vec<TraceInfo>> {
        match self {
            Self::Traces(traces) => Ok(traces),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_annotations(self) -> Result<Vec<AnnotationInfo>> {
        match self {
            Self::Annotations(annotations) => Ok(annotations),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_window(self) -> Result<u64> {
        match self {
            Self::Window(window) => Ok(window),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_vehicles(self) -> Result<Vec<VehicleInfo>> {
        match self {
            Self::Vehicles(vehicles) => Ok(vehicles),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_vehicle_profile(self) -> Result<VehicleProfileInfo> {
        match self {
            Self::VehicleProfile(profile) => Ok(profile),
            response => Err(Error::protocol(format!(
                "vehicle profile request returned {response:?}"
            ))),
        }
    }

    pub fn into_names(self) -> Result<Vec<String>> {
        match self {
            Self::Names(names) => Ok(names),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_markers(self) -> Result<Vec<MarkerInfo>> {
        match self {
            Self::Markers(markers) => Ok(markers),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_layout(self) -> Result<String> {
        match self {
            Self::Layout(layout) => Ok(layout),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_load_report(self) -> Result<LoadReport> {
        match self {
            Self::LoadReport(report) => Ok(report),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }
}

pub trait ControlHost: Send + Sync {
    fn call(&self, request: ControlRequest) -> Result<ControlResponse>;
}

/// Whether a request can be applied without needing a response payload or
/// creating a handle that subsequent statements depend on.
pub fn request_is_batchable(request: &ControlRequest) -> bool {
    match request {
        ControlRequest::Markers(request) => matches!(
            request,
            MarkerRequest::Append { .. }
                | MarkerRequest::RemoveOwned { .. }
                | MarkerRequest::Set { .. }
                | MarkerRequest::Remove(_)
        ),
        ControlRequest::Plots(_) => false,
        ControlRequest::Traces(request) => !matches!(request, TraceRequest::List { .. }),
        ControlRequest::Annotations(request) => {
            matches!(
                request,
                AnnotationRequest::Remove { .. } | AnnotationRequest::Set { .. }
            )
        }
        ControlRequest::Generation(_) => true,
        ControlRequest::Workspace(request) => matches!(
            request,
            WorkspaceRequest::Close { .. }
                | WorkspaceRequest::Equalize
                | WorkspaceRequest::ShowScene { .. }
        ),
        ControlRequest::Playback(_) => true,
        ControlRequest::Vehicles(request) => {
            matches!(
                request,
                VehicleRequest::Set { .. } | VehicleRequest::Remove(_)
            )
        }
        ControlRequest::VehicleProfiles(_)
        | ControlRequest::Layouts(_)
        | ControlRequest::Batch(_) => false,
    }
}
