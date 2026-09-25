use delog_api::control::{
    AccessMode, AnnotationFilter, AnnotationRequest, ControlPrincipal, ControlRequest,
    GenerationRequest, LayoutRequest, MarkerFilter, MarkerInfo, MarkerOrigin, MarkerRequest,
    PlaybackRequest, PlotRequest, ResourceOwner, TraceRequest, VehicleFilter,
    VehicleProfileRequest, VehicleRequest, WorkspaceRequest,
};
use delog_api::{Error, Result};

use super::control_service::AppControl;
use crate::plotting::annotations::Annotation;
use crate::plotting::plot::PlotPane;
use crate::scene3d::vehicle::VehicleConfig;
use crate::shell::workspace::{Pane, Workspace};

pub fn authorize_and_stamp(
    control: &mut AppControl<'_>,
    principal: &ControlPrincipal,
    request: ControlRequest,
) -> Result<ControlRequest> {
    let owner = principal.owner.clone();
    let full = principal.access == AccessMode::Full;
    match request {
        ControlRequest::Guarded { guard, request } => Ok(ControlRequest::Guarded {
            guard,
            request: Box::new(authorize_and_stamp(control, principal, *request)?),
        }),
        ControlRequest::Markers(request) => {
            authorize_markers(control, principal, request).map(ControlRequest::Markers)
        }
        ControlRequest::Plots(request @ (PlotRequest::List { .. } | PlotRequest::Focused)) => {
            Ok(ControlRequest::Plots(request))
        }
        ControlRequest::Traces(request) => {
            authorize_traces(control, principal, request).map(ControlRequest::Traces)
        }
        ControlRequest::Annotations(request) => {
            authorize_annotations(control, principal, request).map(ControlRequest::Annotations)
        }
        ControlRequest::Generation(request) => match request {
            GenerationRequest::RemoveOwned { .. } => {
                if !full {
                    ensure_safe_remove_owned(control, principal)?;
                }
                Ok(ControlRequest::Generation(GenerationRequest::RemoveOwned {
                    owner: owner.name,
                }))
            }
            GenerationRequest::Commit { .. } | GenerationRequest::Rollback { .. } => Err(
                Error::forbidden("external callers cannot commit or roll back generations"),
            ),
        },
        ControlRequest::Workspace(request) => {
            let request = match request {
                WorkspaceRequest::AddPlot {
                    window, direction, ..
                } => WorkspaceRequest::AddPlot {
                    window,
                    direction,
                    owner: Some(owner),
                },
                WorkspaceRequest::Split {
                    window,
                    tile,
                    direction,
                    ..
                } => WorkspaceRequest::Split {
                    window,
                    tile,
                    direction,
                    owner: Some(owner),
                },
                WorkspaceRequest::OpenWindow { title, .. } => WorkspaceRequest::OpenWindow {
                    title,
                    owner: Some(owner),
                },
                request @ (WorkspaceRequest::ListWindows | WorkspaceRequest::GetState) => request,
                request @ (WorkspaceRequest::Equalize { .. }
                | WorkspaceRequest::ShowScene { .. }) => require_full(full, request)?,
                request @ WorkspaceRequest::Close { window, tile } => {
                    if !full {
                        let pane = plot_pane(control, window, tile)?;
                        ensure_owned(pane.owner.as_ref(), principal)?;
                        ensure_no_foreign_content(pane, principal)?;
                    }
                    request
                }
            };
            Ok(ControlRequest::Workspace(request))
        }
        ControlRequest::Playback(PlaybackRequest::Get) => {
            Ok(ControlRequest::Playback(PlaybackRequest::Get))
        }
        ControlRequest::Playback(request) => {
            require_full(full, request).map(ControlRequest::Playback)
        }
        ControlRequest::Vehicles(request) => authorize_vehicles(control, principal, *request)
            .map(|request| ControlRequest::Vehicles(Box::new(request))),
        ControlRequest::VehicleProfiles(request) => match request {
            VehicleProfileRequest::List => Ok(ControlRequest::VehicleProfiles(request)),
            request => require_full(full, request).map(ControlRequest::VehicleProfiles),
        },
        ControlRequest::Layouts(request) => match request {
            LayoutRequest::List | LayoutRequest::Current => Ok(ControlRequest::Layouts(request)),
            request => require_full(full, request).map(ControlRequest::Layouts),
        },
        ControlRequest::Batch(_) => Err(Error::invalid_input(
            "external batches must be authorized and applied through the control host",
        )),
    }
}

fn require_full<T>(full: bool, value: T) -> Result<T> {
    if full {
        Ok(value)
    } else {
        Err(Error::forbidden(
            "external safe mode does not allow this operation",
        ))
    }
}

