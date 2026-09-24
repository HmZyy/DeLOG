use std::path::Path;

use delog_api::control::{
    ControlRequest, ControlResponse, LayoutFieldIssue, LayoutRequest, LoadReport,
};

use super::{AppControl, trace_counts, unpin_removed_traces};
use crate::config::layout::doc::{
    LayoutDoc, decode_doc, delete_named, doc_json, duplicate_named, export_doc, import_doc,
    load_named_doc, rename_named, save_named, try_list_layouts,
};
use crate::shell::layout_apply::{CurrentLayout, LayoutApply, LoadOutcome, current_doc, load_doc};

#[derive(Debug, Default)]
pub struct LayoutControlEffects {
    pub interrupt_layout: bool,
    pub reset_view: bool,
    pub clear_transients: bool,
    pub invalidate_catalog: bool,
}

impl LayoutControlEffects {
    pub fn for_success(request: &ControlRequest) -> Self {
        let mut effects = Self::default();
        if let ControlRequest::Layouts(request) = request {
            match request {
                LayoutRequest::Load { .. }
                | LayoutRequest::ImportFile { .. }
                | LayoutRequest::Apply { .. } => {
                    effects.interrupt_layout = true;
                    effects.reset_view = true;
                }
                LayoutRequest::Clear => {
                    effects.interrupt_layout = true;
                    effects.reset_view = true;
                    effects.clear_transients = true;
                }
                LayoutRequest::Save { .. }
                | LayoutRequest::Delete { .. }
                | LayoutRequest::Rename { .. }
                | LayoutRequest::Duplicate { .. } => effects.invalidate_catalog = true,
                LayoutRequest::List | LayoutRequest::ExportFile { .. } | LayoutRequest::Current => {
                }
            }
        }
        effects
    }

    pub fn merge(&mut self, other: Self) {
        self.interrupt_layout |= other.interrupt_layout;
        self.reset_view |= other.reset_view;
        self.clear_transients |= other.clear_transients;
        self.invalidate_catalog |= other.invalidate_catalog;
    }
}

pub(super) fn apply_layout_request(
    control: &mut AppControl<'_>,
    request: LayoutRequest,
) -> Result<ControlResponse, String> {
    match request {
        LayoutRequest::List => Ok(ControlResponse::Names(
            try_list_layouts().map_err(|error| error.to_string())?,
        )),
        LayoutRequest::Save { name } => {
            let name = validate_name(&name)?;
            let doc = current(control, name.clone());
            save_named(&name, &doc).map_err(|error| error.to_string())?;
            Ok(ControlResponse::Unit)
        }
        LayoutRequest::Load { name } => {
            let name = validate_name(&name)?;
            let doc = load_named_doc(&name).map_err(|error| error.to_string())?;
            apply_doc(control, doc)
        }
        LayoutRequest::Delete { name } => {
            delete_named(&validate_name(&name)?).map_err(|error| error.to_string())?;
            Ok(ControlResponse::Unit)
        }
        LayoutRequest::Rename { from, to } => {
            rename_named(&validate_name(&from)?, &validate_name(&to)?)
                .map_err(|error| error.to_string())?;
            Ok(ControlResponse::Unit)
        }
        LayoutRequest::Duplicate { from, to } => {
            duplicate_named(&validate_name(&from)?, &validate_name(&to)?)
                .map_err(|error| error.to_string())?;
            Ok(ControlResponse::Unit)
        }
        LayoutRequest::ImportFile { path } => {
            let doc =
                import_doc(Path::new(validate_path(&path)?)).map_err(|error| error.to_string())?;
            apply_doc(control, doc)
        }
        LayoutRequest::ExportFile { name, path } => {
            let doc = load_named_doc(&validate_name(&name)?).map_err(|error| error.to_string())?;
            export_doc(Path::new(validate_path(&path)?), &doc)
                .map_err(|error| error.to_string())?;
            Ok(ControlResponse::Unit)
        }
        LayoutRequest::Clear => {
            clear(control);
            Ok(ControlResponse::Unit)
        }
        LayoutRequest::Current => {
            let doc = current(control, "current".into());
            Ok(ControlResponse::Layout(
                doc_json(&doc).map_err(|error| error.to_string())?,
            ))
        }
        LayoutRequest::Apply { json } => {
            let doc = decode_doc(&json).map_err(|error| error.to_string())?;
            apply_doc(control, doc)
        }
    }
}

