use delog_cache::CacheManager;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;
use delog_script::{
    AnnotationFilter, AnnotationRequest, ControlRequest, ControlResponse, PlaybackRequest,
    PlotRequest, SplitDirection as ScriptSplitDirection, TraceInfo, TraceMode as ScriptTraceMode,
    TraceRequest, WorkspaceRequest,
};

use crate::plotting::legend::trace_label;
use crate::plotting::markers::Markers;
use crate::plotting::plot::{PlotPane, TraceMode as PlotTraceMode, TraceRef};
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
}

pub fn apply(
    control: &mut AppControl<'_>,
    request: ControlRequest,
) -> Result<ControlResponse, String> {
    match request {
        ControlRequest::Markers(request) => {
            control.markers.apply_control_request(request);
            Ok(ControlResponse::Unit)
        }
        ControlRequest::Plots(PlotRequest::List { window }) => {
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
            control_ownership::apply_sweep(control, &sweep);
            Ok(ControlResponse::Unit)
        }
        ControlRequest::Workspace(request) => apply_workspace_request(control, request),
        ControlRequest::Playback(request) => apply_playback_request(control, request),
    }
}

fn workspace_for<'a>(
    control: &'a mut AppControl<'_>,
    window: u64,
) -> Result<&'a mut Workspace, String> {
    if window == 0 {
        Ok(&mut *control.workspace)
    } else {
        Ok(&mut control
            .windows
            .iter_mut()
            .find(|w| w.id.0 == window)
            .ok_or_else(|| format!("window {window} is gone"))?
            .workspace)
    }
}

fn plot_pane<'a>(
    control: &'a mut AppControl<'_>,
    window: u64,
    tile: u64,
) -> Result<&'a mut PlotPane, String> {
    workspace_for(control, window)?
        .plot_pane_mut(egui_tiles::TileId(tile))
        .ok_or_else(|| format!("plot {tile} in window {window} is gone"))
}

fn plot_info_for(
    workspace: &Workspace,
    window: u64,
    tile: egui_tiles::TileId,
) -> Result<delog_script::PlotInfo, String> {
    workspace
        .plot_infos(window)
        .into_iter()
        .find(|info| info.tile == tile.0)
        .ok_or_else(|| format!("plot {} in window {window} is gone", tile.0))
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
) -> Result<ControlResponse, String> {
    match request {
        WorkspaceRequest::AddPlot { direction } => {
            let target = control
                .workspace
                .focused_plot_id()
                .or_else(|| control.workspace.tree.root())
                .ok_or_else(|| "the workspace has no panes to split".to_string())?;
            let new_tile = control
                .workspace
                .split_plot(target, app_split_direction(direction))
                .ok_or_else(|| "the workspace could not add a plot".to_string())?;
            Ok(ControlResponse::Plots(vec![plot_info_for(
                control.workspace,
                0,
                new_tile,
            )?]))
        }
        WorkspaceRequest::Split {
            window,
            tile,
            direction,
        } => {
            let workspace = workspace_for(control, window)?;
            if workspace.plot_pane_mut(egui_tiles::TileId(tile)).is_none() {
                return Err(format!("plot {tile} in window {window} is gone"));
            }
            let new_tile = workspace
                .split_plot(egui_tiles::TileId(tile), app_split_direction(direction))
                .ok_or_else(|| format!("plot {tile} in window {window} could not be split"))?;
            Ok(ControlResponse::Plots(vec![plot_info_for(
                workspace, window, new_tile,
            )?]))
        }
        WorkspaceRequest::Close { window, tile } => {
            let workspace = workspace_for(control, window)?;
            if workspace.plot_pane_mut(egui_tiles::TileId(tile)).is_none() {
                return Err(format!("plot {tile} in window {window} is gone"));
            }
            let removed = workspace.close_plot(egui_tiles::TileId(tile));
            for field in removed {
                control.caches.unpin(field);
            }
            Ok(ControlResponse::Unit)
        }
        WorkspaceRequest::Equalize => {
            control.workspace.equalize_plot_heights();
            Ok(ControlResponse::Unit)
        }
        WorkspaceRequest::ShowScene { visible } => {
            if control.workspace.scene_pane_id().is_some() != visible {
                control.workspace.toggle_scene_pane();
            }
            Ok(ControlResponse::Unit)
        }
        WorkspaceRequest::OpenWindow { title } => {
            let id =
                crate::shell::windows::open_window(control.windows, control.next_window_id, title);
            Ok(ControlResponse::Window(id.0))
        }
    }
}

