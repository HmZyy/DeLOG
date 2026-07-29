use std::collections::{BTreeMap, HashMap};

use delog_core::diagnostics::Diag;
use delog_core::identity::SourceId;
use delog_core::snapshot::StoreSnapshot;

use crate::scene3d::camera::OrbitCamera;
use crate::plotting::plot::{GhostTrace, PlotPane, TraceMode, TraceRef};
use crate::scene3d::vehicle::VehicleConfig;
use crate::shell::workspace::{Pane, Scene3dPane, Workspace};

use crate::config::layout::doc::{
    AmbiguousField, CameraLayout, FieldRef, LAYOUT_VERSION, LayoutDoc, LayoutError, LayoutNode,
    PlaybackLayout, Resolver, SceneLayout, SplitLayout, TraceLayout, TraceModeLayout,
    WorkspaceLayout, collect_field_refs, field_ref, vehicle_from_layout, vehicle_to_layout,
};

pub struct LayoutApply {
    pub workspace: Workspace,
    pub fit_all: bool,
    pub speed: f64,
    pub follow_live: bool,
    pub vehicles: Vec<VehicleConfig>,
    pub diagnostics: Vec<Diag>,
}

#[derive(Clone, Debug)]
pub struct PendingLayout {
    pub name: String,
    doc: LayoutDoc,
    ambiguities: Vec<AmbiguousField>,
}

pub enum LoadOutcome {
    Applied(LayoutApply),
    NeedsMapping(PendingLayout),
}

pub struct CurrentLayout<'a> {
    pub name: String,
    pub workspace: &'a Workspace,
    pub snapshot: &'a StoreSnapshot,
    pub speed: f64,
    pub follow_live: bool,
    pub vehicles: &'a [VehicleConfig],
}

impl PendingLayout {
    pub fn ambiguities_mut(&mut self) -> &mut [AmbiguousField] {
        &mut self.ambiguities
    }

    pub fn ambiguity_count(&self) -> usize {
        self.ambiguities.len()
    }

    pub fn apply(self, snapshot: &StoreSnapshot) -> LayoutApply {
        let choices = self
            .ambiguities
            .iter()
            .filter_map(|a| {
                a.candidates
                    .get(a.selected)
                    .map(|c| (a.field.clone(), c.source))
            })
            .collect();
        apply_doc(self.doc, snapshot, &choices, false).expect("choices resolve ambiguities")
    }

    pub fn apply_skipping(self, snapshot: &StoreSnapshot) -> LayoutApply {
        apply_doc(self.doc, snapshot, &HashMap::new(), false).expect("skip mode cannot block")
    }
}

pub fn load_doc(doc: LayoutDoc, snapshot: &StoreSnapshot) -> Result<LoadOutcome, LayoutError> {
    if doc.delog_layout != LAYOUT_VERSION {
        return Err(LayoutError::UnsupportedVersion(doc.delog_layout));
    }
    match apply_doc(doc.clone(), snapshot, &HashMap::new(), true) {
        Ok(applied) => Ok(LoadOutcome::Applied(applied)),
        Err(ambiguities) => Ok(LoadOutcome::NeedsMapping(PendingLayout {
            name: doc.name.clone(),
            doc,
            ambiguities,
        })),
    }
}

pub fn current_doc(input: CurrentLayout<'_>) -> LayoutDoc {
    LayoutDoc {
        delog_layout: LAYOUT_VERSION,
        name: input.name,
        playback: PlaybackLayout {
            speed: input.speed,
            follow_live: input.follow_live,
        },
        workspace: workspace_doc(input.workspace, input.snapshot),
        vehicles: input
            .vehicles
            .iter()
            .filter_map(|v| vehicle_to_layout(v, input.snapshot))
            .collect(),
    }
}

