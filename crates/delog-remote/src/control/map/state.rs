use delog_api::color::format_hex_color;
use delog_api::control::{
    AnnotationRequest, ControlRequest, LayoutRequest, MarkerRequest, PlaybackRequest, PlotRequest,
    TraceRequest, VehicleRequest, WorkspaceRequest,
};
use std::collections::HashSet;

use super::ControlMapper;
use super::convert::{geometry_dto, us_to_ns, vehicle_orientation_json, vehicle_position_json};
use crate::control::handles::{HandleTarget, NativePlotKey};
use crate::protocol::v1::control::*;
use crate::protocol::v1::error::ApiError;

impl ControlMapper<'_> {
    pub fn state(&mut self) -> Result<ControlStateDto, ApiError> {
        let windows = self
            .call(ControlRequest::Workspace(WorkspaceRequest::ListWindows))?
            .into_windows()
            .map_err(ApiError::from)?;
        let workspace = self
            .call(ControlRequest::Workspace(WorkspaceRequest::GetState))?
            .into_workspace()
            .map_err(ApiError::from)?;
        let playback = self
            .call(ControlRequest::Playback(PlaybackRequest::Get))?
            .into_playback()
            .map_err(ApiError::from)?;
        let plots = self
            .call(ControlRequest::Plots(PlotRequest::List { window: None }))?
            .into_plots()
            .map_err(ApiError::from)?;
        let mut traces = Vec::new();
        for plot in &plots {
            let infos = self
                .call(ControlRequest::Traces(TraceRequest::List {
                    window: plot.window,
                    tile: plot.tile,
                }))?
                .into_traces()
                .map_err(ApiError::from)?;
            traces.extend(infos.into_iter().map(|info| {
                (
                    NativePlotKey::with_instance_id(plot.window, plot.tile, plot.instance_id),
                    info,
                )
            }));
        }
        let annotations = self
            .call(ControlRequest::Annotations(AnnotationRequest::List {
                target: None,
            }))?
            .into_annotations()
            .map_err(ApiError::from)?;
        let markers = self
            .call(ControlRequest::Markers(MarkerRequest::List))?
            .into_markers()
            .map_err(ApiError::from)?;
        let vehicles = self
            .call(ControlRequest::Vehicles(Box::new(VehicleRequest::List)))?
            .into_vehicles()
            .map_err(ApiError::from)?;
        let layout_names = self
            .call(ControlRequest::Layouts(LayoutRequest::List))?
            .into_names()
            .map_err(ApiError::from)?;
        let current_layout = self
            .call(ControlRequest::Layouts(LayoutRequest::Current))?
            .into_layout()
            .map_err(ApiError::from)?;

        let mut live = HashSet::new();
        live.extend(
            windows
                .iter()
                .map(|item| HandleTarget::Window { id: item.id }),
        );
        live.extend(plots.iter().map(|item| HandleTarget::Plot {
            window: item.window,
            tile: item.tile,
            instance_id: item.instance_id,
        }));
        live.extend(traces.iter().map(|(key, item)| HandleTarget::Trace {
            window: key.window,
            tile: key.tile,
            index: item.index,
            field: item.field_id,
            plot_instance_id: key.instance_id,
            trace_instance_id: item.instance_id,
        }));
        live.extend(annotations.iter().map(|item| HandleTarget::Annotation {
            window: item.window,
            tile: item.tile,
            id: item.id,
            plot_instance_id: item.plot_instance_id,
        }));
        live.extend(
            markers
                .iter()
                .map(|item| HandleTarget::Marker { id: item.id }),
        );
        live.extend(
            vehicles
                .iter()
                .map(|item| HandleTarget::Vehicle { id: item.id }),
        );
        self.handles.retain_live(&live);

        let windows = windows
            .into_iter()
            .map(|item| WindowStateDto {
                handle: self.handles.register_window(item.id),
                title: item.title,
                owner: item.owner.map(|owner| owner.name),
            })
            .collect();
        let plots = plots
            .into_iter()
            .map(|item| PlotStateDto {
                handle: self.handles.register_plot(NativePlotKey::with_instance_id(
                    item.window,
                    item.tile,
                    item.instance_id,
                )),
                window: self.handles.register_window(item.window),
                label: item.label,
                owner: item.owner.map(|owner| owner.name),
            })
            .collect();
        let traces = traces
            .into_iter()
            .map(|(key, item)| TraceStateDto {
                handle: self.handles.register_trace(
                    key,
                    item.index,
                    item.field_id,
                    item.instance_id,
                ),
                plot: self.handles.register_plot(key),
                field: self.fields.describe(item.field_id).unwrap_or(item.field),
                color: format_hex_color(item.color),
                width_px: item.width_px,
                mode: item.mode.as_str().into(),
                visible: item.visible,
                owner: item.owner.map(|owner| owner.name),
            })
            .collect();
        let annotations = annotations
            .into_iter()
            .map(|item| -> Result<_, ApiError> {
                let key =
                    NativePlotKey::with_instance_id(item.window, item.tile, item.plot_instance_id);
                Ok(AnnotationStateDto {
                    handle: self.handles.register_annotation(key, item.id),
                    plot: self.handles.register_plot(key),
                    geometry: geometry_dto(item.geometry)?,
                    label: item.label,
                    color: format_hex_color(item.color),
                    owner: item.owner,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let markers = markers
            .into_iter()
            .map(|item| -> Result<_, ApiError> {
                Ok(MarkerStateDto {
                    handle: self.handles.register_marker(item.id),
                    time_ns: us_to_ns(item.t_us)?,
                    label: item.label,
                    color: format_hex_color(item.color),
                    note: item.note,
                    origin: item.origin.as_str().into(),
                    owner: item.owner,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let vehicles = vehicles
            .into_iter()
            .map(|item| VehicleStateDto {
                handle: self.handles.register_vehicle(item.id),
                source: item.spec.source,
                label: item.spec.label,
                show: item.spec.show,
                show_path: item.spec.show_path,
                position: vehicle_position_json(item.spec.position),
                orientation: vehicle_orientation_json(item.spec.orientation),
                model: item.spec.model.as_str().into(),
                color: format_hex_color(item.spec.color),
                path_color: format_hex_color(item.spec.path_color),
                scale: item.spec.scale,
                owner: item.spec.owner.map(|owner| owner.name),
            })
            .collect();
        Ok(ControlStateDto {
            windows,
            plots,
            traces,
            annotations,
            markers,
            vehicles,
            layout_names,
            current_layout,
            playback: PlaybackStateDto {
                speed: playback.speed,
                follow_live: playback.follow_live,
            },
            scene_visible: workspace.scene_visible,
        })
    }
}
