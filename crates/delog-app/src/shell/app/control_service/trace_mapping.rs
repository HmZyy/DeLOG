use delog_api::control::{ControlResponse, TraceInfo, TraceMode as ScriptTraceMode, TraceRequest};
use delog_api::{Error, Result};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use super::{AppControl, plot_pane};
use crate::plotting::legend::trace_label;
use crate::plotting::plot::{PlotPane, TraceMode as PlotTraceMode, TraceRef};

pub(super) fn apply_trace_request(
    control: &mut AppControl<'_>,
    request: TraceRequest,
) -> Result<ControlResponse> {
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
            validate_trace_field(control.snapshot, field_id, &field)?;
            let pane = plot_pane(control, window, tile)?;
            let color = color.unwrap_or_else(|| {
                delog_render::palette::trace_color(pane.traces.len()).to_srgb_f32()
            });
            pane.traces.push(TraceRef {
                instance_id: crate::plotting::plot::next_resource_instance_id(),
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
        TraceRequest::AddReturning {
            window,
            tile,
            field_id,
            field,
            color,
            width_px,
            mode,
            owner,
        } => {
            validate_trace_field(control.snapshot, field_id, &field)?;
            let snapshot = control.snapshot;
            let pane = plot_pane(control, window, tile)?;
            let color = color.unwrap_or_else(|| {
                delog_render::palette::trace_color(pane.traces.len()).to_srgb_f32()
            });
            pane.traces.push(TraceRef {
                instance_id: crate::plotting::plot::next_resource_instance_id(),
                field: field_id,
                color,
                width_px: width_px.unwrap_or(1.5),
                mode: plot_trace_mode(mode),
                visible: true,
                label_override: None,
                owner,
            });
            Ok(ControlResponse::Trace(
                trace_info_list(pane, snapshot)
                    .pop()
                    .expect("just inserted trace"),
            ))
        }
        TraceRequest::Remove {
            window,
            tile,
            index,
            field_id,
            field,
        } => {
            if let Some(field_id) = field_id {
                let path = field.as_deref().ok_or_else(|| {
                    Error::internal("field-based trace removal has no field path")
                })?;
                validate_trace_field(control.snapshot, field_id, path)?;
            }
            let pane = plot_pane(control, window, tile)?;
            let removed = match (index, field_id) {
                (Some(index), _) => {
                    if index >= pane.traces.len() {
                        return Err(Error::stale_handle(format!(
                            "trace {index} on plot {tile} in window {window} is gone"
                        )));
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
        TraceRequest::Clear { window, tile } => clear(control, window, tile, None),
        TraceRequest::ClearOwned {
            window,
            tile,
            owner,
        } => clear(control, window, tile, Some(owner.as_str())),
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
                Error::stale_handle(format!(
                    "trace {index} on plot {tile} in window {window} is gone"
                ))
            })?;
            if trace.field != field_id {
                return Err(Error::stale_handle(format!(
                    "trace {index} on plot {tile} in window {window} no longer refers to the field this handle was created for"
                )));
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

fn clear(
    control: &mut AppControl<'_>,
    window: u64,
    tile: u64,
    owner: Option<&str>,
) -> Result<ControlResponse> {
    let pane = plot_pane(control, window, tile)?;
    let remove = |trace: &TraceRef| {
        owner.is_none_or(|owner| {
            trace
                .owner
                .as_ref()
                .map(|candidate| candidate.name.as_str())
                == Some(owner)
        })
    };
    let removed: Vec<FieldId> = pane
        .traces
        .iter()
        .filter(|trace| remove(trace))
        .map(|trace| trace.field)
        .collect();
    pane.traces.retain(|trace| !remove(trace));
    let retained = super::batch::trace_counts(control.workspace, control.windows);
    for field in removed
        .into_iter()
        .collect::<std::collections::HashSet<_>>()
    {
        if retained.contains_key(&field) {
            continue;
        }
        control.caches.unpin(field);
    }
    Ok(ControlResponse::Unit)
}

fn trace_info_list(pane: &PlotPane, snapshot: &StoreSnapshot) -> Vec<TraceInfo> {
    pane.traces
        .iter()
        .enumerate()
        .map(|(index, trace)| TraceInfo {
            instance_id: trace.instance_id,
            owner: trace.owner.clone(),
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

pub(super) fn validate_trace_field(
    snapshot: &StoreSnapshot,
    field_id: FieldId,
    path: &str,
) -> Result<()> {
    let field = snapshot
        .fields
        .get(field_id.index())
        .filter(|field| field.id == field_id && !field.removed)
        .ok_or_else(|| Error::stale_handle(format!("field '{path}' is gone")))?;
    let topic = snapshot
        .topic(field.topic)
        .filter(|topic| !topic.entry.removed)
        .ok_or_else(|| Error::stale_handle(format!("field '{path}' topic is gone")))?;
    snapshot
        .source(topic.entry.source)
        .filter(|source| !source.entry.removed)
        .ok_or_else(|| Error::stale_handle(format!("field '{path}' source is gone")))?;
    let current_path = format!("{}.{}", topic.entry.name, field.name);
    if path != current_path {
        return Err(Error::stale_handle(format!(
            "field '{path}' no longer matches field {} ('{current_path}')",
            field_id.0
        )));
    }
    Ok(())
}
