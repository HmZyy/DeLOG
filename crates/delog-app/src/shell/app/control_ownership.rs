use std::collections::HashSet;

use delog_api::control::{GenerationRequest, ScriptOwner};
use delog_core::identity::FieldId;

use super::control_service::AppControl;
use crate::plotting::annotations::AnnotationOwner;
use crate::plotting::plot::PlotPane;
use crate::scene3d::vehicle::{VehicleConfig, VehicleOwner};

#[derive(Debug)]
pub enum Sweep {
    Commit { owner: String, generation: u64 },
    Rollback { owner: String, generation: u64 },
    RemoveOwned { owner: String },
}

impl From<GenerationRequest> for Sweep {
    fn from(request: GenerationRequest) -> Self {
        match request {
            GenerationRequest::Commit { owner, generation } => Self::Commit { owner, generation },
            GenerationRequest::Rollback { owner, generation } => {
                Self::Rollback { owner, generation }
            }
            GenerationRequest::RemoveOwned { owner } => Self::RemoveOwned { owner },
        }
    }
}

impl Sweep {
    pub fn removes(&self, owner: Option<&ScriptOwner>) -> bool {
        let Some(owner) = owner else {
            return false;
        };
        self.removes_owner(&owner.name, owner.generation)
    }

    fn removes_annotation(&self, owner: Option<&AnnotationOwner>) -> bool {
        let Some(owner) = owner else {
            return false;
        };
        self.removes_owner(&owner.name, owner.generation)
    }

    fn removes_vehicle(&self, owner: Option<&VehicleOwner>) -> bool {
        let Some(owner) = owner else {
            return false;
        };
        self.removes_owner(&owner.name, owner.generation)
    }

    fn removes_owner(&self, owner: &str, owner_generation: u64) -> bool {
        match self {
            Self::Commit {
                owner: name,
                generation,
            } => owner == name && owner_generation < *generation,
            Self::Rollback {
                owner: name,
                generation,
            } => owner == name && owner_generation == *generation,
            Self::RemoveOwned { owner: name } => owner == name,
        }
    }
}

pub fn sweep_vec<T>(
    items: &mut Vec<T>,
    owner_of: impl Fn(&T) -> Option<&ScriptOwner>,
    sweep: &Sweep,
) {
    items.retain(|item| !sweep.removes(owner_of(item)));
}

fn sweep_pane(pane: &mut PlotPane, sweep: &Sweep) -> (Vec<FieldId>, usize) {
    let removed: Vec<FieldId> = pane
        .traces
        .iter()
        .filter(|trace| sweep.removes(trace.owner.as_ref()))
        .map(|trace| trace.field)
        .collect();
    sweep_vec(&mut pane.traces, |trace| trace.owner.as_ref(), sweep);
    sweep_vec(&mut pane.ghosts, |trace| trace.owner.as_ref(), sweep);
    let annotations = pane.annotations.items().len();
    pane.annotations
        .retain(|annotation| !sweep.removes_annotation(annotation.owner.as_ref()));
    let count = removed.len() + annotations - pane.annotations.items().len();
    (removed, count)
}

fn holds_manual_content(pane: &PlotPane) -> bool {
    pane.traces.iter().any(|trace| trace.owner.is_none())
        || pane.ghosts.iter().any(|ghost| ghost.owner.is_none())
        || pane
            .annotations
            .items()
            .iter()
            .any(|annotation| annotation.owner.is_none())
}

fn sweep_vehicles(vehicles: &mut Vec<VehicleConfig>, sweep: &Sweep) -> usize {
    let before = vehicles.len();
    vehicles.retain(|vehicle| !sweep.removes_vehicle(vehicle.runtime.owner.as_ref()));
    before - vehicles.len()
}

