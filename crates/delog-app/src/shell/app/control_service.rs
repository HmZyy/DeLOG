use delog_api::control::{
    AnnotationFilter, AnnotationInfo, AnnotationRequest, ControlCall, ControlRequest,
    ControlResponse, PlaybackInfo, PlaybackRequest, PlotInfo, PlotRequest, ResourceGuard,
    ScriptOwner, SplitDirection as ScriptSplitDirection, VehicleFilter, VehicleInfo,
    VehicleNedReference as ScriptNedReference, VehicleOrientation as ScriptVehicleOrientation,
    VehiclePatch, VehiclePosition, VehicleRequest, VehicleSpec, WorkspaceInfo, WorkspaceRequest,
};
use delog_api::{Error, Result};
use delog_cache::CacheManager;
use delog_core::snapshot::StoreSnapshot;

#[cfg(test)]
use delog_api::control::TraceRequest;
#[cfg(test)]
use delog_core::identity::FieldId;

mod batch;
mod layouts;
mod profiles;
mod trace_mapping;
mod vehicle_mapping;

use batch::{apply_batch, trace_counts, unpin_removed_traces};
pub use layouts::LayoutControlEffects;
use layouts::apply_layout_request;
use profiles::apply_vehicle_profile_request;
use trace_mapping::apply_trace_request;
use vehicle_mapping::*;

use crate::plotting::markers::Markers;
use crate::plotting::plot::PlotPane;
use crate::plotting::timeline::Playback;
use crate::shell::app::control_ownership;
use crate::shell::workspace::Workspace;

pub struct AppControl<'a> {
    pub markers: &'a mut Markers,
    pub workspace: &'a mut Workspace,
    pub windows: &'a mut Vec<crate::shell::windows::ExtendedWindow>,
    pub playback: &'a mut Playback,
    pub next_window_id: &'a mut u64,
    pub caches: &'a mut CacheManager,
    pub snapshot: &'a StoreSnapshot,
    pub vehicles: &'a mut Vec<crate::scene3d::vehicle::VehicleConfig>,
    pub next_vehicle_id: &'a mut u64,
    pub vehicle_revision: &'a mut u64,
    pub traj_dirty: &'a mut bool,
    pub vehicle_profiles: Option<&'a crate::session::vehicle_profiles::VehicleProfileLibrary>,
}

pub fn apply(control: &mut AppControl<'_>, request: ControlRequest) -> Result<ControlResponse> {
    if let Err(error) = request.validate() {
        if let ControlRequest::Batch(requests) = &request {
            batch::rollback_terminal_commit_for_batch(control, requests);
        }
        return Err(error);
    }
    if let ControlRequest::Batch(requests) = request {
        return apply_batch(control, requests);
    }
    apply_one(control, request)
}

pub fn apply_call(control: &mut AppControl<'_>, call: ControlCall) -> Result<ControlResponse> {
    match call {
        ControlCall::Trusted(request) => apply(control, request),
        ControlCall::External {
            principal,
            request: ControlRequest::Batch(requests),
        } => batch::apply_authorized_batch(control, &principal, requests),
        ControlCall::External { principal, request } => {
            let request = super::control_policy::authorize_and_stamp(control, &principal, request)?;
            apply(control, request)
        }
    }
}

fn apply_one(control: &mut AppControl<'_>, request: ControlRequest) -> Result<ControlResponse> {
    match request {
        ControlRequest::Guarded { guard, request } => {
            verify_guard(control, guard)?;
            apply_one(control, *request)
        }
        ControlRequest::Markers(request) => control.markers.apply_control_request(request),
        ControlRequest::Plots(PlotRequest::List { window }) => {
            if let Some(window) = window {
                workspace_for(control, window)?;
            }
            let all = crate::shell::windows::plot_infos(control.workspace, control.windows);
            Ok(ControlResponse::Plots(match window {
                Some(id) => all.into_iter().filter(|info| info.window == id).collect(),
                None => all,
            }))
        }
        ControlRequest::Plots(PlotRequest::Focused) => {
            let focused = control.workspace.focused_plot_id().and_then(|tile| {
                control
                    .workspace
                    .plot_infos(0)
                    .into_iter()
                    .find(|info| info.tile == tile.0)
            });
            Ok(ControlResponse::Plots(focused.into_iter().collect()))
        }
        ControlRequest::Traces(request) => apply_trace_request(control, request),
        ControlRequest::Annotations(request) => apply_annotation_request(control, request),
        ControlRequest::Generation(request) => {
            let sweep = control_ownership::Sweep::from(request);
            let removed = control_ownership::apply_sweep(control, &sweep);
            Ok(match sweep {
                control_ownership::Sweep::RemoveOwned { .. } => ControlResponse::Removed(removed),
                _ => ControlResponse::Unit,
            })
        }
        ControlRequest::Workspace(request) => apply_workspace_request(control, request),
        ControlRequest::Playback(request) => apply_playback_request(control, request),
        ControlRequest::Vehicles(request) => apply_vehicle_request(control, *request),
        ControlRequest::VehicleProfiles(request) => apply_vehicle_profile_request(control, request),
        ControlRequest::Layouts(request) => apply_layout_request(control, request),
        ControlRequest::Batch(_) => Err(Error::invalid_input(
            "nested control batches are not supported",
        )),
    }
}

fn verify_guard(control: &mut AppControl<'_>, guard: ResourceGuard) -> Result<()> {
    let (window, tile, plot_id) = match guard {
        ResourceGuard::Plot {
            window,
            tile,
            instance_id,
        } => (window, tile, instance_id),
        ResourceGuard::Trace {
            window,
            tile,
            plot_instance_id,
            ..
        }
        | ResourceGuard::Annotation {
            window,
            tile,
            plot_instance_id,
            ..
        } => (window, tile, plot_instance_id),
    };
    let pane = plot_pane(control, window, tile)?;
    if pane.instance_id != plot_id {
        return Err(Error::stale_handle("plot handle is stale"));
    }
    match guard {
        ResourceGuard::Trace {
            index,
            trace_instance_id,
            ..
        } => {
            if pane.traces.get(index).map(|trace| trace.instance_id) != Some(trace_instance_id) {
                return Err(Error::stale_handle("trace handle is stale"));
            }
        }
        ResourceGuard::Annotation { id, .. } => {
            if pane.annotations.get(id).is_none() {
                return Err(Error::stale_handle("annotation handle is stale"));
            }
        }
        ResourceGuard::Plot { .. } => {}
    }
    Ok(())
}

pub(crate) fn apply_vehicle_request(
    control: &mut AppControl<'_>,
    request: VehicleRequest,
) -> Result<ControlResponse> {
    crate::scene3d::vehicle::assign_runtime_ids(control.vehicles, control.next_vehicle_id)
        .map_err(Error::internal)?;
    match request {
        VehicleRequest::List => Ok(ControlResponse::Vehicles(vehicle_infos(
            control.vehicles,
            control.snapshot,
        )?)),
        VehicleRequest::Add(spec) => {
            let mut vehicle = vehicle_from_spec(control.snapshot, spec)?;
            crate::scene3d::vehicle::assign_runtime_id(&mut vehicle, control.next_vehicle_id)
                .map_err(Error::internal)?;
            control.vehicles.push(vehicle);
            mark_vehicles_changed(control);
            let index = control.vehicles.len() - 1;
            Ok(ControlResponse::Vehicles(vec![vehicle_info(
                &control.vehicles[index],
                index,
                control.snapshot,
            )?]))
        }
        VehicleRequest::Set { id, patch } => {
            let index = vehicle_index(control.vehicles, id)?;
            let mut updated = control.vehicles[index].clone();
            apply_vehicle_patch(control.snapshot, &mut updated, patch)?;
            let info = vehicle_info(&updated, index, control.snapshot)?;
            control.vehicles[index] = updated;
            mark_vehicles_changed(control);
            Ok(ControlResponse::Vehicles(vec![info]))
        }
        VehicleRequest::Remove(filter) => {
            let before = control.vehicles.len();
            match filter {
                VehicleFilter::Index(index) => {
                    if index >= control.vehicles.len() {
                        return Err(Error::stale_handle(format!(
                            "vehicle index {index} is gone"
                        )));
                    }
                    control.vehicles.remove(index);
                }
                VehicleFilter::Id(id) => {
                    let index = vehicle_index(control.vehicles, id)?;
                    control.vehicles.remove(index);
                }
                VehicleFilter::Label(label) => {
                    control.vehicles.retain(|vehicle| vehicle.label != label);
                }
                VehicleFilter::Source(source) => {
                    control.vehicles.retain(|vehicle| vehicle.source != source);
                }
                VehicleFilter::Owner(owner) => {
                    control.vehicles.retain(|vehicle| {
                        vehicle
                            .runtime
                            .owner
                            .as_ref()
                            .map(|candidate| candidate.name.as_str())
                            != Some(owner.as_str())
                    });
                }
                VehicleFilter::All => control.vehicles.clear(),
            }
            if control.vehicles.len() != before {
                mark_vehicles_changed(control);
            }
            Ok(ControlResponse::Unit)
        }
    }
}

pub(super) fn vehicle_index(
    vehicles: &[crate::scene3d::vehicle::VehicleConfig],
    id: u64,
) -> Result<usize> {
    vehicles
        .iter()
        .position(|vehicle| vehicle.runtime.id == id)
        .ok_or_else(|| Error::stale_handle(format!("vehicle {id} is gone")))
}

pub(crate) fn mark_vehicles_changed(control: &mut AppControl<'_>) {
    *control.vehicle_revision = control.vehicle_revision.wrapping_add(1);
    *control.traj_dirty = true;
}

fn vehicle_infos(
    vehicles: &[crate::scene3d::vehicle::VehicleConfig],
    snapshot: &StoreSnapshot,
) -> Result<Vec<VehicleInfo>> {
    vehicles
        .iter()
        .enumerate()
        .map(|(index, vehicle)| vehicle_info(vehicle, index, snapshot))
        .collect()
}

fn vehicle_info(
    vehicle: &crate::scene3d::vehicle::VehicleConfig,
    index: usize,
    snapshot: &StoreSnapshot,
) -> Result<VehicleInfo> {
    use crate::scene3d::vehicle::{NedReference, OriMapping, PosMapping};

    let position = match &vehicle.pos {
        PosMapping::Ned {
            north,
            east,
            down,
            reference,
        } => VehiclePosition::Ned {
            north: resolved_field(snapshot, *north)?,
            east: resolved_field(snapshot, *east)?,
            down: resolved_field(snapshot, *down)?,
            reference: reference
                .as_ref()
                .map(|reference| -> Result<ScriptNedReference> {
                    match reference {
                        NedReference::Manual(reference) => Ok(ScriptNedReference::Manual {
                            lat_deg: reference.lat_deg,
                            lon_deg: reference.lon_deg,
                            alt_m: reference.alt_m,
                        }),
                        NedReference::Fields { lat, lon, alt } => Ok(ScriptNedReference::Fields {
                            lat: resolved_field(snapshot, *lat)?,
                            lon: resolved_field(snapshot, *lon)?,
                            alt: resolved_field(snapshot, *alt)?,
                        }),
                    }
                })
                .transpose()?,
        },
        PosMapping::Gps {
            lat,
            lon,
            alt,
            lat_lon_dege7,
            alt_mm,
            alt_offset_m,
        } => VehiclePosition::Gps {
            lat: resolved_field(snapshot, *lat)?,
            lon: resolved_field(snapshot, *lon)?,
            alt: resolved_field(snapshot, *alt)?,
            lat_lon_dege7: *lat_lon_dege7,
            alt_mm: *alt_mm,
            alt_offset_m: *alt_offset_m,
        },
    };
    let orientation = match &vehicle.ori {
        OriMapping::Static => ScriptVehicleOrientation::Static,
        OriMapping::Euler {
            roll,
            pitch,
            yaw,
            degrees,
        } => ScriptVehicleOrientation::Euler {
            roll: resolved_field(snapshot, *roll)?,
            pitch: resolved_field(snapshot, *pitch)?,
            yaw: resolved_field(snapshot, *yaw)?,
            degrees: *degrees,
        },
        OriMapping::Quat { w, x, y, z } => ScriptVehicleOrientation::Quat {
            w: resolved_field(snapshot, *w)?,
            x: resolved_field(snapshot, *x)?,
            y: resolved_field(snapshot, *y)?,
            z: resolved_field(snapshot, *z)?,
        },
    };
    let source = snapshot
        .source(vehicle.source)
        .filter(|source| !source.entry.removed)
        .ok_or_else(|| {
            Error::stale_handle(format!("vehicle source {} is gone", vehicle.source.0))
        })?;
    Ok(VehicleInfo {
        id: vehicle.runtime.id,
        index,
        spec: VehicleSpec {
            source_id: vehicle.source,
            source: source.entry.label.clone(),
            label: vehicle.label.clone(),
            show: vehicle.show,
            show_path: vehicle.show_path,
            position,
            orientation,
            model: script_model(&vehicle.model),
            color: color_to_script(vehicle.color),
            path_color: color_to_script(vehicle.path_color),
            scale: vehicle.scale,
            owner: vehicle.runtime.owner.as_ref().map(|owner| ScriptOwner {
                name: owner.name.clone(),
                generation: owner.generation,
            }),
        },
    })
}