fn apply_playback_request(
    control: &mut AppControl<'_>,
    request: PlaybackRequest,
) -> Result<ControlResponse, String> {
    match request {
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

fn apply_trace_request(
    control: &mut AppControl<'_>,
    request: TraceRequest,
) -> Result<ControlResponse, String> {
    match request {
        TraceRequest::List { window, tile } => {
            let snapshot = control.snapshot;
            let pane = plot_pane(control, window, tile)?;
            Ok(ControlResponse::Traces(trace_info_list(pane, snapshot)))
        }
        TraceRequest::Add {
            window,
            tile,
            field_id,
            field,
            color,
            width_px,
            mode,
            owner,
        } => {
            if !field_exists(control.snapshot, field_id) {
                return Err(format!("field '{field}' is gone"));
            }
            let pane = plot_pane(control, window, tile)?;
            let color = color.unwrap_or_else(|| {
                delog_render::palette::trace_color(pane.traces.len()).to_srgb_f32()
            });
            pane.traces.push(TraceRef {
                field: field_id,
                color,
                width_px: width_px.unwrap_or(1.5),
                mode: plot_trace_mode(mode),
                visible: true,
                label_override: None,
                owner,
            });
            Ok(ControlResponse::Unit)
        }
        TraceRequest::Remove {
            window,
            tile,
            index,
            field_id,
            field: _,
        } => {
            let pane = plot_pane(control, window, tile)?;
            let removed = match (index, field_id) {
                (Some(index), _) => {
                    if index >= pane.traces.len() {
                        return Err(format!(
                            "trace {index} on plot {tile} in window {window} is gone"
                        ));
                    }
                    Some(pane.traces.remove(index).field)
                }
                (None, Some(field_id)) => {
                    let existed = pane.traces.iter().any(|trace| trace.field == field_id);
                    pane.traces.retain(|trace| trace.field != field_id);
                    existed.then_some(field_id)
                }
                (None, None) => None,
            };
            if let Some(field) = removed {
                control.caches.unpin(field);
            }
            Ok(ControlResponse::Unit)
        }
        TraceRequest::Clear { window, tile } => {
            let pane = plot_pane(control, window, tile)?;
            let removed: Vec<FieldId> = pane.traces.iter().map(|trace| trace.field).collect();
            pane.traces.clear();
            for field in removed {
                control.caches.unpin(field);
            }
            Ok(ControlResponse::Unit)
        }
        TraceRequest::Set {
            window,
            tile,
            index,
            field_id,
            color,
            width_px,
            mode,
            visible,
        } => {
            let pane = plot_pane(control, window, tile)?;
            let trace = pane.traces.get_mut(index).ok_or_else(|| {
                format!("trace {index} on plot {tile} in window {window} is gone")
            })?;
            if trace.field != field_id {
                return Err(format!(
                    "trace {index} on plot {tile} in window {window} no longer refers to the field this handle was created for"
                ));
            }
            if let Some(color) = color {
                trace.color = color;
            }
            if let Some(width_px) = width_px {
                trace.width_px = width_px;
            }
            if let Some(mode) = mode {
                trace.mode = plot_trace_mode(mode);
            }
            if let Some(visible) = visible {
                trace.visible = visible;
            }
            Ok(ControlResponse::Unit)
        }
    }
}

fn apply_annotation_request(
    control: &mut AppControl<'_>,
    request: AnnotationRequest,
) -> Result<ControlResponse, String> {
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
            annotation.owner = owner.map(Into::into);
            let info = delog_script::AnnotationInfo {
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
                        return Err(
                            "a global annotation removal cannot address a single annotation by index or id".into(),
                        );
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
                format!("annotation {id} on plot {tile} in window {window} is gone")
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

fn remove_matching_annotations(
    pane: &mut PlotPane,
    filter: &AnnotationFilter,
) -> Result<(), String> {
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
                .ok_or_else(|| format!("annotation {index} is gone"))?;
            pane.annotations.remove(id);
            Ok(())
        }
        AnnotationFilter::Id(id) => {
            if pane.annotations.get(*id).is_none() {
                return Err(format!("annotation {id} is gone"));
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

fn field_exists(snapshot: &StoreSnapshot, field_id: FieldId) -> bool {
    snapshot
        .fields
        .get(field_id.index())
        .is_some_and(|field| field.id == field_id && !field.removed)
}

fn trace_info_list(pane: &PlotPane, snapshot: &StoreSnapshot) -> Vec<TraceInfo> {
    pane.traces
        .iter()
        .enumerate()
        .map(|(index, trace)| TraceInfo {
            index,
            field_id: trace.field,
            field: trace_label(snapshot, trace.field),
            color: trace.color,
            width_px: trace.width_px,
            mode: script_trace_mode(trace.mode),
            visible: trace.visible,
        })
        .collect()
}

fn script_trace_mode(mode: PlotTraceMode) -> ScriptTraceMode {
    match mode {
        PlotTraceMode::Line => ScriptTraceMode::Line,
        PlotTraceMode::Scatter => ScriptTraceMode::Scatter,
        PlotTraceMode::Step => ScriptTraceMode::Step,
    }
}

fn plot_trace_mode(mode: ScriptTraceMode) -> PlotTraceMode {
    match mode {
        ScriptTraceMode::Line => PlotTraceMode::Line,
        ScriptTraceMode::Scatter => PlotTraceMode::Scatter,
        ScriptTraceMode::Step => PlotTraceMode::Step,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use delog_script::{MarkerRequest, PendingMarker};

    fn marker(time_us: i64, label: &str) -> PendingMarker {
        PendingMarker {
            time_us,
            label: label.into(),
            color: None,
            note: String::new(),
        }
    }

    #[test]
    fn a_marker_replace_request_reaches_the_marker_store() {
        let mut markers = crate::plotting::markers::Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let snapshot = StoreSnapshot::empty();
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
        };
        let response = apply(
            &mut control,
            ControlRequest::Markers(MarkerRequest::Replace {
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
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
        };
        for owner in ["flight.py", "other.py"] {
            apply(
                &mut control,
                ControlRequest::Markers(MarkerRequest::Replace {
                    owner: owner.into(),
                    generation: 1,
                    markers: vec![marker(10, owner)],
                }),
            )
            .unwrap();
        }
        apply(
            &mut control,
            ControlRequest::Markers(MarkerRequest::Replace {
                owner: "flight.py".into(),
                generation: 2,
                markers: vec![marker(20, "new")],
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
        AppControl {
            markers,
            workspace,
            windows,
            playback,
            next_window_id,
            caches,
            snapshot,
        }
    }

    fn add_request(
        tile: u64,
        field_id: FieldId,
        mode: ScriptTraceMode,
        owner: Option<delog_script::ScriptOwner>,
    ) -> ControlRequest {
        ControlRequest::Traces(TraceRequest::Add {
            window: 0,
            tile,
            field_id,
            field: "IMU.AccX".into(),
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
        let owner = Some(delog_script::ScriptOwner {
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
                    Some(delog_script::ScriptOwner {
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
            add_request(tile, field_b, ScriptTraceMode::Line, None),
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
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
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
    fn add_plot_splits_the_root_and_reports_the_new_planes_info() {
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
            ControlRequest::Workspace(WorkspaceRequest::AddPlot {
                direction: ScriptSplitDirection::Horizontal,
            }),
        )
        .unwrap();
        let ControlResponse::Plots(infos) = response else {
            panic!("expected Plots response");
        };
        assert_eq!(infos.len(), 1);
        assert_eq!(control.workspace.plot_infos(0).len(), 2);
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
            ControlRequest::Workspace(WorkspaceRequest::Equalize),
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
    fn open_window_creates_a_new_window_and_returns_its_id() {
        let snapshot = StoreSnapshot::empty();
        let mut markers = Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        let mut playback = Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = CacheManager::new();
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
        };
        let response = apply(
            &mut control,
            ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                title: Some("Compare".into()),
            }),
        )
        .unwrap();
        assert_eq!(response, ControlResponse::Window(1));
        assert_eq!(control.windows.len(), 1);
        assert_eq!(control.windows[0].id.0, 1);
        assert_eq!(control.windows[0].title, "Compare");
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
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
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
            add_request(tile, field_b, ScriptTraceMode::Line, None),
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

    fn hline_at(y: f64) -> delog_script::AnnotationGeometry {
        delog_script::AnnotationGeometry::HLine { y }
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Add {
                window: 0,
                tile,
                geometry: hline_at(9.81),
                label: "1g".into(),
                style: delog_script::AnnotationStylePatch::default(),
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Add {
                window: 0,
                tile,
                geometry: hline_at(9.81),
                label: String::new(),
                style: delog_script::AnnotationStylePatch {
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
        let owner = Some(delog_script::ScriptOwner {
            name: "flight.py".into(),
            generation: 4,
        });
        let response = apply(
            &mut control,
            ControlRequest::Annotations(delog_script::AnnotationRequest::Add {
                window: 0,
                tile,
                geometry: hline_at(9.81),
                label: String::new(),
                style: delog_script::AnnotationStylePatch::default(),
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
        assert_eq!(
            pane.annotations.get(id).unwrap().owner,
            owner.map(Into::into)
        );
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Add {
                window: 0,
                tile: 999,
                geometry: hline_at(9.81),
                label: String::new(),
                style: delog_script::AnnotationStylePatch::default(),
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
        geometry: delog_script::AnnotationGeometry,
        label: &str,
        owner: Option<delog_script::ScriptOwner>,
    ) -> u64 {
        let response = apply(
            control,
            ControlRequest::Annotations(delog_script::AnnotationRequest::Add {
                window,
                tile,
                geometry,
                label: label.into(),
                style: delog_script::AnnotationStylePatch::default(),
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
            delog_script::AnnotationGeometry::Rect {
                a: (0, 0.0),
                b: (1, 1.0),
            },
            "box",
            None,
        );
        let hline_id = add_annotation(&mut control, 0, tile, hline_at(9.81), "1g", None);
        let response = apply(
            &mut control,
            ControlRequest::Annotations(delog_script::AnnotationRequest::List {
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
        assert_eq!(infos[0].kind, delog_script::AnnotationKind::Rect);
        assert_eq!(infos[1].id, hline_id);
        assert_eq!(infos[1].index, 1);
        assert_eq!(infos[1].kind, delog_script::AnnotationKind::HLine);
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::List { target: None }),
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Remove {
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Remove {
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Id(stale),
            }),
        )
        .unwrap();
        let error = apply(
            &mut control,
            ControlRequest::Annotations(delog_script::AnnotationRequest::Remove {
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
            delog_script::AnnotationGeometry::Rect {
                a: (0, 0.0),
                b: (1, 1.0),
            },
            "rect",
            None,
        );
        apply(
            &mut control,
            ControlRequest::Annotations(delog_script::AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Kind(delog_script::AnnotationKind::HLine),
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
        let owner = Some(delog_script::ScriptOwner {
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Remove {
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Remove {
                target: None,
                filter: AnnotationFilter::Index(0),
            }),
        )
        .unwrap_err();
        assert!(index_error.contains("index or id"), "{index_error}");
        let id_error = apply(
            &mut control,
            ControlRequest::Annotations(delog_script::AnnotationRequest::Remove {
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Kind(delog_script::AnnotationKind::HLine),
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Set {
                window: 0,
                tile,
                id,
                label: Some("burst".into()),
                geometry: Some(hline_at(9.81)),
                style: delog_script::AnnotationStylePatch {
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
            ControlRequest::Annotations(delog_script::AnnotationRequest::Remove {
                target: Some((0, tile)),
                filter: AnnotationFilter::Id(id),
            }),
        )
        .unwrap();
        let error = apply(
            &mut control,
            ControlRequest::Annotations(delog_script::AnnotationRequest::Set {
                window: 0,
                tile,
                id,
                label: Some("burst".into()),
                geometry: None,
                style: delog_script::AnnotationStylePatch::default(),
            }),
        )
        .unwrap_err();
        assert!(error.contains("is gone"), "{error}");
    }
}
