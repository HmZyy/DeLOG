mod annotations;
mod authorization;
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
pub use authorization::{
    AccessMode, AuthorizedControlHost, CancelCheck, ControlCall, ControlPrincipal,
};
pub use layouts::{
    LayoutFieldIssue, LayoutRequest, LoadReport, validate_layout_name, validate_layout_path,
};
pub use markers::{MarkerFilter, MarkerInfo, MarkerOrigin, MarkerPatch, MarkerRequest};
pub use owner::{GenerationRequest, PlotContext, ResourceOwner, ScriptOwner};
pub use plots::{PlotInfo, PlotRequest};
pub use traces::{TraceInfo, TraceMode, TraceRequest};
pub use vehicles::{
    ProfileFieldRef, ProfileNedReference, ProfileOrientation, ProfilePosition,
    ResolvedVehicleField, VehicleFilter, VehicleInfo, VehicleModel, VehicleNedReference,
    VehicleOrientation, VehiclePatch, VehiclePosition, VehicleProfileInfo, VehicleProfileRequest,
    VehicleRequest, VehicleSpec, validate_profile_name,
};
pub use workspace::{
    PlaybackInfo, PlaybackRequest, SplitDirection, WindowInfo, WorkspaceInfo, WorkspaceRequest,
};

use crate::{Error, Result};

const WRONG_RESPONSE: &str = "the DeLOG window answered with the wrong kind of result";

/// Immutable identities checked on the UI thread after external authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceGuard {
    Plot {
        window: u64,
        tile: u64,
        instance_id: u64,
    },
    Trace {
        window: u64,
        tile: u64,
        plot_instance_id: u64,
        index: usize,
        trace_instance_id: u64,
    },
    Annotation {
        window: u64,
        tile: u64,
        plot_instance_id: u64,
        id: u64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlRequest {
    Guarded {
        guard: ResourceGuard,
        request: Box<ControlRequest>,
    },
    Markers(MarkerRequest),
    Plots(PlotRequest),
    Traces(TraceRequest),
    Annotations(AnnotationRequest),
    Generation(GenerationRequest),
    Workspace(WorkspaceRequest),
    Playback(PlaybackRequest),
    Vehicles(Box<VehicleRequest>),
    VehicleProfiles(VehicleProfileRequest),
    Layouts(LayoutRequest),
    Batch(Vec<ControlRequest>),
}

impl ControlRequest {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Guarded { guard: _, request } => {
                if matches!(request.as_ref(), Self::Guarded { .. } | Self::Batch(_)) {
                    return Err(Error::invalid_input(
                        "nested guarded and batch requests are not supported",
                    ));
                }
                request.validate()
            }
            Self::Markers(request) => request.validate(),
            Self::Plots(PlotRequest::List { window: _ } | PlotRequest::Focused) => Ok(()),
            Self::Traces(request) => request.validate(),
            Self::Annotations(request) => request.validate(),
            Self::Generation(request) => request.validate(),
            Self::Workspace(request) => request.validate(),
            Self::Playback(request) => request.validate(),
            Self::Vehicles(request) => request.validate(),
            Self::VehicleProfiles(request) => request.validate(),
            Self::Layouts(request) => request.validate(),
            Self::Batch(requests) => {
                for (index, request) in requests.iter().enumerate() {
                    if !request_is_batchable(request) {
                        return Err(Error::invalid_input(format!(
                            "batch request {index} is not batchable"
                        )));
                    }
                    request
                        .validate()
                        .map_err(|error| error.with_context(format!("batch request {index}")))?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlResponse {
    Unit,
    Trace(TraceInfo),
    Marker(MarkerInfo),
    Windows(Vec<WindowInfo>),
    Workspace(WorkspaceInfo),
    Playback(PlaybackInfo),
    Plots(Vec<PlotInfo>),
    Traces(Vec<TraceInfo>),
    Annotations(Vec<AnnotationInfo>),
    Window(WindowInfo),
    Vehicles(Vec<VehicleInfo>),
    VehicleProfile(Box<VehicleProfileInfo>),
    Names(Vec<String>),
    Markers(Vec<MarkerInfo>),
    Layout(String),
    LoadReport(LoadReport),
    Removed(usize),
}

impl ControlResponse {
    pub fn into_trace(self) -> Result<TraceInfo> {
        match self {
            Self::Trace(value) => Ok(value),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_marker(self) -> Result<MarkerInfo> {
        match self {
            Self::Marker(value) => Ok(value),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }
    pub fn into_windows(self) -> Result<Vec<WindowInfo>> {
        match self {
            Self::Windows(value) => Ok(value),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_workspace(self) -> Result<WorkspaceInfo> {
        match self {
            Self::Workspace(value) => Ok(value),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }

    pub fn into_playback(self) -> Result<PlaybackInfo> {
        match self {
            Self::Playback(value) => Ok(value),
            _ => Err(Error::protocol(WRONG_RESPONSE)),
        }
    }
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
        self.into_window_info().map(|window| window.id)
    }

    pub fn into_window_info(self) -> Result<WindowInfo> {
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
            Self::VehicleProfile(profile) => Ok(*profile),
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

    pub fn into_removed(self) -> Result<usize> {
        match self {
            Self::Removed(count) => Ok(count),
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
        ControlRequest::Guarded { .. } => false,
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
                | WorkspaceRequest::Equalize { .. }
                | WorkspaceRequest::ShowScene { .. }
        ),
        ControlRequest::Playback(request) => matches!(request, PlaybackRequest::Set { .. }),
        ControlRequest::Vehicles(request) => {
            matches!(
                request.as_ref(),
                VehicleRequest::Set { .. } | VehicleRequest::Remove(_)
            )
        }
        ControlRequest::VehicleProfiles(_)
        | ControlRequest::Layouts(_)
        | ControlRequest::Batch(_) => false,
    }
}