fn vehicle_from_spec(
    snapshot: &StoreSnapshot,
    spec: VehicleSpec,
) -> Result<crate::scene3d::vehicle::VehicleConfig> {
    validate_source(snapshot, spec.source_id, &spec.source)?;
    validate_scale(spec.scale)?;
    Ok(crate::scene3d::vehicle::VehicleConfig {
        runtime: crate::scene3d::vehicle::VehicleRuntime {
            id: 0,
            owner: spec
                .owner
                .map(|owner| crate::scene3d::vehicle::VehicleOwner {
                    name: owner.name,
                    generation: owner.generation,
                }),
        },
        source: spec.source_id,
        label: spec.label,
        show: spec.show,
        show_path: spec.show_path,
        pos: app_position(snapshot, spec.source_id, spec.position)?,
        ori: app_orientation(snapshot, spec.source_id, spec.orientation)?,
        model: app_model(spec.model),
        color: app_color(spec.color, "vehicle color")?,
        path_color: app_color(spec.path_color, "vehicle path color")?,
        scale: spec.scale,
    })
}

fn apply_vehicle_patch(
    snapshot: &StoreSnapshot,
    vehicle: &mut crate::scene3d::vehicle::VehicleConfig,
    patch: VehiclePatch,
) -> Result<()> {
    if let Some(label) = patch.label {
        vehicle.label = label;
    }
    if let Some(show) = patch.show {
        vehicle.show = show;
    }
    if let Some(show_path) = patch.show_path {
        vehicle.show_path = show_path;
    }
    if let Some(position) = patch.position {
        vehicle.pos = app_position(snapshot, vehicle.source, position)?;
    }
    if let Some(orientation) = patch.orientation {
        vehicle.ori = app_orientation(snapshot, vehicle.source, orientation)?;
    }
    if let Some(model) = patch.model {
        vehicle.model = app_model(model);
    }
    if let Some(color) = patch.color {
        vehicle.color = app_color(color, "vehicle color")?;
    }
    if let Some(path_color) = patch.path_color {
        vehicle.path_color = app_color(path_color, "vehicle path color")?;
    }
    if let Some(scale) = patch.scale {
        validate_scale(scale)?;
        vehicle.scale = scale;
    }
    Ok(())
}

fn workspace_for<'a>(control: &'a mut AppControl<'_>, window: u64) -> Result<&'a mut Workspace> {
    if window == 0 {
        Ok(&mut *control.workspace)
    } else {
        Ok(&mut control
            .windows
            .iter_mut()
            .find(|w| w.id.0 == window)
            .ok_or_else(|| Error::stale_handle(format!("window {window} is gone")))?
            .workspace)
    }
}

fn plot_pane<'a>(
    control: &'a mut AppControl<'_>,
    window: u64,
    tile: u64,
) -> Result<&'a mut PlotPane> {
    plot_pane_in_workspace(workspace_for(control, window)?, window, tile)
}

fn plot_pane_in_workspace(
    workspace: &mut Workspace,
    window: u64,
    tile: u64,
) -> Result<&mut PlotPane> {
    workspace
        .plot_pane_mut(egui_tiles::TileId(tile))
        .ok_or_else(|| Error::stale_handle(format!("plot {tile} in window {window} is gone")))
}

fn plot_info_for(workspace: &Workspace, window: u64, tile: egui_tiles::TileId) -> Result<PlotInfo> {
    workspace
        .plot_infos(window)
        .into_iter()
        .find(|info| info.tile == tile.0)
        .ok_or_else(|| Error::stale_handle(format!("plot {} in window {window} is gone", tile.0)))
}

fn app_split_direction(direction: ScriptSplitDirection) -> crate::shell::workspace::SplitDirection {
    match direction {
        ScriptSplitDirection::Horizontal => crate::shell::workspace::SplitDirection::Horizontal,
        ScriptSplitDirection::Vertical => crate::shell::workspace::SplitDirection::Vertical,
    }
}

fn apply_workspace_request(
    control: &mut AppControl<'_>,
    request: WorkspaceRequest,
) -> Result<ControlResponse> {
    match request {
        WorkspaceRequest::ListWindows => {
            let mut windows = vec![delog_api::control::WindowInfo {
                id: 0,
                title: crate::shell::windows::WindowId::MAIN.title(),
                owner: None,
            }];
            windows.extend(
                control
                    .windows
                    .iter()
                    .map(|window| delog_api::control::WindowInfo {
                        id: window.id.0,
                        title: window.title.clone(),
                        owner: window.owner.clone(),
                    }),
            );
            Ok(ControlResponse::Windows(windows))
        }
        WorkspaceRequest::GetState => Ok(ControlResponse::Workspace(WorkspaceInfo {
            scene_visible: control.workspace.scene_pane_id().is_some(),
        })),
        WorkspaceRequest::AddPlot {
            window,
            direction,
            owner,
        } => {
            let window = window.unwrap_or(0);
            let workspace = workspace_for(control, window)?;
            let target = workspace
                .focused_plot_id()
                .or_else(|| workspace.tree.root())
                .ok_or_else(|| Error::internal("the workspace has no panes to split"))?;
            let new_tile = workspace
                .split_plot(target, app_split_direction(direction))
                .ok_or_else(|| Error::execution("the workspace could not add a plot"))?;
            workspace.plot_pane_mut(new_tile).expect("new plot").owner = owner;
            Ok(ControlResponse::Plots(vec![plot_info_for(
                workspace, window, new_tile,
            )?]))
        }
        WorkspaceRequest::Split {
            window,
            tile,
            direction,
            owner,
        } => {
            let workspace = workspace_for(control, window)?;
            plot_pane_in_workspace(workspace, window, tile)?;
            let new_tile = workspace
                .split_plot(egui_tiles::TileId(tile), app_split_direction(direction))
                .ok_or_else(|| {
                    Error::execution(format!("plot {tile} in window {window} could not be split"))
                })?;
            workspace.plot_pane_mut(new_tile).expect("new plot").owner = owner;
            Ok(ControlResponse::Plots(vec![plot_info_for(
                workspace, window, new_tile,
            )?]))
        }
        WorkspaceRequest::Close { window, tile } => {
            let workspace = workspace_for(control, window)?;
            plot_pane_in_workspace(workspace, window, tile)?;
            let removed = workspace.close_plot(egui_tiles::TileId(tile));
            for field in removed {
                control.caches.unpin(field);
            }
            Ok(ControlResponse::Unit)
        }
        WorkspaceRequest::Equalize { window } => {
            workspace_for(control, window.unwrap_or(0))?.equalize_plot_heights();
            Ok(ControlResponse::Unit)
        }
        WorkspaceRequest::ShowScene { visible } => {
            if control.workspace.scene_pane_id().is_some() != visible {
                control.workspace.toggle_scene_pane();
            }
            Ok(ControlResponse::Unit)
        }
        WorkspaceRequest::OpenWindow { title, owner } => {
            let id =
                crate::shell::windows::open_window(control.windows, control.next_window_id, title);
            let window = control.windows.last_mut().expect("new window");
            window.owner = owner.clone();
            for pane in window.workspace.plot_panes_mut() {
                pane.owner = owner.clone();
            }
            Ok(ControlResponse::Window(delog_api::control::WindowInfo {
                id: id.0,
                title: window.title.clone(),
                owner,
            }))
        }
    }
}

fn apply_playback_request(
    control: &mut AppControl<'_>,
    request: PlaybackRequest,
) -> Result<ControlResponse> {
    match request {
        PlaybackRequest::Get => Ok(ControlResponse::Playback(PlaybackInfo {
            speed: f64::from(control.playback.speed),
            follow_live: control.playback.follow_live,
        })),
        PlaybackRequest::Set { speed, follow_live } => {
            if let Some(speed) = speed {
                control.playback.set_speed(speed as f32);
            }
            if let Some(follow_live) = follow_live {
                control.playback.follow_live = follow_live;
            }
            Ok(ControlResponse::Unit)
        }
    }
}

fn apply_annotation_request(
    control: &mut AppControl<'_>,
    request: AnnotationRequest,
) -> Result<ControlResponse> {
    match request {
        AnnotationRequest::Add {
            window,
            tile,
            geometry,
            label,
            style,
            owner,
        } => {
            let pane = plot_pane(control, window, tile)?;
            let id =
                pane.annotations
                    .add_geometry(crate::plotting::annotations::Geometry::from_script(
                        geometry,
                    ));
            let index = pane.annotations.items().len() - 1;
            let annotation = pane
                .annotations
                .get_mut(id)
                .expect("the annotation was just inserted");
            annotation.label = label;
            annotation.apply_style_patch(style);
            annotation.owner = owner;
            let info = AnnotationInfo {
                plot_instance_id: pane.instance_id,
                window,
                tile,
                id,
                index,
                kind: annotation.geom.kind().to_script(),
                geometry: annotation.geom.to_script(),
                label: annotation.label.clone(),
                color: annotation.style.color,
                owner: annotation.owner.as_ref().map(|owner| owner.name.clone()),
            };
            Ok(ControlResponse::Annotations(vec![info]))
        }
        AnnotationRequest::List { target } => {
            let infos = match target {
                Some((window, tile)) => {
                    let pane = plot_pane(control, window, tile)?;
                    crate::shell::workspace::annotation_infos_for_pane(window, tile, pane)
                }
                None => crate::shell::windows::annotation_infos(control.workspace, control.windows),
            };
            Ok(ControlResponse::Annotations(infos))
        }
        AnnotationRequest::Remove { target, filter } => {
            match target {
                Some((window, tile)) => {
                    let pane = plot_pane(control, window, tile)?;
                    remove_matching_annotations(pane, &filter)?;
                }
                None => {
                    if matches!(filter, AnnotationFilter::Index(_) | AnnotationFilter::Id(_)) {
                        return Err(Error::invalid_input(
                            "a global annotation removal cannot address a single annotation by index or id",
                        ));
                    }
                    for pane in control.workspace.plot_panes_mut() {
                        remove_matching_annotations(pane, &filter)?;
                    }
                    for window in control.windows.iter_mut() {
                        for pane in window.workspace.plot_panes_mut() {
                            remove_matching_annotations(pane, &filter)?;
                        }
                    }
                }
            }
            Ok(ControlResponse::Unit)
        }
        AnnotationRequest::Set {
            window,
            tile,
            id,
            label,
            geometry,
            style,
        } => {
            let pane = plot_pane(control, window, tile)?;
            let annotation = pane.annotations.get_mut(id).ok_or_else(|| {
                Error::stale_handle(format!(
                    "annotation {id} on plot {tile} in window {window} is gone"
                ))
            })?;
            if let Some(label) = label {
                annotation.label = label;
            }
            if let Some(geometry) = geometry {
                annotation.geom = crate::plotting::annotations::Geometry::from_script(geometry);
            }
            annotation.apply_style_patch(style);
            Ok(ControlResponse::Unit)
        }
    }
}