fn ensure_owned(actual: Option<&ResourceOwner>, principal: &ControlPrincipal) -> Result<()> {
    if actual.is_some_and(|owner| owner.name == principal.owner.name) {
        Ok(())
    } else {
        Err(Error::forbidden(
            "external safe mode can modify only resources owned by this client",
        ))
    }
}

fn ensure_owner_name(actual: Option<&str>, principal: &ControlPrincipal) -> Result<()> {
    if actual == Some(principal.owner.name.as_str()) {
        Ok(())
    } else {
        Err(Error::forbidden(
            "external safe mode can modify only resources owned by this client",
        ))
    }
}

fn is_foreign_owner(actual: Option<&ResourceOwner>, principal: &ControlPrincipal) -> bool {
    actual.is_none_or(|owner| owner.name != principal.owner.name)
}

fn has_foreign_content(pane: &PlotPane, principal: &ControlPrincipal) -> bool {
    pane.traces
        .iter()
        .any(|trace| is_foreign_owner(trace.owner.as_ref(), principal))
        || pane
            .ghosts
            .iter()
            .any(|ghost| is_foreign_owner(ghost.owner.as_ref(), principal))
        || pane
            .annotations
            .items()
            .iter()
            .any(|annotation| is_foreign_owner(annotation.owner.as_ref(), principal))
}

fn ensure_no_foreign_content(pane: &PlotPane, principal: &ControlPrincipal) -> Result<()> {
    if has_foreign_content(pane, principal) {
        Err(Error::forbidden(
            "external safe mode cannot remove a plot containing content this client does not own",
        ))
    } else {
        Ok(())
    }
}

fn ensure_safe_remove_owned(control: &AppControl<'_>, principal: &ControlPrincipal) -> Result<()> {
    let owned_pane = |pane: &PlotPane| {
        pane.owner
            .as_ref()
            .is_some_and(|owner| owner.name == principal.owner.name)
    };
    for pane in control.workspace.plot_panes().chain(
        control
            .windows
            .iter()
            .flat_map(|window| window.workspace.plot_panes()),
    ) {
        if owned_pane(pane) {
            ensure_no_foreign_content(pane, principal)?;
        }
    }
    for window in control.windows.iter().filter(|window| {
        window
            .owner
            .as_ref()
            .is_some_and(|owner| owner.name == principal.owner.name)
    }) {
        if window
            .workspace
            .plot_panes()
            .any(|pane| !owned_pane(pane) || has_foreign_content(pane, principal))
        {
            return Err(Error::forbidden(
                "external safe mode cannot remove a window containing a plot or content this client does not own",
            ));
        }
    }
    Ok(())
}

fn workspace<'a>(control: &'a AppControl<'_>, window: u64) -> Result<&'a Workspace> {
    if window == 0 {
        Ok(control.workspace)
    } else {
        control
            .windows
            .iter()
            .find(|candidate| candidate.id.0 == window)
            .map(|window| &window.workspace)
            .ok_or_else(|| Error::stale_handle(format!("window {window} is gone")))
    }
}

fn plot_pane<'a>(control: &'a AppControl<'_>, window: u64, tile: u64) -> Result<&'a PlotPane> {
    match workspace(control, window)?
        .tree
        .tiles
        .get(egui_tiles::TileId(tile))
    {
        Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) => Ok(pane),
        _ => Err(Error::stale_handle(format!(
            "plot {tile} in window {window} is gone"
        ))),
    }
}

fn authorize_traces(
    control: &AppControl<'_>,
    principal: &ControlPrincipal,
    request: TraceRequest,
) -> Result<TraceRequest> {
    let full = principal.access == AccessMode::Full;
    match request {
        request @ TraceRequest::List { .. } => Ok(request),
        TraceRequest::Add {
            window,
            tile,
            field_id,
            field,
            color,
            width_px,
            mode,
            ..
        } => Ok(TraceRequest::Add {
            window,
            tile,
            field_id,
            field,
            color,
            width_px,
            mode,
            owner: Some(principal.owner.clone()),
        }),
        TraceRequest::AddReturning {
            window,
            tile,
            field_id,
            field,
            color,
            width_px,
            mode,
            ..
        } => Ok(TraceRequest::AddReturning {
            window,
            tile,
            field_id,
            field,
            color,
            width_px,
            mode,
            owner: Some(principal.owner.clone()),
        }),
        request @ TraceRequest::Remove {
            window,
            tile,
            index,
            field_id,
            ..
        } => {
            if !full {
                let pane = plot_pane(control, window, tile)?;
                let targets: Vec<_> = match (index, field_id) {
                    (Some(index), _) => pane.traces.get(index).into_iter().collect(),
                    (None, Some(field)) => pane
                        .traces
                        .iter()
                        .filter(|trace| trace.field == field)
                        .collect(),
                    (None, None) => Vec::new(),
                };
                for trace in targets {
                    ensure_owned(trace.owner.as_ref(), principal)?;
                }
            }
            Ok(request)
        }
        TraceRequest::Clear { window, tile } if !full => Ok(TraceRequest::ClearOwned {
            window,
            tile,
            owner: principal.owner.name.clone(),
        }),
        request @ TraceRequest::Clear { .. } => Ok(request),
        TraceRequest::ClearOwned {
            window,
            tile,
            owner,
        } => Ok(TraceRequest::ClearOwned {
            window,
            tile,
            owner: if full {
                owner
            } else {
                principal.owner.name.clone()
            },
        }),
        request @ TraceRequest::Set {
            window,
            tile,
            index,
            field_id,
            ..
        } => {
            if !full {
                let trace = plot_pane(control, window, tile)?
                    .traces
                    .get(index)
                    .ok_or_else(|| Error::stale_handle("trace handle is stale"))?;
                ensure_owned(trace.owner.as_ref(), principal)?;
                if trace.field != field_id {
                    return Err(Error::stale_handle("trace handle is stale"));
                }
            }
            Ok(request)
        }
    }
}