pub fn apply_sweep(control: &mut AppControl<'_>, sweep: &Sweep) -> usize {
    let markers = control.markers.count();
    match sweep {
        Sweep::Commit { owner, generation } => {
            control.markers.sweep_generation(owner, *generation, true);
        }
        Sweep::Rollback { owner, generation } => {
            control.markers.sweep_generation(owner, *generation, false);
        }
        Sweep::RemoveOwned { owner } => {
            control
                .markers
                .apply_control_request(delog_api::control::MarkerRequest::RemoveOwned {
                    owner: owner.clone(),
                })
                .expect("removing markers by owner cannot fail");
        }
    }
    let mut removed = markers - control.markers.count();
    let mut removed_fields = Vec::new();
    control.windows.retain(|window| {
        if !sweep.removes(window.owner.as_ref())
            || window.workspace.plot_panes().any(holds_manual_content)
        {
            return true;
        }
        removed += 1 + window.workspace.plot_panes().count();
        removed_fields.extend(
            window
                .workspace
                .plot_panes()
                .flat_map(|pane| pane.traces.iter().map(|trace| trace.field)),
        );
        false
    });
    let (fields, count) = sweep_workspace(control.workspace, sweep);
    removed_fields.extend(fields);
    removed += count;
    for window in control.windows.iter_mut() {
        let (fields, count) = sweep_workspace(&mut window.workspace, sweep);
        removed_fields.extend(fields);
        removed += count;
    }
    let retained_fields: HashSet<_> = control
        .workspace
        .plot_panes()
        .chain(
            control
                .windows
                .iter()
                .flat_map(|window| window.workspace.plot_panes()),
        )
        .flat_map(|pane| pane.traces.iter().map(|trace| trace.field))
        .collect();
    for field in removed_fields.into_iter().collect::<HashSet<_>>() {
        if !retained_fields.contains(&field) {
            control.caches.unpin(field);
        }
    }
    let vehicles = sweep_vehicles(control.vehicles, sweep);
    if vehicles > 0 {
        super::control_service::mark_vehicles_changed(control);
    }
    removed + vehicles
}