fn remove_matching_annotations(pane: &mut PlotPane, filter: &AnnotationFilter) -> Result<()> {
    match filter {
        AnnotationFilter::All => {
            pane.annotations.clear();
            Ok(())
        }
        AnnotationFilter::Index(index) => {
            let id = pane
                .annotations
                .items()
                .get(*index)
                .map(|annotation| annotation.id)
                .ok_or_else(|| Error::stale_handle(format!("annotation {index} is gone")))?;
            pane.annotations.remove(id);
            Ok(())
        }
        AnnotationFilter::Id(id) => {
            if pane.annotations.get(*id).is_none() {
                return Err(Error::stale_handle(format!("annotation {id} is gone")));
            }
            pane.annotations.remove(*id);
            Ok(())
        }
        AnnotationFilter::Kind(kind) => {
            pane.annotations
                .retain(|annotation| annotation.geom.kind().to_script() != *kind);
            Ok(())
        }
        AnnotationFilter::Label(label) => {
            pane.annotations
                .retain(|annotation| &annotation.label != label);
            Ok(())
        }
        AnnotationFilter::Owner(owner) => {
            pane.annotations.retain(|annotation| {
                annotation.owner.as_ref().map(|owner| owner.name.as_str()) != Some(owner.as_str())
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::plotting::plot::TraceMode as PlotTraceMode;
    use delog_api::ErrorKind;
    use delog_api::control::TraceMode as ScriptTraceMode;

    trait ErrorContains {
        fn contains(&self, text: &str) -> bool;
    }

    impl ErrorContains for delog_api::Error {
        fn contains(&self, text: &str) -> bool {
            self.to_string().contains(text)
        }
    }
    use delog_api::control::{
        AnnotationGeometry, AnnotationKind, AnnotationStylePatch, GenerationRequest, LayoutRequest,
        MarkerPatch, MarkerRequest, ProfilePosition, ResolvedVehicleField, ScriptOwner,
        VehicleFilter, VehicleInfo, VehicleModel, VehicleOrientation, VehiclePatch,
        VehiclePosition, VehicleProfileRequest, VehicleRequest, VehicleSpec,
    };
    use delog_api::markers::PendingMarker;

    fn marker(time_us: i64, label: &str) -> PendingMarker {
        PendingMarker {
            time_us,
            label: label.into(),
            color: None,
            note: String::new(),
        }
    }

    type TestVehicleState = (
        &'static mut Vec<crate::scene3d::vehicle::VehicleConfig>,
        &'static mut u64,
        &'static mut u64,
        &'static mut bool,
    );

    fn test_vehicle_state() -> TestVehicleState {
        (
            Box::leak(Box::new(Vec::new())),
            Box::leak(Box::new(1)),
            Box::leak(Box::new(0)),
            Box::leak(Box::new(false)),
        )
    }

    #[test]
    fn a_marker_append_request_reaches_the_marker_store() {
        let mut markers = crate::plotting::markers::Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let snapshot = StoreSnapshot::empty();
        let (vehicles, next_vehicle_id, vehicle_revision, traj_dirty) = test_vehicle_state();
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles,
            next_vehicle_id,
            vehicle_revision,
            traj_dirty,
            vehicle_profiles: None,
        };
        let response = apply(
            &mut control,
            ControlRequest::Markers(MarkerRequest::Append {
                owner: "flight.py".into(),
                generation: 1,
                markers: vec![marker(10, "armed")],
            }),
        )
        .unwrap();
        assert_eq!(response, ControlResponse::Unit);
        assert_eq!(markers.as_slice().len(), 1);
        assert_eq!(markers.as_slice()[0].label, "armed");
    }

    #[test]
    fn a_rerun_replaces_only_its_own_owner() {
        let mut markers = crate::plotting::markers::Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let snapshot = StoreSnapshot::empty();
        let (vehicles, next_vehicle_id, vehicle_revision, traj_dirty) = test_vehicle_state();
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles,
            next_vehicle_id,
            vehicle_revision,
            traj_dirty,
            vehicle_profiles: None,
        };
        for owner in ["flight.py", "other.py"] {
            apply(
                &mut control,
                ControlRequest::Markers(MarkerRequest::Append {
                    owner: owner.into(),
                    generation: 1,
                    markers: vec![marker(10, owner)],
                }),
            )
            .unwrap();
        }
        apply(
            &mut control,
            ControlRequest::Markers(MarkerRequest::Append {
                owner: "flight.py".into(),
                generation: 2,
                markers: vec![marker(20, "new")],
            }),
        )
        .unwrap();
        apply(
            &mut control,
            ControlRequest::Generation(GenerationRequest::Commit {
                owner: "flight.py".into(),
                generation: 2,
            }),
        )
        .unwrap();
        let labels: Vec<&str> = markers
            .as_slice()
            .iter()
            .map(|m| m.label.as_str())
            .collect();
        assert_eq!(labels, ["other.py", "new"]);
    }

    fn snapshot_with_field(source: &str, topic: &str, field: &str) -> (StoreSnapshot, FieldId) {
        let mut ids = delog_core::identity::IdentityRegistry::new();
        let source_id = ids.add_source(source);
        let topic_id = ids.add_topic(source_id, topic).unwrap();
        let field_id = ids.add_field(topic_id, field).unwrap();
        (
            StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot"),
            field_id,
        )
    }

    fn snapshot_with_two_fields(
        source: &str,
        topic: &str,
        field_a: &str,
        field_b: &str,
    ) -> (StoreSnapshot, FieldId, FieldId) {
        let mut ids = delog_core::identity::IdentityRegistry::new();
        let source_id = ids.add_source(source);
        let topic_id = ids.add_topic(source_id, topic).unwrap();
        let field_a_id = ids.add_field(topic_id, field_a).unwrap();
        let field_b_id = ids.add_field(topic_id, field_b).unwrap();
        (
            StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot"),
            field_a_id,
            field_b_id,
        )
    }

    struct VehicleHarness<'a> {
        markers: Markers,
        workspace: crate::shell::workspace::Workspace,
        windows: Vec<crate::shell::windows::ExtendedWindow>,
        playback: Playback,
        next_window_id: u64,
        caches: CacheManager,
        snapshot: StoreSnapshot,
        vehicles: Vec<crate::scene3d::vehicle::VehicleConfig>,
        next_vehicle_id: u64,
        vehicle_revision: u64,
        traj_dirty: bool,
        vehicle_profiles: Option<&'a crate::session::vehicle_profiles::VehicleProfileLibrary>,
    }

    impl VehicleHarness<'_> {
        fn new(snapshot: StoreSnapshot) -> Self {
            Self {
                markers: Markers::new(),
                workspace: crate::shell::workspace::Workspace::new(),
                windows: Vec::new(),
                playback: Playback::default(),
                next_window_id: 1,
                caches: CacheManager::new(),
                snapshot,
                vehicles: Vec::new(),
                next_vehicle_id: 1,
                vehicle_revision: 0,
                traj_dirty: false,
                vehicle_profiles: None,
            }
        }

        fn with_profiles<'a>(
            snapshot: StoreSnapshot,
            profiles: &'a crate::session::vehicle_profiles::VehicleProfileLibrary,
        ) -> VehicleHarness<'a> {
            VehicleHarness {
                markers: Markers::new(),
                workspace: crate::shell::workspace::Workspace::new(),
                windows: Vec::new(),
                playback: Playback::default(),
                next_window_id: 1,
                caches: CacheManager::new(),
                snapshot,
                vehicles: Vec::new(),
                next_vehicle_id: 1,
                vehicle_revision: 0,
                traj_dirty: false,
                vehicle_profiles: Some(profiles),
            }
        }

        fn apply(&mut self, request: VehicleRequest) -> Result<ControlResponse> {
            let mut control = AppControl {
                markers: &mut self.markers,
                workspace: &mut self.workspace,
                windows: &mut self.windows,
                playback: &mut self.playback,
                next_window_id: &mut self.next_window_id,
                caches: &mut self.caches,
                snapshot: &self.snapshot,
                vehicles: &mut self.vehicles,
                next_vehicle_id: &mut self.next_vehicle_id,
                vehicle_revision: &mut self.vehicle_revision,
                traj_dirty: &mut self.traj_dirty,
                vehicle_profiles: self.vehicle_profiles,
            };
            super::apply(&mut control, ControlRequest::Vehicles(Box::new(request)))
        }

        fn apply_control(&mut self, request: ControlRequest) -> Result<ControlResponse> {
            let mut control = AppControl {
                markers: &mut self.markers,
                workspace: &mut self.workspace,
                windows: &mut self.windows,
                playback: &mut self.playback,
                next_window_id: &mut self.next_window_id,
                caches: &mut self.caches,
                snapshot: &self.snapshot,
                vehicles: &mut self.vehicles,
                next_vehicle_id: &mut self.next_vehicle_id,
                vehicle_revision: &mut self.vehicle_revision,
                traj_dirty: &mut self.traj_dirty,
                vehicle_profiles: self.vehicle_profiles,
            };
            super::apply(&mut control, request)
        }

        fn apply_profile(&mut self, request: VehicleProfileRequest) -> Result<ControlResponse> {
            let mut control = AppControl {
                markers: &mut self.markers,
                workspace: &mut self.workspace,
                windows: &mut self.windows,
                playback: &mut self.playback,
                next_window_id: &mut self.next_window_id,
                caches: &mut self.caches,
                snapshot: &self.snapshot,
                vehicles: &mut self.vehicles,
                next_vehicle_id: &mut self.next_vehicle_id,
                vehicle_revision: &mut self.vehicle_revision,
                traj_dirty: &mut self.traj_dirty,
                vehicle_profiles: self.vehicle_profiles,
            };
            super::apply(&mut control, ControlRequest::VehicleProfiles(request))
        }

        fn add(&mut self, spec: VehicleSpec) -> Result<VehicleInfo> {
            match self.apply(VehicleRequest::Add(spec))? {
                ControlResponse::Vehicles(mut infos) if infos.len() == 1 => Ok(infos.remove(0)),
                response => Err(Error::internal(format!(
                    "unexpected response: {response:?}"
                ))),
            }
        }

        fn source(&self, label: &str) -> delog_core::identity::SourceId {
            self.snapshot
                .sources
                .iter()
                .find(|source| !source.entry.removed && source.entry.label == label)
                .unwrap()
                .entry
                .id
        }

        fn field(&self, source: &str, topic: &str, field: &str) -> FieldId {
            let source = self
                .snapshot
                .sources
                .iter()
                .find(|candidate| !candidate.entry.removed && candidate.entry.label == source)
                .unwrap();
            let topic = source
                .topics
                .iter()
                .filter_map(|id| self.snapshot.topic(*id))
                .find(|candidate| !candidate.entry.removed && candidate.entry.name == topic)
                .unwrap();
            self.snapshot
                .fields
                .iter()
                .find(|candidate| {
                    !candidate.removed
                        && candidate.topic == topic.entry.id
                        && candidate.name == field
                })
                .unwrap()
                .id
        }

        fn gps_spec(&self, source: &str, label: &str) -> VehicleSpec {
            let resolved = |field: &str| ResolvedVehicleField {
                id: self.field(source, "GPS", field),
                path: format!("{source}/GPS/{field}"),
            };
            VehicleSpec {
                source_id: self.source(source),
                source: source.into(),
                label: label.into(),
                show: true,
                show_path: true,
                position: VehiclePosition::Gps {
                    lat: resolved("lat"),
                    lon: resolved("lon"),
                    alt: resolved("alt"),
                    lat_lon_dege7: false,
                    alt_mm: false,
                    alt_offset_m: 0.0,
                },
                orientation: VehicleOrientation::Static,
                model: VehicleModel::Quad,
                color: [0.3, 0.6, 1.0, 1.0],
                path_color: [1.0, 0.6, 0.2, 1.0],
                scale: 1.0,
                owner: None,
            }
        }
    }

    #[test]
    fn a_supported_batch_commits_all_mutations_together() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        let response = harness
            .apply_control(ControlRequest::Batch(vec![
                ControlRequest::Playback(PlaybackRequest::Set {
                    speed: Some(2.0),
                    follow_live: Some(true),
                }),
                ControlRequest::Markers(MarkerRequest::Append {
                    owner: "flight.py".into(),
                    generation: 1,
                    markers: vec![marker(10, "armed")],
                }),
            ]))
            .unwrap();

        assert_eq!(response, ControlResponse::Unit);
        assert_eq!(harness.playback.speed, 2.0);
        assert!(harness.playback.follow_live);
        assert_eq!(harness.markers.as_slice()[0].label, "armed");
    }

    #[test]
    fn stale_plot_and_invalid_playback_keep_distinct_error_kinds() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        let stale = harness
            .apply_control(ControlRequest::Traces(TraceRequest::List {
                window: 0,
                tile: u64::MAX,
            }))
            .unwrap_err();
        assert_eq!(stale.kind(), ErrorKind::StaleHandle);

        let before = harness.playback;
        let invalid = harness
            .apply_control(ControlRequest::Playback(PlaybackRequest::Set {
                speed: Some(f64::NAN),
                follow_live: Some(true),
            }))
            .unwrap_err();
        assert_eq!(invalid.kind(), ErrorKind::InvalidInput);
        assert_eq!(harness.playback, before);
    }

    #[test]
    fn listing_a_missing_window_reports_a_stale_handle() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        let error = harness
            .apply_control(ControlRequest::Plots(PlotRequest::List {
                window: Some(u64::MAX),
            }))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::StaleHandle);
    }

    #[test]
    fn stale_vehicle_in_batch_preserves_kind_and_rolls_back() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        let before = harness.playback;
        let error = harness
            .apply_control(ControlRequest::Batch(vec![
                ControlRequest::Playback(PlaybackRequest::Set {
                    speed: Some(4.0),
                    follow_live: Some(true),
                }),
                ControlRequest::Vehicles(Box::new(VehicleRequest::Set {
                    id: u64::MAX,
                    patch: VehiclePatch {
                        label: Some("gone".into()),
                        ..VehiclePatch::default()
                    },
                })),
            ]))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::StaleHandle);
        assert!(error.to_string().starts_with("batch request 1:"), "{error}");
        assert_eq!(harness.playback, before);
    }

    #[test]
    fn replaced_field_path_is_a_stale_handle() {
        let mut harness = VehicleHarness::new(vehicle_snapshot_for(&["flight"]));
        let mut spec = harness.gps_spec("flight", "Vehicle");
        if let VehiclePosition::Gps { lat, .. } = &mut spec.position {
            lat.path = "flight/GPS/replaced".into();
        }
        let error = harness.apply(VehicleRequest::Add(spec)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::StaleHandle);
        assert!(harness.vehicles.is_empty());
    }

    #[test]
    fn a_failed_batch_leaves_earlier_mutations_unchanged() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        let before = harness.playback;
        let error = harness
            .apply_control(ControlRequest::Batch(vec![
                ControlRequest::Playback(PlaybackRequest::Set {
                    speed: Some(4.0),
                    follow_live: Some(true),
                }),
                ControlRequest::Markers(MarkerRequest::Set {
                    id: u64::MAX,
                    patch: MarkerPatch {
                        label: Some("gone".into()),
                        ..MarkerPatch::default()
                    },
                }),
            ]))
            .unwrap_err();

        assert!(error.contains("batch request 1"), "{error}");
        assert_eq!(harness.playback, before);
        assert!(harness.markers.as_slice().is_empty());
    }

    #[test]
    fn batch_validation_rejects_reads_before_any_mutation_runs() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        let before = harness.playback;
        let error = harness
            .apply_control(ControlRequest::Batch(vec![
                ControlRequest::Playback(PlaybackRequest::Set {
                    speed: Some(4.0),
                    follow_live: None,
                }),
                ControlRequest::Plots(PlotRequest::Focused),
            ]))
            .unwrap_err();

        assert!(error.contains("batch request 1"), "{error}");
        assert_eq!(harness.playback, before);
    }

    #[test]
    fn named_batch_validation_failure_rolls_back_its_immediate_generation() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        for (generation, label) in [(1, "previous"), (2, "failed run")] {
            harness
                .apply_control(ControlRequest::Markers(MarkerRequest::Append {
                    owner: "flight.py".into(),
                    generation,
                    markers: vec![marker(generation as i64, label)],
                }))
                .unwrap();
        }

        let error = harness
            .apply_control(ControlRequest::Batch(vec![
                ControlRequest::Plots(PlotRequest::Focused),
                ControlRequest::Generation(GenerationRequest::Commit {
                    owner: "flight.py".into(),
                    generation: 2,
                }),
            ]))
            .unwrap_err();

        assert!(error.contains("batch request 0"), "{error}");
        assert_eq!(
            harness
                .markers
                .as_slice()
                .iter()
                .map(|marker| marker.label.as_str())
                .collect::<Vec<_>>(),
            ["previous"]
        );
    }

    #[test]
    fn a_failed_named_publication_rolls_back_immediate_objects_from_that_generation() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        for (generation, label) in [(1, "previous"), (2, "failed run")] {
            harness
                .apply_control(ControlRequest::Markers(MarkerRequest::Append {
                    owner: "flight.py".into(),
                    generation,
                    markers: vec![marker(generation as i64, label)],
                }))
                .unwrap();
        }
        let before = harness.playback;

        let error = harness
            .apply_control(ControlRequest::Batch(vec![
                ControlRequest::Playback(PlaybackRequest::Set {
                    speed: Some(3.0),
                    follow_live: None,
                }),
                ControlRequest::Markers(MarkerRequest::Set {
                    id: u64::MAX,
                    patch: MarkerPatch::default(),
                }),
                ControlRequest::Generation(GenerationRequest::Commit {
                    owner: "flight.py".into(),
                    generation: 2,
                }),
            ]))
            .unwrap_err();

        assert!(error.contains("batch request 1"), "{error}");
        assert_eq!(harness.playback, before);
        assert_eq!(
            harness
                .markers
                .as_slice()
                .iter()
                .map(|marker| marker.label.as_str())
                .collect::<Vec<_>>(),
            ["previous"]
        );
    }

    #[test]
    fn layout_apply_skips_and_reports_ambiguous_and_unresolved_fields() {
        let mut ids = delog_core::identity::IdentityRegistry::new();
        for source_name in ["flight-b", "flight-a"] {
            let source = ids.add_source(source_name);
            let topic = ids.add_topic(source, "ATT").unwrap();
            ids.add_field(topic, "roll").unwrap();
        }
        let snapshot = StoreSnapshot::from_registry(&ids, [], 0).unwrap();
        let mut harness = VehicleHarness::new(snapshot);
        let json = serde_json::json!({
            "delog_layout": 2,
            "name": "report",
            "playback": {"speed": 2.0, "follow_live": true},
            "workspace": {"root": {"plot": {
                "traces": [
                    {"field": {"topic": "ATT", "field": "roll"}, "color": [1.0, 1.0, 1.0, 1.0], "width_px": 1.5, "mode": "line", "visible": true},
                    {"field": {"topic": "GPS", "field": "alt"}, "color": [1.0, 1.0, 1.0, 1.0], "width_px": 1.5, "mode": "line", "visible": true}
                ],
                "show_legend": true,
                "show_tooltip": true,
                "annotations": [{
                    "kind": "segment", "points": [[1.0, 2.0]], "y": null,
                    "label": "broken", "color": [1.0, 1.0, 1.0, 1.0],
                    "stroke_px": 1.0, "fill_opacity": 0.0, "font_px": 12.0,
                    "arrow": false
                }]
            }}},
            "windows": [],
            "vehicles": []
        })
        .to_string();

        let response = harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Apply { json }))
            .unwrap();
        let ControlResponse::LoadReport(report) = response else {
            panic!("expected a layout load report")
        };
        assert_eq!(report.ambiguous.len(), 1);
        assert_eq!(report.ambiguous[0].field, "ATT.roll");
        assert_eq!(report.ambiguous[0].candidates, ["flight-a", "flight-b"]);
        assert_eq!(report.unresolved, ["GPS.alt"]);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("broken"));
        assert_eq!(harness.playback.speed, 2.0);
        assert!(harness.playback.follow_live);
        let pane = harness.workspace.plot_panes().next().unwrap();
        assert_eq!(pane.traces.len(), 0);
        assert_eq!(pane.ghosts.len(), 2);
    }

    #[test]
    fn layout_current_returns_versioned_json_and_invalid_apply_is_atomic() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        harness.playback.set_speed(3.0);
        harness.playback.follow_live = true;
        let response = harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Current))
            .unwrap();
        let ControlResponse::Layout(json) = response else {
            panic!("expected layout JSON")
        };
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["delog_layout"], 2);
        assert_eq!(value["playback"]["speed"], 3.0);

        let before = harness.playback;
        let error = harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Apply {
                json: r#"{"delog_layout":99}"#.into(),
            }))
            .unwrap_err();
        assert!(error.contains("unsupported layout version") || error.contains("JSON"));
        assert_eq!(harness.playback, before);
    }

    #[test]
    fn layout_clear_resets_current_state_without_reusing_marker_or_vehicle_ids() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        let old_marker = harness.markers.add_at(10);
        harness
            .windows
            .push(crate::shell::windows::ExtendedWindow::new(
                crate::shell::windows::WindowId(4),
            ));
        harness.next_window_id = 5;
        harness.next_vehicle_id = 17;
        harness.playback.set_speed(4.0);
        harness.playback.follow_live = true;

        assert_eq!(
            harness
                .apply_control(ControlRequest::Layouts(LayoutRequest::Clear))
                .unwrap(),
            ControlResponse::Unit
        );
        assert!(harness.windows.is_empty());
        assert_eq!(harness.next_window_id, 5);
        assert_eq!(harness.playback.speed, 1.0);
        assert!(!harness.playback.follow_live);
        assert!(harness.markers.as_slice().is_empty());
        assert_eq!(harness.next_vehicle_id, 17);
        let new_marker = harness.markers.add_at(20);
        assert!(new_marker > old_marker);
    }

    #[test]
    fn window_ids_do_not_retarget_after_layout_clear_or_apply() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        let ControlResponse::Window(old) = harness
            .apply_control(ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                title: None,
                owner: None,
            }))
            .unwrap()
        else {
            panic!("expected window")
        };

        harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Clear))
            .unwrap();
        let old_error = harness
            .apply_control(ControlRequest::Plots(PlotRequest::List {
                window: Some(old.id),
            }))
            .unwrap_err();
        assert_eq!(old_error.kind(), ErrorKind::StaleHandle);
        let ControlResponse::Layout(empty_layout) = harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Current))
            .unwrap()
        else {
            panic!("expected layout")
        };

        let ControlResponse::Window(replacement) = harness
            .apply_control(ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                title: None,
                owner: None,
            }))
            .unwrap()
        else {
            panic!("expected window")
        };
        assert_ne!(replacement.id, old.id);

        harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Apply {
                json: empty_layout,
            }))
            .unwrap();
        let ControlResponse::Window(after_apply) = harness
            .apply_control(ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                title: None,
                owner: None,
            }))
            .unwrap()
        else {
            panic!("expected window")
        };
        assert_ne!(after_apply.id, old.id);
        assert_ne!(after_apply.id, replacement.id);
        for stale_id in [old.id, replacement.id] {
            let error = harness
                .apply_control(ControlRequest::Plots(PlotRequest::List {
                    window: Some(stale_id),
                }))
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::StaleHandle);
        }
    }

    #[test]
    fn restored_windows_receive_fresh_ids_instead_of_retargeting_stale_handles() {
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());
        let mut stale_ids = Vec::new();
        for title in ["First", "Second"] {
            let ControlResponse::Window(info) = harness
                .apply_control(ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                    title: Some(title.into()),
                    owner: None,
                }))
                .unwrap()
            else {
                panic!("expected window")
            };
            stale_ids.push(info.id);
        }
        let ControlResponse::Layout(saved) = harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Current))
            .unwrap()
        else {
            panic!("expected saved layout")
        };
        harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Clear))
            .unwrap();
        harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Apply {
                json: saved,
            }))
            .unwrap();

        assert_eq!(
            harness.workspace.tree.id(),
            egui::Id::new(("plot_workspace", crate::shell::windows::WindowId::MAIN.0))
        );
        assert_eq!(harness.windows.len(), 2);
        let restored_ids = harness
            .windows
            .iter()
            .map(|window| window.id.0)
            .collect::<Vec<_>>();
        assert!(restored_ids.iter().all(|id| *id > stale_ids[1]));
        assert!(restored_ids[0] < restored_ids[1]);
        assert!(harness.next_window_id > *restored_ids.iter().max().unwrap());
        assert_eq!(
            harness
                .windows
                .iter()
                .map(|window| window.title.as_str())
                .collect::<Vec<_>>(),
            ["First", "Second"]
        );
        for window in &harness.windows {
            assert_eq!(
                window.workspace.tree.id(),
                egui::Id::new(("plot_workspace", window.id.0))
            );
            assert_eq!(window.workspace.plot_infos(window.id.0).len(), 1);
        }
        for stale_id in stale_ids {
            let error = harness
                .apply_control(ControlRequest::Plots(PlotRequest::List {
                    window: Some(stale_id),
                }))
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::StaleHandle);
        }
    }

    #[test]
    fn layout_named_library_operations_and_file_transfer_round_trip() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        struct XdgGuard(Option<std::ffi::OsString>);
        impl Drop for XdgGuard {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => unsafe { std::env::set_var("XDG_DATA_HOME", value) },
                    None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
                }
            }
        }
        let temp = tempfile::TempDir::new().unwrap();
        let old = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", temp.path()) };
        let _guard = XdgGuard(old);
        let mut harness = VehicleHarness::new(StoreSnapshot::empty());

        harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Save {
                name: "alpha".into(),
            }))
            .unwrap();
        assert_eq!(
            harness
                .apply_control(ControlRequest::Layouts(LayoutRequest::List))
                .unwrap(),
            ControlResponse::Names(vec!["alpha".into()])
        );
        harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Duplicate {
                from: "alpha".into(),
                to: "beta".into(),
            }))
            .unwrap();
        harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Rename {
                from: "beta".into(),
                to: "gamma".into(),
            }))
            .unwrap();
        let exported = temp.path().join("exported.json");
        harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::ExportFile {
                name: "alpha".into(),
                path: exported.display().to_string(),
            }))
            .unwrap();
        assert!(exported.is_file());
        assert!(matches!(
            harness
                .apply_control(ControlRequest::Layouts(LayoutRequest::ImportFile {
                    path: exported.display().to_string(),
                }))
                .unwrap(),
            ControlResponse::LoadReport(_)
        ));
        assert_eq!(
            harness
                .apply_control(ControlRequest::Layouts(LayoutRequest::List))
                .unwrap(),
            ControlResponse::Names(vec!["alpha".into(), "gamma".into()])
        );
        for name in ["alpha", "gamma"] {
            harness
                .apply_control(ControlRequest::Layouts(LayoutRequest::Delete {
                    name: name.into(),
                }))
                .unwrap();
        }
        assert_eq!(
            harness
                .apply_control(ControlRequest::Layouts(LayoutRequest::List))
                .unwrap(),
            ControlResponse::Names(Vec::new())
        );

        let error = harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Save {
                name: "../unsafe".into(),
            }))
            .unwrap_err();
        assert!(error.contains("layout names"), "{error}");
    }

    #[test]
    fn layout_apply_assigns_fresh_vehicle_ids_and_invalidates_trajectories() {
        let mut harness = VehicleHarness::new(vehicle_snapshot_for(&["flight"]));
        let spec = harness.gps_spec("flight", "Saved UAV");
        let original = harness.add(spec).unwrap();
        let ControlResponse::Layout(json) = harness
            .apply_control(ControlRequest::Layouts(LayoutRequest::Current))
            .unwrap()
        else {
            panic!("expected layout JSON")
        };
        harness
            .apply(VehicleRequest::Remove(VehicleFilter::All))
            .unwrap();
        harness.traj_dirty = false;
        let revision_before = harness.vehicle_revision;

        assert!(matches!(
            harness
                .apply_control(ControlRequest::Layouts(LayoutRequest::Apply { json }))
                .unwrap(),
            ControlResponse::LoadReport(_)
        ));
        assert_eq!(harness.vehicles.len(), 1);
        assert_ne!(harness.vehicles[0].runtime.id, original.id);
        assert_eq!(harness.vehicle_revision, revision_before + 1);
        assert!(harness.traj_dirty);
    }

    fn vehicle_snapshot_for(sources: &[&str]) -> StoreSnapshot {
        let mut ids = delog_core::identity::IdentityRegistry::new();
        for source_name in sources {
            let source = ids.add_source(*source_name);
            let gps = ids.add_topic(source, "GPS").unwrap();
            for field in ["lat", "lon", "alt"] {
                ids.add_field(gps, field).unwrap();
            }
        }
        StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot")
    }

    fn profile_doc_with_gps_field(
        topic: &str,
        field: &str,
    ) -> crate::session::vehicle_profiles::VehicleProfileDoc {
        use crate::config::layout::doc::{
            FieldRef, ModelLayout, OriLayout, PosLayout, VehicleLayout,
        };
        use crate::session::vehicle_profiles::{VEHICLE_PROFILE_VERSION, VehicleProfileDoc};

        let field = || FieldRef {
            topic: topic.to_owned(),
            field: field.to_owned(),
        };
        VehicleProfileDoc {
            delog_vehicle_profile: VEHICLE_PROFILE_VERSION,
            name: "missing".to_owned(),
            vehicle: VehicleLayout {
                label: "UAV".to_owned(),
                owner: None,
                show: true,
                show_path: true,
                model: ModelLayout::Quad,
                color: [76, 153, 255, 255],
                path_color: [255, 153, 51, 255],
                scale: 1.0,
                position: PosLayout::Gps {
                    lat: field(),
                    lon: field(),
                    alt: field(),
                    lat_lon_dege7: false,
                    alt_mm: false,
                    alt_offset_m: 0.0,
                },
                orientation: OriLayout::Static,
            },
        }
    }

    #[test]
    fn profile_requests_save_load_apply_and_delete_through_the_real_library() {
        let temp = tempfile::TempDir::new().unwrap();
        let library = crate::session::vehicle_profiles::VehicleProfileLibrary::new(temp.path());
        let mut harness =
            VehicleHarness::with_profiles(vehicle_snapshot_for(&["flight"]), &library);
        let mut spec = harness.gps_spec("flight", "UAV");
        spec.scale = 0.001;
        spec.color = [1.0, 0.0, 0.0, 128.0 / 255.0];
        spec.path_color = [0.0, 1.0, 0.0, 64.0 / 255.0];
        let original = harness.add(spec).unwrap();

        harness
            .apply_profile(VehicleProfileRequest::Save {
                name: "quad-gps".into(),
                vehicle_id: original.id,
            })
            .unwrap();
        assert_eq!(
            harness.apply_profile(VehicleProfileRequest::List).unwrap(),
            ControlResponse::Names(vec!["quad-gps".into()])
        );
        let loaded = harness
            .apply_profile(VehicleProfileRequest::Load {
                name: "quad-gps".into(),
            })
            .unwrap();
        let ControlResponse::VehicleProfile(loaded) = loaded else {
            panic!("expected vehicle profile")
        };
        assert_eq!(loaded.name, "quad-gps");
        assert!(matches!(loaded.position, ProfilePosition::Gps { .. }));
        assert_eq!(loaded.scale, 0.001);
        assert_eq!(loaded.color, [1.0, 0.0, 0.0, 128.0 / 255.0]);
        assert_eq!(loaded.path_color, [0.0, 1.0, 0.0, 64.0 / 255.0]);

        let source = harness.source("flight");
        let applied = harness
            .apply_profile(VehicleProfileRequest::Apply {
                name: "quad-gps".into(),
                source_id: source,
                source: "flight".into(),
                owner: Some(ScriptOwner {
                    name: "flight.py".into(),
                    generation: 2,
                }),
            })
            .unwrap();
        let ControlResponse::Vehicles(applied) = applied else {
            panic!("expected vehicle")
        };
        assert_eq!(applied.len(), 1);
        assert_ne!(applied[0].id, original.id);
        assert_eq!(applied[0].spec.owner.as_ref().unwrap().generation, 2);
        assert_eq!(applied[0].spec.scale, 0.001);
        assert_eq!(applied[0].spec.color, [1.0, 0.0, 0.0, 128.0 / 255.0]);
        assert_eq!(applied[0].spec.path_color, [0.0, 1.0, 0.0, 64.0 / 255.0]);

        harness
            .apply_profile(VehicleProfileRequest::Delete {
                name: "quad-gps".into(),
            })
            .unwrap();
        assert_eq!(
            harness.apply_profile(VehicleProfileRequest::List).unwrap(),
            ControlResponse::Names(Vec::new())
        );
    }

    #[test]
    fn applying_a_profile_with_unresolved_fields_adds_no_vehicle() {
        let temp = tempfile::TempDir::new().unwrap();
        let library = crate::session::vehicle_profiles::VehicleProfileLibrary::new(temp.path());
        library
            .save("missing", &profile_doc_with_gps_field("GPS", "missing"))
            .unwrap();
        let mut harness =
            VehicleHarness::with_profiles(vehicle_snapshot_for(&["flight"]), &library);
        let before = harness.vehicles.len();
        let source = harness.source("flight");
        let error = harness
            .apply_profile(VehicleProfileRequest::Apply {
                name: "missing".into(),
                source_id: source,
                source: "flight".into(),
                owner: None,
            })
            .unwrap_err();
        assert!(
            error.contains("missing") && error.contains("flight"),
            "{error}"
        );
        assert_eq!(harness.vehicles.len(), before);
        assert_eq!(harness.vehicle_revision, 0);
    }

    #[test]
    fn profiles_with_invalid_scale_or_manual_georeference_are_rejected_atomically() {
        use crate::config::layout::doc::{NedRefLayout, PosLayout};

        let temp = tempfile::TempDir::new().unwrap();
        let library = crate::session::vehicle_profiles::VehicleProfileLibrary::new(temp.path());

        let mut invalid_scale = profile_doc_with_gps_field("GPS", "lat");
        invalid_scale.name = "invalid-scale".into();
        invalid_scale.vehicle.scale = -1.0;
        std::fs::write(
            temp.path().join("invalid-scale.json"),
            serde_json::to_string_pretty(&invalid_scale).unwrap(),
        )
        .unwrap();

        let mut invalid_georef = profile_doc_with_gps_field("GPS", "lat");
        invalid_georef.name = "invalid-georef".into();
        invalid_georef.vehicle.position = PosLayout::Ned {
            north: crate::config::layout::doc::FieldRef {
                topic: "GPS".into(),
                field: "lat".into(),
            },
            east: crate::config::layout::doc::FieldRef {
                topic: "GPS".into(),
                field: "lon".into(),
            },
            down: crate::config::layout::doc::FieldRef {
                topic: "GPS".into(),
                field: "alt".into(),
            },
            reference: Some(NedRefLayout::Manual {
                lat_deg: 91.0,
                lon_deg: 0.0,
                alt_m: 0.0,
            }),
        };
        std::fs::write(
            temp.path().join("invalid-georef.json"),
            serde_json::to_string_pretty(&invalid_georef).unwrap(),
        )
        .unwrap();

        let mut harness =
            VehicleHarness::with_profiles(vehicle_snapshot_for(&["flight"]), &library);
        let source = harness.source("flight");
        for name in ["invalid-scale", "invalid-georef"] {
            let error = harness
                .apply_profile(VehicleProfileRequest::Apply {
                    name: name.into(),
                    source_id: source,
                    source: "flight".into(),
                    owner: None,
                })
                .unwrap_err();
            assert!(error.contains("invalid"), "{error}");
            assert!(harness.vehicles.is_empty());
            assert_eq!(harness.vehicle_revision, 0);
            assert!(!harness.traj_dirty);
        }
    }

    #[test]
    fn missing_and_corrupt_profiles_return_readable_errors_without_mutation() {
        let temp = tempfile::TempDir::new().unwrap();
        let library = crate::session::vehicle_profiles::VehicleProfileLibrary::new(temp.path());
        std::fs::write(temp.path().join("broken.json"), "{ not json").unwrap();
        let mut harness =
            VehicleHarness::with_profiles(vehicle_snapshot_for(&["flight"]), &library);

        let missing = harness
            .apply_profile(VehicleProfileRequest::Load {
                name: "missing".into(),
            })
            .unwrap_err();
        assert!(missing.contains("missing"), "{missing}");

        let corrupt = harness
            .apply_profile(VehicleProfileRequest::Load {
                name: "broken".into(),
            })
            .unwrap_err();
        assert!(
            corrupt.contains("broken") && corrupt.contains("invalid"),
            "{corrupt}"
        );
        assert!(harness.vehicles.is_empty());
        assert_eq!(harness.vehicle_revision, 0);
    }

    #[test]
    fn adding_and_setting_a_vehicle_assigns_a_stable_id_and_invalidates_trajectories() {
        let mut harness = VehicleHarness::new(vehicle_snapshot_for(&["flight"]));
        let spec = harness.gps_spec("flight", "Vehicle");
        let added = harness.add(spec).unwrap();
        assert_ne!(added.id, 0);
        assert_eq!(harness.vehicle_revision, 1);
        assert!(harness.traj_dirty);

        harness.traj_dirty = false;
        harness
            .apply(VehicleRequest::Set {
                id: added.id,
                patch: VehiclePatch {
                    label: Some("renamed".into()),
                    ..VehiclePatch::default()
                },
            })
            .unwrap();
        assert_eq!(harness.vehicles[0].label, "renamed");
        assert_eq!(harness.vehicles[0].runtime.id, added.id);
        assert_eq!(harness.vehicle_revision, 2);
        assert!(harness.traj_dirty);
    }

    #[test]
    fn a_set_that_cannot_build_its_response_leaves_vehicle_state_unchanged() {
        let mut ids = delog_core::identity::IdentityRegistry::new();
        let source = ids.add_source("flight");
        let gps = ids.add_topic(source, "GPS").unwrap();
        for field in ["lat", "lon", "alt"] {
            ids.add_field(gps, field).unwrap();
        }
        let snapshot = StoreSnapshot::from_registry(&ids, [], 0).expect("identity snapshot");
        let mut harness = VehicleHarness::new(snapshot);
        let spec = harness.gps_spec("flight", "Vehicle");
        let added = harness.add(spec).unwrap();

        ids.remove_source(source);
        harness.snapshot =
            StoreSnapshot::from_registry(&ids, [], 1).expect("removed-source snapshot");
        harness.traj_dirty = false;
        let vehicles_before = harness.vehicles.clone();
        let revision_before = harness.vehicle_revision;

        let error = harness
            .apply(VehicleRequest::Set {
                id: added.id,
                patch: VehiclePatch {
                    label: Some("must not commit".into()),
                    ..VehiclePatch::default()
                },
            })
            .unwrap_err();

        assert!(error.contains("gone"), "{error}");
        assert_eq!(harness.vehicles, vehicles_before);
        assert_eq!(harness.vehicle_revision, revision_before);
        assert!(!harness.traj_dirty);
    }

    #[test]
    fn a_stale_vehicle_handle_never_retargets_a_new_vector_entry() {
        let mut harness = VehicleHarness::new(vehicle_snapshot_for(&["flight"]));
        let spec = harness.gps_spec("flight", "old");
        let old = harness.add(spec).unwrap();
        harness
            .apply(VehicleRequest::Remove(VehicleFilter::Id(old.id)))
            .unwrap();
        let spec = harness.gps_spec("flight", "replacement");
        let replacement = harness.add(spec).unwrap();
        assert_ne!(old.id, replacement.id);

        let error = harness
            .apply(VehicleRequest::Set {
                id: old.id,
                patch: VehiclePatch {
                    label: Some("wrong target".into()),
                    ..VehiclePatch::default()
                },
            })
            .unwrap_err();
        assert!(error.contains(&old.id.to_string()), "{error}");
        assert_ne!(harness.vehicles[0].label, "wrong target");
    }

    #[test]
    fn a_mapping_field_from_another_source_is_rejected_atomically() {
        let mut harness = VehicleHarness::new(vehicle_snapshot_for(&["flight_a", "flight_b"]));
        let mut spec = harness.gps_spec("flight_a", "Vehicle");
        let foreign = harness.field("flight_b", "GPS", "alt");
        match &mut spec.position {
            VehiclePosition::Gps { alt, .. } => {
                *alt = ResolvedVehicleField {
                    id: foreign,
                    path: "flight_b/GPS/alt".into(),
                };
            }
            VehiclePosition::Ned { .. } => panic!("GPS fixture returned NED"),
        }
        let error = harness.apply(VehicleRequest::Add(spec)).unwrap_err();
        assert!(error.contains("flight_b/GPS/alt"), "{error}");
        assert!(harness.vehicles.is_empty());
        assert_eq!(harness.vehicle_revision, 0);
    }

    #[test]
    fn remove_by_label_and_source_remove_every_match_but_not_other_vehicles() {
        let mut harness = VehicleHarness::new(vehicle_snapshot_for(&["flight_a", "flight_b"]));
        for (source, label) in [
            ("flight_a", "group"),
            ("flight_a", "group"),
            ("flight_a", "keep"),
            ("flight_b", "keep"),
        ] {
            let spec = harness.gps_spec(source, label);
            harness.add(spec).unwrap();
        }
        harness
            .apply(VehicleRequest::Remove(VehicleFilter::Label("group".into())))
            .unwrap();
        assert_eq!(
            harness
                .vehicles
                .iter()
                .map(|vehicle| vehicle.label.as_str())
                .collect::<Vec<_>>(),
            ["keep", "keep"]
        );
        let source_a = harness.source("flight_a");
        harness
            .apply(VehicleRequest::Remove(VehicleFilter::Source(source_a)))
            .unwrap();
        assert_eq!(harness.vehicles.len(), 1);
        assert_eq!(harness.vehicles[0].source, harness.source("flight_b"));
    }

    fn root_tile(workspace: &crate::shell::workspace::Workspace) -> u64 {
        workspace.plot_infos(0)[0].tile
    }

    fn control_with_one_plot<'a>(
        markers: &'a mut Markers,
        workspace: &'a mut crate::shell::workspace::Workspace,
        windows: &'a mut Vec<crate::shell::windows::ExtendedWindow>,
        playback: &'a mut Playback,
        next_window_id: &'a mut u64,
        caches: &'a mut CacheManager,
        snapshot: &'a StoreSnapshot,
    ) -> AppControl<'a> {
        let (vehicles, next_vehicle_id, vehicle_revision, traj_dirty) = test_vehicle_state();
        AppControl {
            markers,
            workspace,
            windows,
            playback,
            next_window_id,
            caches,
            snapshot,
            vehicles,
            next_vehicle_id,
            vehicle_revision,
            traj_dirty,
            vehicle_profiles: None,
        }
    }

    fn add_request(
        tile: u64,
        field_id: FieldId,
        mode: ScriptTraceMode,
        owner: Option<ScriptOwner>,
    ) -> ControlRequest {
        add_request_with_path(tile, field_id, "IMU.AccX", mode, owner)
    }

    fn add_request_with_path(
        tile: u64,
        field_id: FieldId,
        path: &str,
        mode: ScriptTraceMode,
        owner: Option<ScriptOwner>,
    ) -> ControlRequest {
        ControlRequest::Traces(TraceRequest::Add {
            window: 0,
            tile,
            field_id,
            field: path.into(),
            color: None,
            width_px: None,
            mode,
            owner,
        })
    }

    #[test]
    fn adding_a_trace_uses_the_worker_resolved_field_id_and_stamps_the_owner() {
        let (snapshot, field) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let owner = Some(ScriptOwner {
            name: "flight.py".into(),
            generation: 3,
        });
        apply(
            &mut control,
            add_request(tile, field, ScriptTraceMode::Step, owner.clone()),
        )
        .unwrap();
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert_eq!(pane.traces.len(), 1);
        assert_eq!(pane.traces[0].field, field);
        assert_eq!(pane.traces[0].mode, PlotTraceMode::Step);
        assert_eq!(pane.traces[0].owner, owner);
    }

    #[test]
    fn trace_add_rejects_a_field_id_with_the_wrong_path() {
        let (snapshot, field_id) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let mut request = add_request(tile, field_id, ScriptTraceMode::Line, None);
        if let ControlRequest::Traces(TraceRequest::Add { field, .. }) = &mut request {
            *field = "IMU.Gyro".into();
        }
        let error = apply(&mut control, request).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::StaleHandle);
        assert!(
            control
                .workspace
                .plot_pane_mut(egui_tiles::TileId(tile))
                .unwrap()
                .traces
                .is_empty()
        );
    }

    #[test]
    fn a_rerun_can_re_add_the_same_field_before_the_sweep_removes_its_previous_generation() {
        let (snapshot, field) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        for generation in [1, 2] {
            apply(
                &mut control,
                add_request(
                    tile,
                    field,
                    ScriptTraceMode::Line,
                    Some(ScriptOwner {
                        name: "flight.py".into(),
                        generation,
                    }),
                ),
            )
            .unwrap();
        }
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert_eq!(pane.traces.len(), 2);
        assert!(pane.traces.iter().all(|trace| trace.field == field));
        control_ownership::apply_sweep(
            &mut control,
            &control_ownership::Sweep::Commit {
                owner: "flight.py".into(),
                generation: 2,
            },
        );
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert_eq!(pane.traces.len(), 1);
        assert_eq!(pane.traces[0].owner.as_ref().unwrap().generation, 2);
    }

    #[test]
    fn a_field_id_the_snapshot_no_longer_carries_is_rejected_with_a_readable_error() {
        let (snapshot, field) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let gone = FieldId(field.0 + 1);
        let error = apply(
            &mut control,
            add_request(tile, gone, ScriptTraceMode::Line, None),
        )
        .unwrap_err();
        assert!(error.contains("is gone"), "{error}");
    }

    #[test]
    fn list_reports_traces_with_topic_dot_field_labels_and_ids() {
        let (snapshot, field) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            add_request(tile, field, ScriptTraceMode::Line, None),
        )
        .unwrap();
        let response = apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::List { window: 0, tile }),
        )
        .unwrap();
        let ControlResponse::Traces(infos) = response else {
            panic!("expected Traces response");
        };
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].field, "IMU.AccX");
        assert_eq!(infos[0].field_id, field);
    }

    #[test]
    fn remove_by_field_id_drops_every_matching_trace() {
        let (snapshot, field) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            add_request(tile, field, ScriptTraceMode::Line, None),
        )
        .unwrap();
        apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::Remove {
                window: 0,
                tile,
                index: None,
                field_id: Some(field),
                field: Some("IMU.AccX".into()),
            }),
        )
        .unwrap();
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert!(pane.traces.is_empty());
    }

    #[test]
    fn trace_remove_rejects_wrong_or_missing_field_identity_without_unpinning() {
        let (snapshot, field_id) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            add_request(tile, field_id, ScriptTraceMode::Line, None),
        )
        .unwrap();
        pin(control.caches, field_id);

        for (requested_id, path) in [(field_id, "IMU.Gyro"), (FieldId(u32::MAX), "IMU.AccX")] {
            let error = apply(
                &mut control,
                ControlRequest::Traces(TraceRequest::Remove {
                    window: 0,
                    tile,
                    index: None,
                    field_id: Some(requested_id),
                    field: Some(path.into()),
                }),
            )
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::StaleHandle);
            assert_eq!(
                control
                    .workspace
                    .plot_pane_mut(egui_tiles::TileId(tile))
                    .unwrap()
                    .traces[0]
                    .field,
                field_id
            );
            assert!(control.caches.is_pinned(field_id));
        }
    }

    #[test]
    fn remove_by_index_out_of_range_is_rejected() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let error = apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::Remove {
                window: 0,
                tile,
                index: Some(0),
                field_id: None,
                field: None,
            }),
        )
        .unwrap_err();
        assert!(error.contains("is gone"), "{error}");
    }

    #[test]
    fn set_updates_only_the_given_fields() {
        let (snapshot, field) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            add_request(tile, field, ScriptTraceMode::Line, None),
        )
        .unwrap();
        apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::Set {
                window: 0,
                tile,
                index: 0,
                field_id: field,
                color: None,
                width_px: None,
                mode: None,
                visible: Some(false),
            }),
        )
        .unwrap();
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert!(!pane.traces[0].visible);
        assert_eq!(pane.traces[0].mode, PlotTraceMode::Line);
    }

    #[test]
    fn set_is_rejected_when_the_trace_at_that_index_no_longer_matches_the_handle() {
        let (snapshot, field_a, field_b) =
            snapshot_with_two_fields("flight", "IMU", "AccX", "Gyro");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            add_request(tile, field_a, ScriptTraceMode::Line, None),
        )
        .unwrap();
        apply(
            &mut control,
            add_request_with_path(tile, field_b, "IMU.Gyro", ScriptTraceMode::Line, None),
        )
        .unwrap();
        apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::Remove {
                window: 0,
                tile,
                index: Some(0),
                field_id: None,
                field: None,
            }),
        )
        .unwrap();
        let error = apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::Set {
                window: 0,
                tile,
                index: 0,
                field_id: field_a,
                color: None,
                width_px: None,
                mode: None,
                visible: Some(false),
            }),
        )
        .unwrap_err();
        assert!(error.contains("no longer refers"), "{error}");
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert_eq!(pane.traces[0].field, field_b);
        assert!(pane.traces[0].visible, "the rejected Set must not apply");
    }

    #[test]
    fn a_missing_pane_is_reported_by_window_and_tile() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let (vehicles, next_vehicle_id, vehicle_revision, traj_dirty) = test_vehicle_state();
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles,
            next_vehicle_id,
            vehicle_revision,
            traj_dirty,
            vehicle_profiles: None,
        };
        let error = apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::Clear {
                window: 0,
                tile: 999,
            }),
        )
        .unwrap_err();
        assert!(error.contains("999"), "{error}");
    }

    #[test]
    fn add_plot_reports_the_new_panes_owner() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let owner = Some(ScriptOwner {
            name: "external".into(),
            generation: 7,
        });
        let response = apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::AddPlot {
                window: None,
                owner: owner.clone(),
                direction: ScriptSplitDirection::Horizontal,
            }),
        )
        .unwrap();
        let ControlResponse::Plots(infos) = response else {
            panic!("expected Plots response");
        };
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].owner, owner);
        assert_eq!(control.workspace.plot_infos(0)[1].owner, owner);
        assert_eq!(control.workspace.plot_infos(0).len(), 2);
    }

    #[test]
    fn add_plot_and_equalize_target_the_requested_window() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let window = apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                owner: None,
                title: Some("Second".into()),
            }),
        )
        .unwrap()
        .into_window_info()
        .unwrap()
        .id;
        let main_before = control.workspace.plot_infos(0).len();
        let second_before = control.windows[0].workspace.plot_infos(window).len();
        let response = apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::AddPlot {
                window: Some(window),
                owner: None,
                direction: ScriptSplitDirection::Vertical,
            }),
        )
        .unwrap();
        let ControlResponse::Plots(infos) = response else {
            panic!("expected Plots response");
        };
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].window, window);
        assert_eq!(control.workspace.plot_infos(0).len(), main_before);
        let second = control.windows[0].workspace.plot_infos(window);
        assert_eq!(second.len(), second_before + 1);
        assert!(second.iter().any(|info| info.tile == infos[0].tile));
        assert_eq!(
            apply(
                &mut control,
                ControlRequest::Workspace(WorkspaceRequest::Equalize {
                    window: Some(window)
                }),
            )
            .unwrap(),
            ControlResponse::Unit
        );
        let missing = apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::AddPlot {
                window: Some(window + 40),
                owner: None,
                direction: ScriptSplitDirection::Vertical,
            }),
        )
        .unwrap_err();
        assert!(missing.contains("gone"), "{missing}");
    }

    #[test]
    fn split_reports_the_new_pane_by_window_and_tile() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let response = apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::Split {
                owner: None,
                window: 0,
                tile,
                direction: ScriptSplitDirection::Vertical,
            }),
        )
        .unwrap();
        let ControlResponse::Plots(infos) = response else {
            panic!("expected Plots response");
        };
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].window, 0);
        assert_ne!(infos[0].tile, tile);
    }

    #[test]
    fn split_on_a_missing_pane_is_rejected() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let error = apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::Split {
                owner: None,
                window: 0,
                tile: 999,
                direction: ScriptSplitDirection::Vertical,
            }),
        )
        .unwrap_err();
        assert!(error.contains("999"), "{error}");
    }

    #[test]
    fn close_removes_the_pane_and_leaves_the_workspace_with_one() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::Split {
                owner: None,
                window: 0,
                tile,
                direction: ScriptSplitDirection::Vertical,
            }),
        )
        .unwrap();
        assert_eq!(control.workspace.plot_infos(0).len(), 2);
        apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::Close { window: 0, tile }),
        )
        .unwrap();
        assert_eq!(control.workspace.plot_infos(0).len(), 1);
    }

    #[test]
    fn equalize_reaches_the_workspace_without_error() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let response = apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::Equalize { window: None }),
        )
        .unwrap();
        assert_eq!(response, ControlResponse::Unit);
    }

    #[test]
    fn show_scene_true_twice_stays_idempotent() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        for _ in 0..2 {
            apply(
                &mut control,
                ControlRequest::Workspace(WorkspaceRequest::ShowScene { visible: true }),
            )
            .unwrap();
        }
        assert!(control.workspace.scene_pane_id().is_some());
    }

    #[test]
    fn open_window_returns_its_info_and_stamps_its_initial_plot() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let (vehicles, next_vehicle_id, vehicle_revision, traj_dirty) = test_vehicle_state();
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles,
            next_vehicle_id,
            vehicle_revision,
            traj_dirty,
            vehicle_profiles: None,
        };
        let owner = Some(ScriptOwner {
            name: "external".into(),
            generation: 7,
        });
        let response = apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                owner: owner.clone(),
                title: Some("Compare".into()),
            }),
        )
        .unwrap();
        let info = response.into_window_info().unwrap();
        assert_eq!(info.id, 1);
        assert_eq!(info.title, "Compare");
        assert_eq!(info.owner, owner);
        assert_eq!(control.windows.len(), 1);
        assert_eq!(control.windows[0].id.0, 1);
        assert_eq!(control.windows[0].title, "Compare");
        assert_eq!(control.windows[0].owner, owner);
        assert_eq!(control.windows[0].workspace.plot_infos(1)[0].owner, owner);
        assert_eq!(*control.next_window_id, 2);
    }

    #[test]
    fn playback_set_updates_speed_and_follow_live_independently() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let (vehicles, next_vehicle_id, vehicle_revision, traj_dirty) = test_vehicle_state();
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles,
            next_vehicle_id,
            vehicle_revision,
            traj_dirty,
            vehicle_profiles: None,
        };
        apply(
            &mut control,
            ControlRequest::Playback(PlaybackRequest::Set {
                speed: Some(2.0),
                follow_live: None,
            }),
        )
        .unwrap();
        assert_eq!(control.playback.speed, 2.0);
        assert!(!control.playback.follow_live);
        apply(
            &mut control,
            ControlRequest::Playback(PlaybackRequest::Set {
                speed: None,
                follow_live: Some(true),
            }),
        )
        .unwrap();
        assert_eq!(control.playback.speed, 2.0);
        assert!(control.playback.follow_live);
    }

    #[test]
    fn native_state_queries_include_empty_windows_scene_and_playback() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows = vec![crate::shell::windows::ExtendedWindow::new(
            crate::shell::windows::WindowId(7),
        )];
        let mut playback = Playback::default();
        playback.set_speed(2.0);
        playback.follow_live = true;
        let mut next_window_id = 8;
        let mut caches = CacheManager::new();
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::ShowScene { visible: true }),
        )
        .unwrap();
        let windows = apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::ListWindows),
        )
        .unwrap()
        .into_windows()
        .unwrap();
        assert_eq!(
            windows.iter().map(|window| window.id).collect::<Vec<_>>(),
            [0, 7]
        );
        assert!(
            apply(
                &mut control,
                ControlRequest::Workspace(WorkspaceRequest::GetState)
            )
            .unwrap()
            .into_workspace()
            .unwrap()
            .scene_visible
        );
        let playback = apply(&mut control, ControlRequest::Playback(PlaybackRequest::Get))
            .unwrap()
            .into_playback()
            .unwrap();
        assert_eq!(playback.speed, 2.0);
        assert!(playback.follow_live);
    }

    fn pin(caches: &mut CacheManager, field: FieldId) {
        caches.request(field, &Arc::new(StoreSnapshot::empty()));
        assert!(caches.is_pinned(field));
    }

    #[test]
    fn closing_a_plot_unpins_the_fields_it_was_plotting() {
        let (snapshot, field) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            add_request(tile, field, ScriptTraceMode::Line, None),
        )
        .unwrap();
        pin(control.caches, field);
        apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::Close { window: 0, tile }),
        )
        .unwrap();
        assert!(!control.caches.is_pinned(field));
    }

    #[test]
    fn removing_a_trace_by_index_unpins_its_field() {
        let (snapshot, field) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            add_request(tile, field, ScriptTraceMode::Line, None),
        )
        .unwrap();
        pin(control.caches, field);
        apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::Remove {
                window: 0,
                tile,
                index: Some(0),
                field_id: None,
                field: None,
            }),
        )
        .unwrap();
        assert!(!control.caches.is_pinned(field));
    }

    #[test]
    fn removing_a_trace_by_field_id_unpins_its_field() {
        let (snapshot, field) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            add_request(tile, field, ScriptTraceMode::Line, None),
        )
        .unwrap();
        pin(control.caches, field);
        apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::Remove {
                window: 0,
                tile,
                index: None,
                field_id: Some(field),
                field: Some("IMU.AccX".into()),
            }),
        )
        .unwrap();
        assert!(!control.caches.is_pinned(field));
    }

    #[test]
    fn removing_by_a_field_id_with_no_matching_trace_does_not_unpin_anything() {
        let (snapshot, field) = snapshot_with_field("flight", "IMU", "AccX");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        pin(control.caches, field);
        apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::Remove {
                window: 0,
                tile,
                index: None,
                field_id: Some(field),
                field: Some("IMU.AccX".into()),
            }),
        )
        .unwrap();
        assert!(
            control.caches.is_pinned(field),
            "nothing was actually removed, so the field must stay pinned"
        );
    }

    #[test]
    fn clearing_a_plot_unpins_every_field_it_was_plotting() {
        let (snapshot, field_a, field_b) =
            snapshot_with_two_fields("flight", "IMU", "AccX", "Gyro");
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        apply(
            &mut control,
            add_request(tile, field_a, ScriptTraceMode::Line, None),
        )
        .unwrap();
        apply(
            &mut control,
            add_request_with_path(tile, field_b, "IMU.Gyro", ScriptTraceMode::Line, None),
        )
        .unwrap();
        pin(control.caches, field_a);
        pin(control.caches, field_b);
        apply(
            &mut control,
            ControlRequest::Traces(TraceRequest::Clear { window: 0, tile }),
        )
        .unwrap();
        assert!(!control.caches.is_pinned(field_a));
        assert!(!control.caches.is_pinned(field_b));
    }

    fn hline_at(y: f64) -> AnnotationGeometry {
        AnnotationGeometry::HLine { y }
    }

    #[test]
    fn adding_an_annotation_with_no_style_args_matches_a_hand_drawn_default() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let response = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Add {
                window: 0,
                tile,
                geometry: hline_at(9.81),
                label: "1g".into(),
                style: AnnotationStylePatch::default(),
                owner: None,
            }),
        )
        .unwrap();
        let ControlResponse::Annotations(infos) = response else {
            panic!("expected Annotations response");
        };
        let id = infos[0].id;
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        let annotation = pane.annotations.get(id).unwrap();
        assert_eq!(
            annotation.geom,
            crate::plotting::annotations::Geometry::HLine { y: 9.81 }
        );
        assert_eq!(annotation.label, "1g");
        assert_eq!(
            annotation.style,
            crate::plotting::annotations::default_style(id)
        );
        assert_eq!(annotation.owner, None);
    }

    #[test]
    fn a_style_patch_overrides_only_the_fields_it_sets() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let response = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Add {
                window: 0,
                tile,
                geometry: hline_at(9.81),
                label: String::new(),
                style: AnnotationStylePatch {
                    fill_opacity: Some(0.15),
                    arrow: Some(true),
                    ..Default::default()
                },
                owner: None,
            }),
        )
        .unwrap();
        let ControlResponse::Annotations(infos) = response else {
            panic!("expected Annotations response");
        };
        let id = infos[0].id;
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        let annotation = pane.annotations.get(id).unwrap();
        let default = crate::plotting::annotations::default_style(id);
        assert_eq!(annotation.style.fill_opacity, 0.15);
        assert!(annotation.style.arrow);
        assert_eq!(annotation.style.color, default.color);
        assert_eq!(annotation.style.stroke_px, default.stroke_px);
        assert_eq!(annotation.style.font_px, default.font_px);
    }

    #[test]
    fn a_scripted_annotation_is_stamped_with_its_owner() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let owner = Some(ScriptOwner {
            name: "flight.py".into(),
            generation: 4,
        });
        let response = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Add {
                window: 0,
                tile,
                geometry: hline_at(9.81),
                label: String::new(),
                style: AnnotationStylePatch::default(),
                owner: owner.clone(),
            }),
        )
        .unwrap();
        let ControlResponse::Annotations(infos) = response else {
            panic!("expected Annotations response");
        };
        let id = infos[0].id;
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert_eq!(pane.annotations.get(id).unwrap().owner, owner);
    }

    #[test]
    fn adding_an_annotation_to_a_missing_pane_is_rejected() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let error = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Add {
                window: 0,
                tile: 999,
                geometry: hline_at(9.81),
                label: String::new(),
                style: AnnotationStylePatch::default(),
                owner: None,
            }),
        )
        .unwrap_err();
        assert!(error.contains("999"), "{error}");
    }

    fn add_annotation(
        control: &mut AppControl<'_>,
        window: u64,
        tile: u64,
        geometry: AnnotationGeometry,
        label: &str,
        owner: Option<ScriptOwner>,
    ) -> u64 {
        let response = apply(
            control,
            ControlRequest::Annotations(AnnotationRequest::Add {
                window,
                tile,
                geometry,
                label: label.into(),
                style: AnnotationStylePatch::default(),
                owner,
            }),
        )
        .unwrap();
        let ControlResponse::Annotations(infos) = response else {
            panic!("expected Annotations response");
        };
        infos[0].id
    }

    #[test]
    fn listing_annotations_reports_index_and_kind_in_creation_order() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let rect_id = add_annotation(
            &mut control,
            0,
            tile,
            AnnotationGeometry::Rect {
                a: (0, 0.0),
                b: (1, 1.0),
            },
            "box",
            None,
        );
        let hline_id = add_annotation(&mut control, 0, tile, hline_at(9.81), "1g", None);
        let response = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::List {
                target: Some((0, tile)),
            }),
        )
        .unwrap();
        let ControlResponse::Annotations(infos) = response else {
            panic!("expected Annotations response");
        };
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].id, rect_id);
        assert_eq!(infos[0].index, 0);
        assert_eq!(infos[0].kind, AnnotationKind::Rect);
        assert_eq!(infos[1].id, hline_id);
        assert_eq!(infos[1].index, 1);
        assert_eq!(infos[1].kind, AnnotationKind::HLine);
    }

    #[test]
    fn listing_annotations_walks_every_window_when_no_target_is_given() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows = vec![crate::shell::windows::ExtendedWindow::new(
            crate::shell::windows::WindowId(1),
        )];
        let mut playback = Playback::default();
        let mut next_window_id = 2u64;
        let mut caches = CacheManager::new();
        let main_tile = root_tile(&workspace);
        let other_tile = windows[0].workspace.plot_infos(1)[0].tile;
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        add_annotation(&mut control, 0, main_tile, hline_at(1.0), "main", None);
        add_annotation(&mut control, 1, other_tile, hline_at(2.0), "other", None);
        let response = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::List { target: None }),
        )
        .unwrap();
        let ControlResponse::Annotations(infos) = response else {
            panic!("expected Annotations response");
        };
        let windows_seen: Vec<u64> = infos.iter().map(|info| info.window).collect();
        assert_eq!(windows_seen, vec![0, 1]);
    }

    #[test]
    fn removing_by_index_removes_only_that_annotation() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        add_annotation(&mut control, 0, tile, hline_at(1.0), "first", None);
        let second = add_annotation(&mut control, 0, tile, hline_at(2.0), "second", None);
        apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Index(0),
            }),
        )
        .unwrap();
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert_eq!(pane.annotations.items().len(), 1);
        assert_eq!(pane.annotations.items()[0].id, second);
    }

    #[test]
    fn removing_an_out_of_range_index_is_rejected() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let error = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Index(0),
            }),
        )
        .unwrap_err();
        assert!(error.contains("is gone"), "{error}");
    }

    #[test]
    fn removing_a_stale_id_is_rejected_and_never_touches_another_annotation() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let stale = add_annotation(&mut control, 0, tile, hline_at(1.0), "gone", None);
        let survivor = add_annotation(&mut control, 0, tile, hline_at(2.0), "survivor", None);
        apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Id(stale),
            }),
        )
        .unwrap();
        let error = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Id(stale),
            }),
        )
        .unwrap_err();
        assert!(error.contains("is gone"), "{error}");
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert_eq!(pane.annotations.items().len(), 1);
        assert_eq!(pane.annotations.items()[0].id, survivor);
    }

    #[test]
    fn removing_by_kind_only_matches_that_kind() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        add_annotation(&mut control, 0, tile, hline_at(1.0), "hline", None);
        let rect = add_annotation(
            &mut control,
            0,
            tile,
            AnnotationGeometry::Rect {
                a: (0, 0.0),
                b: (1, 1.0),
            },
            "rect",
            None,
        );
        apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Kind(AnnotationKind::HLine),
            }),
        )
        .unwrap();
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert_eq!(pane.annotations.items().len(), 1);
        assert_eq!(pane.annotations.items()[0].id, rect);
    }

    #[test]
    fn a_global_removal_by_owner_reaches_every_window() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows = vec![crate::shell::windows::ExtendedWindow::new(
            crate::shell::windows::WindowId(1),
        )];
        let mut playback = Playback::default();
        let mut next_window_id = 2u64;
        let mut caches = CacheManager::new();
        let main_tile = root_tile(&workspace);
        let other_tile = windows[0].workspace.plot_infos(1)[0].tile;
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let owner = Some(ScriptOwner {
            name: "flight.py".into(),
            generation: 1,
        });
        add_annotation(
            &mut control,
            0,
            main_tile,
            hline_at(1.0),
            "main",
            owner.clone(),
        );
        add_annotation(
            &mut control,
            1,
            other_tile,
            hline_at(2.0),
            "other",
            owner.clone(),
        );
        apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: None,
                filter: AnnotationFilter::Owner("flight.py".into()),
            }),
        )
        .unwrap();
        assert!(
            control
                .workspace
                .plot_pane_mut(egui_tiles::TileId(main_tile))
                .unwrap()
                .annotations
                .is_empty()
        );
        assert!(
            control.windows[0]
                .workspace
                .plot_pane_mut(egui_tiles::TileId(other_tile))
                .unwrap()
                .annotations
                .is_empty()
        );
    }

    #[test]
    fn a_global_removal_by_index_or_id_is_rejected_before_touching_any_pane() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        add_annotation(&mut control, 0, tile, hline_at(1.0), "kept", None);
        let index_error = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: None,
                filter: AnnotationFilter::Index(0),
            }),
        )
        .unwrap_err();
        assert!(index_error.contains("index or id"), "{index_error}");
        let id_error = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: None,
                filter: AnnotationFilter::Id(0),
            }),
        )
        .unwrap_err();
        assert!(id_error.contains("index or id"), "{id_error}");
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert_eq!(pane.annotations.items().len(), 1);
    }

    #[test]
    fn removing_the_selected_annotation_clears_selection_state() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let id = add_annotation(&mut control, 0, tile, hline_at(1.0), "1g", None);
        {
            let pane = control
                .workspace
                .plot_pane_mut(egui_tiles::TileId(tile))
                .unwrap();
            pane.annotations.selected = Some(id);
        }
        apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Kind(AnnotationKind::HLine),
            }),
        )
        .unwrap();
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        assert_eq!(pane.annotations.selected, None);
    }

    #[test]
    fn set_updates_label_geometry_and_style() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let id = add_annotation(&mut control, 0, tile, hline_at(1.0), "1g", None);
        apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Set {
                window: 0,
                tile,
                id,
                label: Some("burst".into()),
                geometry: Some(hline_at(9.81)),
                style: AnnotationStylePatch {
                    arrow: Some(true),
                    ..Default::default()
                },
            }),
        )
        .unwrap();
        let pane = control
            .workspace
            .plot_pane_mut(egui_tiles::TileId(tile))
            .unwrap();
        let annotation = pane.annotations.get(id).unwrap();
        assert_eq!(annotation.label, "burst");
        assert_eq!(
            annotation.geom,
            crate::plotting::annotations::Geometry::HLine { y: 9.81 }
        );
        assert!(annotation.style.arrow);
    }

    #[test]
    fn set_on_a_removed_annotation_is_rejected() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let tile = root_tile(&workspace);
        let mut control = control_with_one_plot(
            &mut markers,
            &mut workspace,
            &mut windows,
            &mut playback,
            &mut next_window_id,
            &mut caches,
            &snapshot,
        );
        let id = add_annotation(&mut control, 0, tile, hline_at(1.0), "1g", None);
        apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Id(id),
            }),
        )
        .unwrap();
        let error = apply(
            &mut control,
            ControlRequest::Annotations(AnnotationRequest::Set {
                window: 0,
                tile,
                id,
                label: Some("burst".into()),
                geometry: None,
                style: AnnotationStylePatch::default(),
            }),
        )
        .unwrap_err();
        assert!(error.contains("is gone"), "{error}");
    }
}
