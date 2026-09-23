use std::collections::HashMap;

use delog_cache::CacheManager;
use delog_core::identity::FieldId;
use delog_script::{ControlRequest, ControlResponse, GenerationRequest};

use super::{AppControl, apply_one};
use crate::shell::app::control_ownership;
use crate::shell::workspace::Workspace;

pub(super) fn apply_batch(
    control: &mut AppControl<'_>,
    requests: Vec<ControlRequest>,
) -> Result<ControlResponse, String> {
    let terminal_commit = requests.last().and_then(|request| match request {
        ControlRequest::Generation(GenerationRequest::Commit { owner, generation }) => {
            Some((owner.clone(), *generation))
        }
        _ => None,
    });
    for (index, request) in requests.iter().enumerate() {
        if !delog_script::control::request_is_batchable(request) {
            rollback_terminal_commit(control, terminal_commit.as_ref());
            return Err(format!(
                "batch request {index} is not an atomic mutation supported by delog.batch()"
            ));
        }
    }

    let traces_before = trace_counts(control.workspace, control.windows);
    let mut markers = control.markers.clone();
    let mut workspace = control.workspace.clone();
    let mut windows = control.windows.clone();
    let mut playback = *control.playback;
    let mut next_window_id = *control.next_window_id;
    let mut vehicles = control.vehicles.clone();
    let mut next_vehicle_id = *control.next_vehicle_id;
    let mut vehicle_revision = *control.vehicle_revision;
    let mut traj_dirty = *control.traj_dirty;
    let mut shadow_caches = CacheManager::new();
    let mut shadow = AppControl {
        markers: &mut markers,
        workspace: &mut workspace,
        windows: &mut windows,
        playback: &mut playback,
        next_window_id: &mut next_window_id,
        caches: &mut shadow_caches,
        snapshot: control.snapshot,
        vehicles: &mut vehicles,
        next_vehicle_id: &mut next_vehicle_id,
        vehicle_revision: &mut vehicle_revision,
        traj_dirty: &mut traj_dirty,
        vehicle_profiles: control.vehicle_profiles,
    };

    for (index, request) in requests.into_iter().enumerate() {
        if let Err(error) = apply_one(&mut shadow, request) {
            drop(shadow);
            rollback_terminal_commit(control, terminal_commit.as_ref());
            return Err(format!("batch request {index} failed: {error}"));
        }
    }
    drop(shadow);

    *control.markers = markers;
    *control.workspace = workspace;
    *control.windows = windows;
    *control.playback = playback;
    *control.next_window_id = next_window_id;
    *control.vehicles = vehicles;
    *control.next_vehicle_id = next_vehicle_id;
    *control.vehicle_revision = vehicle_revision;
    *control.traj_dirty = traj_dirty;
    let traces_after = trace_counts(control.workspace, control.windows);
    unpin_removed_traces(control.caches, &traces_before, &traces_after);
    Ok(ControlResponse::Unit)
}

fn rollback_terminal_commit(control: &mut AppControl<'_>, commit: Option<&(String, u64)>) {
    if let Some((owner, generation)) = commit {
        control_ownership::apply_sweep(
            control,
            &control_ownership::Sweep::Rollback {
                owner: owner.clone(),
                generation: *generation,
            },
        );
    }
}

pub(super) fn trace_counts(
    workspace: &Workspace,
    windows: &[crate::shell::windows::ExtendedWindow],
) -> HashMap<FieldId, usize> {
    let mut counts = HashMap::new();
    let mut count_workspace = |workspace: &Workspace| {
        for pane in workspace.plot_panes() {
            for trace in &pane.traces {
                *counts.entry(trace.field).or_insert(0) += 1;
            }
        }
    };
    count_workspace(workspace);
    for window in windows {
        count_workspace(&window.workspace);
    }
    counts
}

pub(super) fn unpin_removed_traces(
    caches: &mut CacheManager,
    before: &HashMap<FieldId, usize>,
    after: &HashMap<FieldId, usize>,
) {
    for (field, before_count) in before {
        let removed = before_count.saturating_sub(after.get(field).copied().unwrap_or(0));
        for _ in 0..removed {
            caches.unpin(*field);
        }
    }
}