fn sweep_workspace(
    workspace: &mut crate::shell::workspace::Workspace,
    sweep: &Sweep,
) -> (Vec<FieldId>, usize) {
    let owned_tiles: Vec<_> = workspace
        .tree
        .tiles
        .iter()
        .filter_map(|(id, tile)| match tile {
            egui_tiles::Tile::Pane(crate::shell::workspace::Pane::Plot(pane))
                if sweep.removes(pane.owner.as_ref()) && !holds_manual_content(pane) =>
            {
                Some(*id)
            }
            _ => None,
        })
        .collect();
    let mut removed = Vec::new();
    let mut count = owned_tiles.len();
    for tile in owned_tiles {
        removed.extend(workspace.close_plot(tile));
    }
    for pane in workspace.plot_panes_mut() {
        let (fields, items) = sweep_pane(pane, sweep);
        removed.extend(fields);
        count += items;
    }
    (removed, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_owned_keeps_a_shared_field_pinned_for_a_retained_window() {
        use crate::shell::windows::{ExtendedWindow, WindowId};
        use crate::shell::workspace::{SplitDirection, Workspace};
        use std::sync::Arc;

        let mut markers = crate::plotting::markers::Markers::new();
        let mut workspace = Workspace::new();
        let manual = workspace.tree.root().unwrap();
        let owned = workspace
            .split_plot(manual, SplitDirection::Horizontal)
            .unwrap();
        let owned_pane = workspace.plot_pane_mut(owned).unwrap();
        owned_pane.owner = owner("flight", 7);
        owned_pane.add_trace(FieldId(42));
        owned_pane.traces[0].owner = owner("flight", 7);

        let mut windows = vec![ExtendedWindow::new(WindowId(1))];
        windows[0].owner = owner("other", 3);
        windows[0]
            .workspace
            .plot_panes_mut()
            .next()
            .unwrap()
            .add_trace(FieldId(42));

        let snapshot = delog_core::snapshot::StoreSnapshot::empty();
        let mut playback = crate::plotting::timeline::Playback::default();
        let mut next_window_id = 2;
        let mut next_vehicle_id = 1;
        let mut vehicle_revision = 0;
        let mut traj_dirty = false;
        let mut vehicles = Vec::new();
        let mut caches = delog_cache::CacheManager::new();
        caches.request(FieldId(42), &Arc::new(snapshot.clone()));
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles: &mut vehicles,
            next_vehicle_id: &mut next_vehicle_id,
            vehicle_revision: &mut vehicle_revision,
            traj_dirty: &mut traj_dirty,
            vehicle_profiles: None,
        };

        crate::shell::app::control_service::apply(
            &mut control,
            delog_api::control::ControlRequest::Generation(GenerationRequest::RemoveOwned {
                owner: "flight".into(),
            }),
        )
        .unwrap();

        assert!(control.workspace.plot_pane_mut(owned).is_none());
        assert_eq!(
            control.windows[0]
                .workspace
                .plot_panes()
                .next()
                .unwrap()
                .traces[0]
                .field,
            FieldId(42)
        );
        assert!(control.caches.is_pinned(FieldId(42)));
    }

    #[test]
    fn remove_owned_preserves_a_scene_when_the_last_plot_is_removed() {
        use crate::shell::workspace::Workspace;
        use std::sync::Arc;

        let mut markers = crate::plotting::markers::Markers::new();
        let mut workspace = Workspace::new();
        let owned = workspace.tree.root().unwrap();
        let pane = workspace.plot_pane_mut(owned).unwrap();
        pane.owner = owner("flight", 7);
        pane.add_trace(FieldId(9));
        pane.traces[0].owner = owner("flight", 7);
        workspace.toggle_scene_pane();
        let scene = workspace.scene_pane_id().unwrap();

        let mut windows = Vec::new();
        let snapshot = delog_core::snapshot::StoreSnapshot::empty();
        let mut playback = crate::plotting::timeline::Playback::default();
        let mut next_window_id = 1;
        let mut next_vehicle_id = 1;
        let mut vehicle_revision = 0;
        let mut traj_dirty = false;
        let mut vehicles = Vec::new();
        let mut caches = delog_cache::CacheManager::new();
        caches.request(FieldId(9), &Arc::new(snapshot.clone()));
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles: &mut vehicles,
            next_vehicle_id: &mut next_vehicle_id,
            vehicle_revision: &mut vehicle_revision,
            traj_dirty: &mut traj_dirty,
            vehicle_profiles: None,
        };

        crate::shell::app::control_service::apply(
            &mut control,
            delog_api::control::ControlRequest::Generation(GenerationRequest::RemoveOwned {
                owner: "flight".into(),
            }),
        )
        .unwrap();

        assert_eq!(control.workspace.scene_pane_id(), Some(scene));
        assert!(control.workspace.plot_pane_mut(owned).is_none());
        assert_eq!(control.workspace.plot_panes().count(), 0);
        assert!(!control.caches.is_pinned(FieldId(9)));
    }

    #[test]
    fn remove_owned_removes_complete_resources_and_preserves_manual_and_other_owners() {
        use crate::shell::windows::{ExtendedWindow, WindowId};
        use crate::shell::workspace::{SplitDirection, Workspace};
        use delog_api::control::MarkerRequest;
        use delog_api::markers::PendingMarker;
        use std::sync::Arc;

        let mut markers = crate::plotting::markers::Markers::new();
        markers.add_at(1);
        for (name, label) in [("flight", "owned"), ("other", "other")] {
            markers
                .apply_control_request(MarkerRequest::Append {
                    owner: name.into(),
                    generation: 7,
                    markers: vec![PendingMarker {
                        time_us: 2,
                        label: label.into(),
                        color: None,
                        note: String::new(),
                    }],
                })
                .unwrap();
        }
        let mut workspace = Workspace::new();
        let manual = workspace.tree.root().unwrap();
        workspace
            .plot_pane_mut(manual)
            .unwrap()
            .add_trace(FieldId(1));
        let owned = workspace
            .split_plot(manual, SplitDirection::Horizontal)
            .unwrap();
        let pane = workspace.plot_pane_mut(owned).unwrap();
        pane.owner = owner("flight", 7);
        pane.add_trace(FieldId(2));
        pane.traces[0].owner = owner("flight", 7);
        let other = workspace
            .split_plot(manual, SplitDirection::Vertical)
            .unwrap();
        let pane = workspace.plot_pane_mut(other).unwrap();
        pane.owner = owner("other", 7);
        pane.add_trace(FieldId(3));
        pane.add_trace(FieldId(4));
        pane.traces[1].owner = owner("flight", 8);
        pane.ghosts.push(crate::plotting::plot::GhostTrace {
            owner: owner("flight", 9),
            source: None,
            topic: "missing".into(),
            field: "value".into(),
            color: [1.0; 4],
            width_px: 1.0,
            mode: crate::plotting::plot::TraceMode::Line,
            visible: true,
            text_filter: None,
            text_offsets: Vec::new(),
        });
        let mut windows = vec![
            ExtendedWindow::new(WindowId(1)),
            ExtendedWindow::new(WindowId(2)),
            ExtendedWindow::new(WindowId(3)),
        ];
        windows[0].owner = owner("flight", 2);
        windows[1].owner = owner("other", 2);
        for (index, window) in windows.iter_mut().enumerate() {
            window
                .workspace
                .plot_panes_mut()
                .next()
                .unwrap()
                .add_trace(FieldId(index as u32 + 5));
        }
        let pane = windows[0].workspace.plot_panes_mut().next().unwrap();
        pane.owner = owner("flight", 7);
        pane.traces[0].owner = owner("flight", 7);
        let mut vehicles = vec![
            vehicle_with_owner("owned", Some(("flight", 0))),
            vehicle_with_owner("manual", None),
            vehicle_with_owner("other", Some(("other", 1))),
        ];
        let snapshot = delog_core::snapshot::StoreSnapshot::empty();
        let mut playback = crate::plotting::timeline::Playback::default();
        let mut next_window_id = 4;
        let mut next_vehicle_id = 1;
        let mut vehicle_revision = 0;
        let mut traj_dirty = false;
        let mut caches = delog_cache::CacheManager::new();
        for id in 1..=7 {
            caches.request(FieldId(id), &Arc::new(snapshot.clone()));
        }
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles: &mut vehicles,
            next_vehicle_id: &mut next_vehicle_id,
            vehicle_revision: &mut vehicle_revision,
            traj_dirty: &mut traj_dirty,
            vehicle_profiles: None,
        };
        let request =
            delog_api::control::ControlRequest::Generation(GenerationRequest::RemoveOwned {
                owner: "flight".into(),
            });
        crate::shell::app::control_service::apply(&mut control, request.clone()).unwrap();
        crate::shell::app::control_service::apply(&mut control, request).unwrap();
        assert!(control.workspace.plot_pane_mut(owned).is_none());
        assert_eq!(control.workspace.plot_panes().count(), 2);
        assert_eq!(
            control.workspace.plot_pane_mut(manual).unwrap().traces[0].field,
            FieldId(1)
        );
        assert_eq!(
            control.workspace.plot_pane_mut(other).unwrap().traces.len(),
            1
        );
        assert!(
            control
                .workspace
                .plot_pane_mut(other)
                .unwrap()
                .ghosts
                .is_empty()
        );
        assert_eq!(
            control.windows.iter().map(|w| w.id.0).collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(
            control
                .vehicles
                .iter()
                .map(|v| v.label.as_str())
                .collect::<Vec<_>>(),
            ["manual", "other"]
        );
        assert_eq!(
            control
                .markers
                .as_slice()
                .iter()
                .map(|marker| marker.label.as_str())
                .collect::<Vec<_>>(),
            ["Marker 1", "other"]
        );
        for id in [2, 4, 5] {
            assert!(!control.caches.is_pinned(FieldId(id)));
        }
        for id in [1, 3, 6, 7] {
            assert!(control.caches.is_pinned(FieldId(id)));
        }
        assert_eq!(*control.vehicle_revision, 1);
    }

    fn vehicle_with_owner(
        label: &str,
        owner: Option<(&str, u64)>,
    ) -> crate::scene3d::vehicle::VehicleConfig {
        use crate::scene3d::vehicle::{
            ModelKind, OriMapping, PosMapping, VehicleOwner, VehicleRuntime,
        };

        crate::scene3d::vehicle::VehicleConfig {
            runtime: VehicleRuntime {
                id: 0,
                owner: owner.map(|(name, generation)| VehicleOwner {
                    name: name.into(),
                    generation,
                }),
            },
            source: delog_core::identity::SourceId(0),
            label: label.into(),
            show: true,
            show_path: true,
            pos: PosMapping::Gps {
                lat: FieldId(0),
                lon: FieldId(0),
                alt: FieldId(0),
                lat_lon_dege7: false,
                alt_mm: false,
                alt_offset_m: 0.0,
            },
            ori: OriMapping::Static,
            model: ModelKind::None,
            color: egui::Color32::WHITE,
            path_color: egui::Color32::WHITE,
            scale: 1.0,
        }
    }

    fn owner(name: &str, generation: u64) -> Option<ScriptOwner> {
        Some(ScriptOwner {
            name: name.into(),
            generation,
        })
    }

    #[test]
    fn commit_removes_only_older_generations_of_the_same_owner() {
        let mut items = vec![
            (owner("flight.py", 1), "old"),
            (owner("flight.py", 2), "new"),
            (owner("other.py", 1), "other"),
            (None, "hand drawn"),
        ];
        sweep_vec(
            &mut items,
            |item| item.0.as_ref(),
            &Sweep::Commit {
                owner: "flight.py".into(),
                generation: 2,
            },
        );
        let kept: Vec<&str> = items.iter().map(|item| item.1).collect();
        assert_eq!(kept, ["new", "other", "hand drawn"]);
    }

    #[test]
    fn rollback_removes_exactly_the_failed_generation() {
        let mut items = vec![
            (owner("flight.py", 1), "previous"),
            (owner("flight.py", 2), "failed run"),
            (None, "hand drawn"),
        ];
        sweep_vec(
            &mut items,
            |item| item.0.as_ref(),
            &Sweep::Rollback {
                owner: "flight.py".into(),
                generation: 2,
            },
        );
        let kept: Vec<&str> = items.iter().map(|item| item.1).collect();
        assert_eq!(kept, ["previous", "hand drawn"]);
    }

    #[test]
    fn commit_and_rollback_sweep_owned_vehicles_without_touching_manual_ones() {
        let mut vehicles = vec![
            vehicle_with_owner("old", Some(("flight.py", 1))),
            vehicle_with_owner("new", Some(("flight.py", 2))),
            vehicle_with_owner("other", Some(("other.py", 1))),
            vehicle_with_owner("manual", None),
        ];
        sweep_vehicles(
            &mut vehicles,
            &Sweep::Commit {
                owner: "flight.py".into(),
                generation: 2,
            },
        );
        assert_eq!(
            vehicles
                .iter()
                .map(|vehicle| vehicle.label.as_str())
                .collect::<Vec<_>>(),
            ["new", "other", "manual"]
        );

        sweep_vehicles(
            &mut vehicles,
            &Sweep::Rollback {
                owner: "flight.py".into(),
                generation: 2,
            },
        );
        assert_eq!(
            vehicles
                .iter()
                .map(|vehicle| vehicle.label.as_str())
                .collect::<Vec<_>>(),
            ["other", "manual"]
        );
    }

    #[test]
    fn a_restored_vehicle_owner_is_removed_by_its_first_commit_not_rollback() {
        let restored = vehicle_with_owner("restored", Some(("flight.py", 0)));
        let mut rollback_case = vec![restored.clone()];
        sweep_vehicles(
            &mut rollback_case,
            &Sweep::Rollback {
                owner: "flight.py".into(),
                generation: 1,
            },
        );
        assert_eq!(rollback_case.len(), 1);

        let mut commit_case = vec![restored];
        sweep_vehicles(
            &mut commit_case,
            &Sweep::Commit {
                owner: "flight.py".into(),
                generation: 1,
            },
        );
        assert!(commit_case.is_empty());
    }

    #[test]
    fn removing_a_vehicle_through_apply_sweep_invalidates_trajectories() {
        let mut markers = crate::plotting::markers::Markers::new();
        let mut workspace = crate::shell::workspace::Workspace::new();
        let mut windows = Vec::new();
        let mut playback = crate::plotting::timeline::Playback::default();
        let mut next_window_id = 1;
        let mut caches = delog_cache::CacheManager::new();
        let snapshot = delog_core::snapshot::StoreSnapshot::empty();
        let mut vehicles = vec![
            vehicle_with_owner("old", Some(("flight.py", 1))),
            vehicle_with_owner("manual", None),
        ];
        let mut next_vehicle_id = 1;
        let mut vehicle_revision = 7;
        let mut traj_dirty = false;
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut workspace,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles: &mut vehicles,
            next_vehicle_id: &mut next_vehicle_id,
            vehicle_revision: &mut vehicle_revision,
            traj_dirty: &mut traj_dirty,
            vehicle_profiles: None,
        };
        apply_sweep(
            &mut control,
            &Sweep::Commit {
                owner: "flight.py".into(),
                generation: 2,
            },
        );
        assert_eq!(vehicles.len(), 1);
        assert_eq!(vehicles[0].label, "manual");
        assert_eq!(vehicle_revision, 8);
        assert!(traj_dirty);
    }

    #[test]
    fn a_restored_owner_stamped_annotation_survives_its_owners_first_rollback() {
        use crate::plotting::annotations::{DataPos, Geometry};
        use crate::plotting::plot::PlotPane;

        let mut pane = PlotPane::default();
        let id = pane.annotations.add_geometry(Geometry::Text {
            at: DataPos { t_us: 0, y: 0.0 },
        });
        pane.annotations.get_mut(id).unwrap().owner = owner("flight.py", 0);

        sweep_pane(
            &mut pane,
            &Sweep::Rollback {
                owner: "flight.py".into(),
                generation: 1,
            },
        );

        assert_eq!(pane.annotations.items().len(), 1);
    }

    #[test]
    fn a_restored_owner_stamped_annotation_is_removed_by_its_owners_first_commit() {
        use crate::plotting::annotations::{DataPos, Geometry};
        use crate::plotting::plot::PlotPane;

        let mut pane = PlotPane::default();
        let id = pane.annotations.add_geometry(Geometry::Text {
            at: DataPos { t_us: 0, y: 0.0 },
        });
        pane.annotations.get_mut(id).unwrap().owner = owner("flight.py", 0);

        sweep_pane(
            &mut pane,
            &Sweep::Commit {
                owner: "flight.py".into(),
                generation: 1,
            },
        );

        assert!(pane.annotations.is_empty());
    }

    #[test]
    fn unowned_objects_are_never_swept() {
        let mut items = vec![(None, "hand drawn"), (None, "console")];
        sweep_vec(
            &mut items,
            |item| item.0.as_ref(),
            &Sweep::Commit {
                owner: "flight.py".into(),
                generation: 9,
            },
        );
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn a_commit_sweep_unpins_the_fields_of_the_traces_it_removes() {
        use crate::plotting::plot::{TraceMode, TraceRef};
        use crate::shell::workspace::Workspace;
        use delog_core::identity::FieldId;
        use std::sync::Arc;

        let owned = owner("flight.py", 1);
        let field = FieldId(1);

        let mut markers = crate::plotting::markers::Markers::new();
        let mut main = Workspace::new();
        let mut windows: Vec<crate::shell::windows::ExtendedWindow> = Vec::new();
        {
            let pane = main.plot_panes_mut().next().unwrap();
            pane.traces.push(TraceRef {
                instance_id: crate::plotting::plot::next_resource_instance_id(),
                field,
                color: [0.0, 0.0, 0.0, 1.0],
                width_px: 1.0,
                mode: TraceMode::Line,
                visible: true,
                label_override: None,
                owner: owned.clone(),
            });
        }

        let snapshot = delog_core::snapshot::StoreSnapshot::empty();
        let mut playback = crate::plotting::timeline::Playback::default();
        let mut next_window_id = 1u64;
        let mut caches = delog_cache::CacheManager::new();
        let mut vehicles = Vec::new();
        let mut next_vehicle_id = 1u64;
        let mut vehicle_revision = 0u64;
        let mut traj_dirty = false;
        caches.request(
            field,
            &Arc::new(delog_core::snapshot::StoreSnapshot::empty()),
        );
        assert!(caches.is_pinned(field));
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut main,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles: &mut vehicles,
            next_vehicle_id: &mut next_vehicle_id,
            vehicle_revision: &mut vehicle_revision,
            traj_dirty: &mut traj_dirty,
            vehicle_profiles: None,
        };
        apply_sweep(
            &mut control,
            &Sweep::Commit {
                owner: "flight.py".into(),
                generation: 2,
            },
        );

        assert!(!caches.is_pinned(field));
    }

    #[test]
    fn apply_sweep_reaches_panes_in_extended_windows_too() {
        use crate::plotting::annotations::{DataPos, Geometry};
        use crate::plotting::plot::{TraceMode, TraceRef};
        use crate::shell::windows::{ExtendedWindow, WindowId};
        use crate::shell::workspace::Workspace;
        use delog_core::identity::FieldId;

        let owned = owner("flight.py", 1);

        let mut markers = crate::plotting::markers::Markers::new();
        let mut main = Workspace::new();
        let mut windows = vec![ExtendedWindow::new(WindowId(1))];
        {
            let pane = windows[0].workspace.plot_panes_mut().next().unwrap();
            pane.traces.push(TraceRef {
                instance_id: crate::plotting::plot::next_resource_instance_id(),
                field: FieldId(1),
                color: [0.0, 0.0, 0.0, 1.0],
                width_px: 1.0,
                mode: TraceMode::Line,
                visible: true,
                label_override: None,
                owner: owned.clone(),
            });
            let id = pane.annotations.add_geometry(Geometry::Text {
                at: DataPos { t_us: 0, y: 0.0 },
            });
            pane.annotations.get_mut(id).unwrap().owner = owned.clone();
        }

        let snapshot = delog_core::snapshot::StoreSnapshot::empty();
        let mut playback = crate::plotting::timeline::Playback::default();
        let mut next_window_id = 2u64;
        let mut caches = delog_cache::CacheManager::new();
        let mut vehicles = Vec::new();
        let mut next_vehicle_id = 1u64;
        let mut vehicle_revision = 0u64;
        let mut traj_dirty = false;
        let mut control = AppControl {
            markers: &mut markers,
            workspace: &mut main,
            windows: &mut windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles: &mut vehicles,
            next_vehicle_id: &mut next_vehicle_id,
            vehicle_revision: &mut vehicle_revision,
            traj_dirty: &mut traj_dirty,
            vehicle_profiles: None,
        };
        apply_sweep(
            &mut control,
            &Sweep::Commit {
                owner: "flight.py".into(),
                generation: 2,
            },
        );

        let pane = control.windows[0]
            .workspace
            .plot_panes_mut()
            .next()
            .unwrap();
        assert!(pane.traces.is_empty());
        assert!(pane.annotations.is_empty());
    }

    fn run_sweep(
        workspace: &mut crate::shell::workspace::Workspace,
        windows: &mut Vec<crate::shell::windows::ExtendedWindow>,
        sweep: &Sweep,
    ) {
        let mut markers = crate::plotting::markers::Markers::new();
        let snapshot = delog_core::snapshot::StoreSnapshot::empty();
        let mut playback = crate::plotting::timeline::Playback::default();
        let mut next_window_id = 100u64;
        let mut caches = delog_cache::CacheManager::new();
        let mut vehicles = Vec::new();
        let mut next_vehicle_id = 1u64;
        let mut vehicle_revision = 0u64;
        let mut traj_dirty = false;
        let mut control = AppControl {
            markers: &mut markers,
            workspace,
            windows,
            playback: &mut playback,
            next_window_id: &mut next_window_id,
            caches: &mut caches,
            snapshot: &snapshot,
            vehicles: &mut vehicles,
            next_vehicle_id: &mut next_vehicle_id,
            vehicle_revision: &mut vehicle_revision,
            traj_dirty: &mut traj_dirty,
            vehicle_profiles: None,
        };
        apply_sweep(&mut control, sweep);
    }

    fn owned_pane(
        workspace: &mut crate::shell::workspace::Workspace,
        name: &str,
        generation: u64,
        field: u32,
    ) -> egui_tiles::TileId {
        let root = workspace.tree.root().unwrap();
        let tile = workspace
            .split_plot(root, crate::shell::workspace::SplitDirection::Horizontal)
            .unwrap();
        let pane = workspace.plot_pane_mut(tile).unwrap();
        pane.owner = owner(name, generation);
        pane.add_trace(FieldId(field));
        pane.traces[0].owner = owner(name, generation);
        tile
    }

    fn owned_window(id: u64, name: &str, generation: u64) -> crate::shell::windows::ExtendedWindow {
        let mut window =
            crate::shell::windows::ExtendedWindow::new(crate::shell::windows::WindowId(id));
        window.owner = owner(name, generation);
        let pane = window.workspace.plot_panes_mut().next().unwrap();
        pane.owner = owner(name, generation);
        pane.add_trace(FieldId(id as u32 + 50));
        pane.traces[0].owner = owner(name, generation);
        window
    }

    fn window_ids(windows: &[crate::shell::windows::ExtendedWindow]) -> Vec<u64> {
        windows.iter().map(|window| window.id.0).collect()
    }

    #[test]
    fn commit_removes_stale_generation_owned_panes_and_windows() {
        let mut workspace = crate::shell::workspace::Workspace::new();
        let stale = owned_pane(&mut workspace, "flight.py", 1, 1);
        let current = owned_pane(&mut workspace, "flight.py", 2, 2);
        let mut windows = vec![
            owned_window(1, "flight.py", 1),
            owned_window(2, "flight.py", 2),
            owned_window(3, "other.py", 1),
        ];

        run_sweep(
            &mut workspace,
            &mut windows,
            &Sweep::Commit {
                owner: "flight.py".into(),
                generation: 2,
            },
        );

        assert!(workspace.plot_pane_mut(stale).is_none());
        assert!(workspace.plot_pane_mut(current).is_some());
        assert_eq!(window_ids(&windows), [2, 3]);
    }

    #[test]
    fn rollback_removes_panes_and_windows_opened_in_the_failed_generation() {
        let mut workspace = crate::shell::workspace::Workspace::new();
        let previous = owned_pane(&mut workspace, "flight.py", 1, 1);
        let failed = owned_pane(&mut workspace, "flight.py", 2, 2);
        let mut windows = vec![
            owned_window(1, "flight.py", 1),
            owned_window(2, "flight.py", 2),
        ];

        run_sweep(
            &mut workspace,
            &mut windows,
            &Sweep::Rollback {
                owner: "flight.py".into(),
                generation: 2,
            },
        );

        assert!(workspace.plot_pane_mut(previous).is_some());
        assert!(workspace.plot_pane_mut(failed).is_none());
        assert_eq!(window_ids(&windows), [1]);
    }

    fn every_sweep() -> [Sweep; 3] {
        [
            Sweep::Commit {
                owner: "flight.py".into(),
                generation: 2,
            },
            Sweep::Rollback {
                owner: "flight.py".into(),
                generation: 1,
            },
            Sweep::RemoveOwned {
                owner: "flight.py".into(),
            },
        ]
    }

    #[test]
    fn an_owned_pane_holding_a_manual_trace_survives_with_only_owned_content_removed() {
        for sweep in every_sweep() {
            let mut workspace = crate::shell::workspace::Workspace::new();
            let tile = owned_pane(&mut workspace, "flight.py", 1, 1);
            workspace.plot_pane_mut(tile).unwrap().add_trace(FieldId(9));
            let mut windows = vec![owned_window(1, "flight.py", 1)];
            windows[0]
                .workspace
                .plot_panes_mut()
                .next()
                .unwrap()
                .add_trace(FieldId(8));

            run_sweep(&mut workspace, &mut windows, &sweep);

            let pane = workspace
                .plot_pane_mut(tile)
                .expect("manual trace keeps the pane");
            assert_eq!(
                pane.traces
                    .iter()
                    .map(|trace| trace.field)
                    .collect::<Vec<_>>(),
                [FieldId(9)],
                "{sweep:?}"
            );
            assert_eq!(window_ids(&windows), [1], "{sweep:?}");
            let pane = windows[0].workspace.plot_panes().next().unwrap();
            assert_eq!(
                pane.traces
                    .iter()
                    .map(|trace| trace.field)
                    .collect::<Vec<_>>(),
                [FieldId(8)],
                "{sweep:?}"
            );
        }
    }

    #[test]
    fn an_owned_pane_holding_a_manual_annotation_survives_with_only_owned_content_removed() {
        use crate::plotting::annotations::{DataPos, Geometry};

        for sweep in every_sweep() {
            let mut workspace = crate::shell::workspace::Workspace::new();
            let tile = owned_pane(&mut workspace, "flight.py", 1, 1);
            let pane = workspace.plot_pane_mut(tile).unwrap();
            let manual = pane.annotations.add_geometry(Geometry::Text {
                at: DataPos { t_us: 0, y: 0.0 },
            });
            let owned = pane.annotations.add_geometry(Geometry::Text {
                at: DataPos { t_us: 1, y: 0.0 },
            });
            pane.annotations.get_mut(owned).unwrap().owner = owner("flight.py", 1);
            let mut windows = vec![owned_window(1, "flight.py", 1)];
            windows[0]
                .workspace
                .plot_panes_mut()
                .next()
                .unwrap()
                .annotations
                .add_geometry(Geometry::Text {
                    at: DataPos { t_us: 2, y: 0.0 },
                });

            run_sweep(&mut workspace, &mut windows, &sweep);

            let pane = workspace
                .plot_pane_mut(tile)
                .expect("manual annotation keeps the pane");
            assert!(pane.traces.is_empty(), "{sweep:?}");
            assert_eq!(
                pane.annotations
                    .items()
                    .iter()
                    .map(|annotation| annotation.id)
                    .collect::<Vec<_>>(),
                [manual],
                "{sweep:?}"
            );
            assert_eq!(window_ids(&windows), [1], "{sweep:?}");
            let pane = windows[0].workspace.plot_panes().next().unwrap();
            assert!(pane.traces.is_empty(), "{sweep:?}");
            assert_eq!(pane.annotations.items().len(), 1, "{sweep:?}");
        }
    }

    #[test]
    fn a_retained_window_keeps_its_tree_identity_when_its_owned_panes_close() {
        let mut window = owned_window(4, "flight.py", 1);
        let root = window.workspace.tree.root().unwrap();
        let holder = window
            .workspace
            .split_plot(root, crate::shell::workspace::SplitDirection::Vertical)
            .unwrap();
        let pane = window.workspace.plot_pane_mut(holder).unwrap();
        pane.owner = owner("flight.py", 1);
        pane.add_trace(FieldId(7));
        let tree_id = window.workspace.tree.id();
        let mut windows = vec![window];
        let mut workspace = crate::shell::workspace::Workspace::new();

        run_sweep(
            &mut workspace,
            &mut windows,
            &Sweep::RemoveOwned {
                owner: "flight.py".into(),
            },
        );

        assert_eq!(window_ids(&windows), [4]);
        assert_eq!(windows[0].workspace.tree.id(), tree_id);
        assert_eq!(windows[0].workspace.plot_panes().count(), 1);
    }
}