fn current(control: &AppControl<'_>, name: String) -> LayoutDoc {
    current_doc(CurrentLayout {
        name,
        workspace: control.workspace,
        windows: control.windows,
        snapshot: control.snapshot,
        speed: f64::from(control.playback.speed),
        follow_live: control.playback.follow_live,
        vehicles: control.vehicles,
    })
}

fn apply_doc(control: &mut AppControl<'_>, doc: LayoutDoc) -> Result<ControlResponse, String> {
    let layout = match load_doc(doc, control.snapshot).map_err(|error| error.to_string())? {
        LoadOutcome::Applied(layout) => layout,
        LoadOutcome::NeedsMapping(pending) => pending.apply_skipping(control.snapshot),
    };
    let report = bridge_report(&layout);
    apply_layout(control, layout)?;
    Ok(ControlResponse::LoadReport(report))
}

fn bridge_report(layout: &LayoutApply) -> LoadReport {
    let mut ambiguous = layout
        .report
        .ambiguous
        .iter()
        .map(|issue| {
            let mut candidates = issue
                .candidates
                .iter()
                .map(|candidate| candidate.label.clone())
                .collect::<Vec<_>>();
            candidates.sort();
            candidates.dedup();
            LayoutFieldIssue {
                field: format!("{}.{}", issue.field.topic, issue.field.field),
                candidates,
            }
        })
        .collect::<Vec<_>>();
    ambiguous.sort_by(|a, b| a.field.cmp(&b.field));
    let mut unresolved = layout
        .report
        .unresolved
        .iter()
        .map(|field| format!("{}.{}", field.topic, field.field))
        .collect::<Vec<_>>();
    unresolved.sort();
    unresolved.dedup();
    LoadReport {
        ambiguous,
        unresolved,
        warnings: layout.report.warnings.clone(),
    }
}

fn apply_layout(control: &mut AppControl<'_>, mut layout: LayoutApply) -> Result<(), String> {
    let traces_before = trace_counts(control.workspace, control.windows);
    let mut next_vehicle_id = *control.next_vehicle_id;
    crate::scene3d::vehicle::assign_runtime_ids(&mut layout.vehicles, &mut next_vehicle_id)?;

    *control.workspace = layout.workspace;
    *control.windows = layout.windows;
    *control.next_window_id = crate::shell::windows::next_window_id(control.windows);
    control.playback.set_speed(layout.speed as f32);
    control.playback.follow_live = layout.follow_live;
    *control.vehicles = layout.vehicles;
    *control.next_vehicle_id = next_vehicle_id;
    *control.vehicle_revision = control.vehicle_revision.wrapping_add(1);
    *control.traj_dirty = true;

    let traces_after = trace_counts(control.workspace, control.windows);
    unpin_removed_traces(control.caches, &traces_before, &traces_after);
    Ok(())
}

fn clear(control: &mut AppControl<'_>) {
    let traces_before = trace_counts(control.workspace, control.windows);
    *control.workspace = crate::shell::workspace::Workspace::new();
    control.windows.clear();
    *control.next_window_id = 1;
    control.playback.set_speed(1.0);
    control.playback.follow_live = false;
    control.markers.clear_preserving_ids();
    control.vehicles.clear();
    *control.vehicle_revision = control.vehicle_revision.wrapping_add(1);
    *control.traj_dirty = true;
    let traces_after = trace_counts(control.workspace, control.windows);
    unpin_removed_traces(control.caches, &traces_before, &traces_after);
}

fn validate_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("layout names may contain only ASCII letters, digits, '-' and '_'".into());
    }
    Ok(name.to_owned())
}

fn validate_path(path: &str) -> Result<&str, String> {
    if path.trim().is_empty() {
        Err("layout path must not be empty".into())
    } else {
        Ok(path)
    }
}