fn workspace_doc(workspace: &Workspace, snapshot: &StoreSnapshot) -> WorkspaceLayout {
    let root = workspace
        .tree
        .root()
        .and_then(|id| node_to_layout(workspace, snapshot, id))
        .unwrap_or(LayoutNode::Plot {
            traces: Vec::new(),
            show_legend: true,
            show_tooltip: true,
        });
    WorkspaceLayout { root }
}

fn node_to_layout(
    workspace: &Workspace,
    snapshot: &StoreSnapshot,
    tile: egui_tiles::TileId,
) -> Option<LayoutNode> {
    match workspace.tree.tiles.get(tile)? {
        egui_tiles::Tile::Pane(Pane::Plot(pane)) => Some(LayoutNode::Plot {
            traces: pane
                .traces
                .iter()
                .filter_map(|t| trace_to_layout(t, snapshot))
                .chain(pane.ghosts.iter().map(ghost_to_layout))
                .collect(),
            show_legend: pane.show_legend,
            show_tooltip: pane.show_tooltip,
        }),
        egui_tiles::Tile::Pane(Pane::Scene3D(scene)) => Some(LayoutNode::Scene3d(SceneLayout {
            camera: CameraLayout {
                yaw: scene.camera.yaw,
                pitch: scene.camera.pitch,
                distance: scene.camera.distance,
            },
            tracked_vehicle: scene.tracked_vehicle,
            trail_to_playhead: scene.trail_to_playhead,
        })),
        egui_tiles::Tile::Container(container) => {
            let children = container
                .children()
                .filter_map(|&child| node_to_layout(workspace, snapshot, child))
                .collect();
            Some(LayoutNode::Split {
                split: match container.kind() {
                    egui_tiles::ContainerKind::Tabs => SplitLayout::Tabs,
                    egui_tiles::ContainerKind::Horizontal => SplitLayout::Horizontal,
                    egui_tiles::ContainerKind::Vertical => SplitLayout::Vertical,
                    egui_tiles::ContainerKind::Grid => SplitLayout::Grid,
                },
                children,
            })
        }
    }
}

fn trace_to_layout(trace: &TraceRef, snapshot: &StoreSnapshot) -> Option<TraceLayout> {
    Some(TraceLayout {
        field: field_ref(snapshot, trace.field)?,
        color: trace.color,
        width_px: trace.width_px,
        mode: trace.mode.into(),
        visible: trace.visible,
    })
}

fn ghost_to_layout(ghost: &GhostTrace) -> TraceLayout {
    TraceLayout {
        field: FieldRef {
            topic: ghost.topic.clone(),
            field: ghost.field.clone(),
        },
        color: ghost.color,
        width_px: ghost.width_px,
        mode: ghost.mode.into(),
        visible: ghost.visible,
    }
}

fn apply_doc(
    doc: LayoutDoc,
    snapshot: &StoreSnapshot,
    choices: &HashMap<FieldRef, SourceId>,
    collect_ambiguities: bool,
) -> Result<LayoutApply, Vec<AmbiguousField>> {
    let mut resolver = Resolver {
        snapshot,
        choices,
        diagnostics: Vec::new(),
        ambiguities: BTreeMap::new(),
        collect_ambiguities,
    };
    if collect_ambiguities {
        collect_field_refs(&doc, &mut resolver);
        if !resolver.ambiguities.is_empty() {
            return Err(resolver.ambiguities.into_values().collect());
        }
    }
    let workspace = workspace_from_layout(&doc.workspace, &mut resolver);
    let vehicles = doc
        .vehicles
        .iter()
        .filter_map(|v| vehicle_from_layout(v, &mut resolver))
        .collect::<Vec<_>>();

    Ok(LayoutApply {
        workspace,
        fit_all: true,
        speed: doc.playback.speed,
        follow_live: doc.playback.follow_live,
        vehicles,
        diagnostics: resolver.diagnostics,
    })
}