fn authorize_markers(
    control: &AppControl<'_>,
    principal: &ControlPrincipal,
    request: MarkerRequest,
) -> Result<MarkerRequest> {
    let full = principal.access == AccessMode::Full;
    match request {
        MarkerRequest::List => Ok(MarkerRequest::List),
        MarkerRequest::Append { markers, .. } => Ok(MarkerRequest::Append {
            owner: principal.owner.name.clone(),
            generation: principal.owner.generation,
            markers,
        }),
        MarkerRequest::AppendReturning { marker, .. } => Ok(MarkerRequest::AppendReturning {
            owner: principal.owner.name.clone(),
            generation: principal.owner.generation,
            marker,
        }),
        MarkerRequest::RemoveOwned { .. } => Ok(MarkerRequest::RemoveOwned {
            owner: principal.owner.name.clone(),
        }),
        request @ MarkerRequest::Set { id, .. } => {
            if !full {
                let marker = control
                    .markers
                    .marker_infos()
                    .into_iter()
                    .find(|marker| marker.id == id)
                    .ok_or_else(|| Error::stale_handle(format!("marker {id} is gone")))?;
                ensure_owner_name(marker.owner.as_deref(), principal)?;
            }
            Ok(request)
        }
        MarkerRequest::Remove(MarkerFilter::All) if !full => Ok(MarkerRequest::Remove(
            MarkerFilter::Owner(principal.owner.name.clone()),
        )),
        MarkerRequest::Remove(MarkerFilter::Owner(owner)) if !full => {
            let _ = owner;
            Ok(MarkerRequest::Remove(MarkerFilter::Owner(
                principal.owner.name.clone(),
            )))
        }
        MarkerRequest::Remove(filter) => {
            if !full {
                for marker in control
                    .markers
                    .marker_infos()
                    .into_iter()
                    .filter(|marker| marker_matches(marker, &filter))
                {
                    ensure_owner_name(marker.owner.as_deref(), principal)?;
                }
            }
            Ok(MarkerRequest::Remove(filter))
        }
    }
}

fn marker_matches(marker: &MarkerInfo, filter: &MarkerFilter) -> bool {
    match filter {
        MarkerFilter::Id(id) => marker.id == *id,
        MarkerFilter::Index(index) => marker.index == *index,
        MarkerFilter::Owner(owner) => marker.owner.as_deref() == Some(owner),
        MarkerFilter::Origin(origin) => marker.origin == *origin,
        MarkerFilter::ScriptLabel(label) => {
            marker.origin == MarkerOrigin::Script && marker.label == *label
        }
        MarkerFilter::ScriptTimeRange { after, before } => {
            marker.origin == MarkerOrigin::Script
                && after.is_none_or(|after| marker.t_us >= after)
                && before.is_none_or(|before| marker.t_us <= before)
        }
        MarkerFilter::ScriptAll => marker.origin == MarkerOrigin::Script,
        MarkerFilter::All => true,
    }
}

