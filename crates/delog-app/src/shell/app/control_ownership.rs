use delog_core::identity::FieldId;
use delog_script::{GenerationRequest, ScriptOwner};

use super::control_service::AppControl;
use crate::plotting::annotations::AnnotationOwner;
use crate::plotting::plot::PlotPane;
use crate::scene3d::vehicle::{VehicleConfig, VehicleOwner};

#[derive(Debug)]
pub enum Sweep {
    Commit { owner: String, generation: u64 },
    Rollback { owner: String, generation: u64 },
}

impl From<GenerationRequest> for Sweep {
    fn from(request: GenerationRequest) -> Self {
        match request {
            GenerationRequest::Commit { owner, generation } => Self::Commit { owner, generation },
            GenerationRequest::Rollback { owner, generation } => {
                Self::Rollback { owner, generation }
            }
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

fn sweep_pane(pane: &mut PlotPane, sweep: &Sweep) -> Vec<FieldId> {
    let removed: Vec<FieldId> = pane
        .traces
        .iter()
        .filter(|trace| sweep.removes(trace.owner.as_ref()))
        .map(|trace| trace.field)
        .collect();
    sweep_vec(&mut pane.traces, |trace| trace.owner.as_ref(), sweep);
    pane.annotations
        .retain(|annotation| !sweep.removes_annotation(annotation.owner.as_ref()));
    removed
}

fn sweep_vehicles(vehicles: &mut Vec<VehicleConfig>, sweep: &Sweep) -> bool {
    let before = vehicles.len();
    vehicles.retain(|vehicle| !sweep.removes_vehicle(vehicle.runtime.owner.as_ref()));
    vehicles.len() != before
}

pub fn apply_sweep(control: &mut AppControl<'_>, sweep: &Sweep) {
    match sweep {
        Sweep::Commit { owner, generation } => {
            control.markers.sweep_generation(owner, *generation, true);
        }
        Sweep::Rollback { owner, generation } => {
            control.markers.sweep_generation(owner, *generation, false);
        }
    }
    let mut removed_fields = Vec::new();
    for pane in control.workspace.plot_panes_mut() {
        removed_fields.extend(sweep_pane(pane, sweep));
    }
    for window in control.windows.iter_mut() {
        for pane in window.workspace.plot_panes_mut() {
            removed_fields.extend(sweep_pane(pane, sweep));
        }
    }
    for field in removed_fields {
        control.caches.unpin(field);
    }
    if sweep_vehicles(control.vehicles, sweep) {
        super::control_service::mark_vehicles_changed(control);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delog_script::ScriptOwner;

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
        drop(control);

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
        pane.annotations.get_mut(id).unwrap().owner = owner("flight.py", 0).map(Into::into);

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
        pane.annotations.get_mut(id).unwrap().owner = owner("flight.py", 0).map(Into::into);

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
            pane.annotations.get_mut(id).unwrap().owner = owned.clone().map(Into::into);
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
}