fn workspace_from_layout(doc: &WorkspaceLayout, resolver: &mut Resolver<'_>) -> Workspace {
    let mut tiles = egui_tiles::Tiles::default();
    let root = insert_node(&mut tiles, &doc.root, resolver)
        .unwrap_or_else(|| tiles.insert_pane(Pane::Plot(PlotPane::default())));
    Workspace {
        tree: egui_tiles::Tree::new("plot_workspace", root, tiles),
        focused: Some(root),
        shared_y_gutter: 0.0,
        default_show_legend: true,
    }
}

fn insert_node(
    tiles: &mut egui_tiles::Tiles<Pane>,
    node: &LayoutNode,
    resolver: &mut Resolver<'_>,
) -> Option<egui_tiles::TileId> {
    match node {
        LayoutNode::Plot {
            traces,
            show_legend,
            show_tooltip,
        } => {
            let mut pane = PlotPane {
                show_legend: *show_legend,
                show_tooltip: *show_tooltip,
                ..PlotPane::default()
            };
            for trace in traces {
                match trace_from_layout(trace, resolver) {
                    Some(resolved) => pane.traces.push(resolved),
                    None => pane.add_ghost(ghost_from_layout(trace)),
                }
            }
            Some(tiles.insert_pane(Pane::Plot(pane)))
        }
        LayoutNode::Scene3d(scene) => Some(tiles.insert_pane(Pane::Scene3D(Scene3dPane {
            camera: OrbitCamera {
                target: glam::Vec3::ZERO,
                yaw: scene.camera.yaw,
                pitch: scene.camera.pitch,
                distance: scene.camera.distance,
            },
            tracked_vehicle: scene.tracked_vehicle,
            trail_to_playhead: scene.trail_to_playhead,
            ..Scene3dPane::default()
        }))),
        LayoutNode::Split { split, children } => {
            let child_ids = children
                .iter()
                .filter_map(|child| insert_node(tiles, child, resolver))
                .collect::<Vec<_>>();
            if child_ids.is_empty() {
                None
            } else if child_ids.len() == 1 {
                child_ids.first().copied()
            } else {
                Some(tiles.insert_container(egui_tiles::Container::new(
                    match split {
                        SplitLayout::Tabs => egui_tiles::ContainerKind::Tabs,
                        SplitLayout::Horizontal => egui_tiles::ContainerKind::Horizontal,
                        SplitLayout::Vertical => egui_tiles::ContainerKind::Vertical,
                        SplitLayout::Grid => egui_tiles::ContainerKind::Grid,
                    },
                    child_ids,
                )))
            }
        }
    }
}

fn trace_from_layout(trace: &TraceLayout, resolver: &mut Resolver<'_>) -> Option<TraceRef> {
    Some(TraceRef {
        field: resolver.resolve(&trace.field)?,
        color: trace.color,
        width_px: trace.width_px,
        mode: trace.mode.into(),
        visible: trace.visible,
        label_override: None,
    })
}

fn ghost_from_layout(trace: &TraceLayout) -> GhostTrace {
    GhostTrace {
        source: None,
        topic: trace.field.topic.clone(),
        field: trace.field.field.clone(),
        color: trace.color,
        width_px: trace.width_px,
        mode: trace.mode.into(),
        visible: trace.visible,
        text_filter: None,
        text_offsets: Vec::new(),
    }
}

impl From<TraceMode> for TraceModeLayout {
    fn from(value: TraceMode) -> Self {
        match value {
            TraceMode::Line => Self::Line,
            TraceMode::Scatter => Self::Scatter,
            TraceMode::Step => Self::Step,
        }
    }
}

impl From<TraceModeLayout> for TraceMode {
    fn from(value: TraceModeLayout) -> Self {
        match value {
            TraceModeLayout::Line => Self::Line,
            TraceModeLayout::Scatter => Self::Scatter,
            TraceModeLayout::Step => Self::Step,
        }
    }
}

#[cfg(test)]
mod tests;