fn authorize_annotations(
    control: &AppControl<'_>,
    principal: &ControlPrincipal,
    request: AnnotationRequest,
) -> Result<AnnotationRequest> {
    let full = principal.access == AccessMode::Full;
    match request {
        request @ AnnotationRequest::List { .. } => Ok(request),
        AnnotationRequest::Add {
            window,
            tile,
            geometry,
            label,
            style,
            ..
        } => Ok(AnnotationRequest::Add {
            window,
            tile,
            geometry,
            label,
            style,
            owner: Some(principal.owner.clone()),
        }),
        request @ AnnotationRequest::Set {
            window, tile, id, ..
        } => {
            if !full {
                let annotation = plot_pane(control, window, tile)?
                    .annotations
                    .items()
                    .iter()
                    .find(|annotation| annotation.id == id)
                    .ok_or_else(|| Error::stale_handle(format!("annotation {id} is gone")))?;
                ensure_owned(annotation.owner.as_ref(), principal)?;
            }
            Ok(request)
        }
        AnnotationRequest::Remove {
            target,
            filter: AnnotationFilter::All,
        } if !full => Ok(AnnotationRequest::Remove {
            target,
            filter: AnnotationFilter::Owner(principal.owner.name.clone()),
        }),
        AnnotationRequest::Remove {
            target,
            filter: AnnotationFilter::Owner(_),
        } if !full => Ok(AnnotationRequest::Remove {
            target,
            filter: AnnotationFilter::Owner(principal.owner.name.clone()),
        }),
        AnnotationRequest::Remove { target, filter } => {
            if !full {
                visit_annotations(control, target, |annotation, index| {
                    if annotation_matches(annotation, index, &filter) {
                        ensure_owned(annotation.owner.as_ref(), principal)?;
                    }
                    Ok(())
                })?;
            }
            Ok(AnnotationRequest::Remove { target, filter })
        }
    }
}

fn visit_annotations(
    control: &AppControl<'_>,
    target: Option<(u64, u64)>,
    mut visit: impl FnMut(&Annotation, usize) -> Result<()>,
) -> Result<()> {
    if let Some((window, tile)) = target {
        for (index, annotation) in plot_pane(control, window, tile)?
            .annotations
            .items()
            .iter()
            .enumerate()
        {
            visit(annotation, index)?;
        }
        return Ok(());
    }
    for pane in control.workspace.plot_panes() {
        for (index, annotation) in pane.annotations.items().iter().enumerate() {
            visit(annotation, index)?;
        }
    }
    for window in control.windows.iter() {
        for pane in window.workspace.plot_panes() {
            for (index, annotation) in pane.annotations.items().iter().enumerate() {
                visit(annotation, index)?;
            }
        }
    }
    Ok(())
}

fn annotation_matches(annotation: &Annotation, index: usize, filter: &AnnotationFilter) -> bool {
    match filter {
        AnnotationFilter::Index(candidate) => index == *candidate,
        AnnotationFilter::Id(id) => annotation.id == *id,
        AnnotationFilter::Kind(kind) => annotation.geom.kind().to_script() == *kind,
        AnnotationFilter::Label(label) => annotation.label == *label,
        AnnotationFilter::Owner(owner) => {
            annotation.owner.as_ref().map(|owner| owner.name.as_str()) == Some(owner)
        }
        AnnotationFilter::All => true,
    }
}

fn authorize_vehicles(
    control: &AppControl<'_>,
    principal: &ControlPrincipal,
    request: VehicleRequest,
) -> Result<VehicleRequest> {
    let full = principal.access == AccessMode::Full;
    match request {
        VehicleRequest::List => Ok(VehicleRequest::List),
        VehicleRequest::Add(mut spec) => {
            spec.owner = Some(principal.owner.clone());
            Ok(VehicleRequest::Add(spec))
        }
        request @ VehicleRequest::Set { id, .. } => {
            if !full {
                let vehicle = control
                    .vehicles
                    .iter()
                    .find(|vehicle| vehicle.runtime.id == id)
                    .ok_or_else(|| Error::stale_handle(format!("vehicle {id} is gone")))?;
                ensure_owned(vehicle.runtime.owner.as_ref(), principal)?;
            }
            Ok(request)
        }
        VehicleRequest::Remove(VehicleFilter::All) if !full => Ok(VehicleRequest::Remove(
            VehicleFilter::Owner(principal.owner.name.clone()),
        )),
        VehicleRequest::Remove(VehicleFilter::Owner(_)) if !full => Ok(VehicleRequest::Remove(
            VehicleFilter::Owner(principal.owner.name.clone()),
        )),
        VehicleRequest::Remove(filter) => {
            if !full {
                for (index, vehicle) in control.vehicles.iter().enumerate() {
                    if vehicle_matches(vehicle, index, &filter) {
                        ensure_owned(vehicle.runtime.owner.as_ref(), principal)?;
                    }
                }
            }
            Ok(VehicleRequest::Remove(filter))
        }
    }
}

fn vehicle_matches(vehicle: &VehicleConfig, index: usize, filter: &VehicleFilter) -> bool {
    match filter {
        VehicleFilter::Id(id) => vehicle.runtime.id == *id,
        VehicleFilter::Index(candidate) => index == *candidate,
        VehicleFilter::Label(label) => vehicle.label == *label,
        VehicleFilter::Source(source) => vehicle.source == *source,
        VehicleFilter::Owner(owner) => {
            vehicle
                .runtime
                .owner
                .as_ref()
                .map(|owner| owner.name.as_str())
                == Some(owner)
        }
        VehicleFilter::All => true,
    }
}

#[cfg(test)]
mod tests;
