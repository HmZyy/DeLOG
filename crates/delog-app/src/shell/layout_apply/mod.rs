use std::collections::{BTreeMap, HashMap, HashSet};

use delog_core::diagnostics::Diag;
use delog_core::identity::SourceId;
use delog_core::snapshot::StoreSnapshot;

use crate::plotting::plot::{GhostTrace, PlotPane, TraceMode, TraceRef};
use crate::scene3d::camera::OrbitCamera;
use crate::scene3d::trail::TrailMode;
use crate::scene3d::vehicle::VehicleConfig;
use crate::shell::windows::{ExtendedWindow, MIN_WINDOW_SIZE, WindowId};
use crate::shell::workspace::{Pane, Scene3dPane, Workspace};

use crate::config::layout::doc::{
    AmbiguousField, AnnotationLayout, CameraLayout, FieldRef, LAYOUT_VERSION, LayoutDoc,
    LayoutError, LayoutNode, PlaybackLayout, Resolver, SceneLayout, SplitLayout, TraceLayout,
    TraceModeLayout, TrailModeLayout, WindowLayout, WorkspaceLayout, collect_field_refs, field_ref,
    vehicle_from_layout, vehicle_to_layout,
};
use crate::plotting::annotations::{Annotation, AnnotationOwner, DataPos, Geometry, Style};

pub struct LayoutApply {
    pub workspace: Workspace,
    pub windows: Vec<ExtendedWindow>,
    pub fit_all: bool,
    pub speed: f64,
    pub follow_live: bool,
    pub vehicles: Vec<VehicleConfig>,
    pub diagnostics: Vec<Diag>,
    #[cfg_attr(not(feature = "scripting"), allow(dead_code))]
    pub report: LayoutReport,
}

#[cfg_attr(not(feature = "scripting"), allow(dead_code))]
#[derive(Clone, Debug, Default)]
pub struct LayoutReport {
    pub ambiguous: Vec<AmbiguousField>,
    pub unresolved: Vec<FieldRef>,
    pub warnings: Vec<String>,
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

fn trail_mode_to_layout(mode: TrailMode) -> TrailModeLayout {
    match mode {
        TrailMode::ToPlayhead => TrailModeLayout::ToPlayhead,
        TrailMode::VisibleWindow => TrailModeLayout::VisibleWindow,
        TrailMode::Full => TrailModeLayout::Full,
    }
}

fn trail_mode_from_layout(mode: TrailModeLayout) -> TrailMode {
    match mode {
        TrailModeLayout::ToPlayhead => TrailMode::ToPlayhead,
        TrailModeLayout::VisibleWindow => TrailMode::VisibleWindow,
        TrailModeLayout::Full => TrailMode::Full,
    }
}

pub struct CurrentLayout<'a> {
    pub name: String,
    pub workspace: &'a Workspace,
    pub windows: &'a [ExtendedWindow],
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

    #[allow(dead_code)]
    pub fn ambiguities(&self) -> &[AmbiguousField] {
        &self.ambiguities
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
        windows: input
            .windows
            .iter()
            .map(|window| WindowLayout {
                id: Some(window.id.0),
                title: window.title.clone(),
                size: window.size,
                root: workspace_doc(&window.workspace, input.snapshot).root,
            })
            .collect(),
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
            annotations: Vec::new(),
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
            annotations: pane
                .annotations
                .items()
                .iter()
                .map(annotation_to_layout)
                .collect(),
        }),
        egui_tiles::Tile::Pane(Pane::Scene3D(scene)) => Some(LayoutNode::Scene3d(SceneLayout {
            camera: CameraLayout {
                yaw: scene.camera.yaw,
                pitch: scene.camera.pitch,
                distance: scene.camera.distance,
            },
            tracked_vehicle: scene.tracked_vehicle,
            trail_mode: trail_mode_to_layout(scene.trail_mode),
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

fn annotation_to_layout(annotation: &Annotation) -> AnnotationLayout {
    let point = |p: DataPos| [p.t_us as f64, p.y];
    let (kind, points, y) = match annotation.geom {
        Geometry::Text { at } => ("text", vec![point(at)], None),
        Geometry::Segment { from, to } => ("segment", vec![point(from), point(to)], None),
        Geometry::Rect { a, b } => ("rect", vec![point(a), point(b)], None),
        Geometry::Ellipse { a, b } => ("ellipse", vec![point(a), point(b)], None),
        Geometry::HLine { y } => ("hline", Vec::new(), Some(y)),
    };
    AnnotationLayout {
        kind: kind.to_owned(),
        points,
        y,
        label: annotation.label.clone(),
        color: annotation.style.color,
        stroke_px: annotation.style.stroke_px,
        fill_opacity: annotation.style.fill_opacity,
        font_px: annotation.style.font_px,
        arrow: annotation.style.arrow,
        owner: annotation.owner.as_ref().map(|owner| owner.name.clone()),
    }
}

fn geometry_from_layout(layout: &AnnotationLayout) -> Option<Geometry> {
    let point = |p: &[f64; 2]| DataPos {
        t_us: p[0] as i64,
        y: p[1],
    };
    match (layout.kind.as_str(), layout.points.as_slice()) {
        ("text", [a]) => Some(Geometry::Text { at: point(a) }),
        ("segment", [a, b]) => Some(Geometry::Segment {
            from: point(a),
            to: point(b),
        }),
        ("rect", [a, b]) => Some(Geometry::Rect {
            a: point(a),
            b: point(b),
        }),
        ("ellipse", [a, b]) => Some(Geometry::Ellipse {
            a: point(a),
            b: point(b),
        }),
        ("hline", []) => layout.y.map(|y| Geometry::HLine { y }),
        _ => None,
    }
}

fn window_ids(layouts: &[WindowLayout]) -> Vec<WindowId> {
    let mut used: HashSet<u64> = HashSet::new();
    let mut ids = Vec::with_capacity(layouts.len());
    for (index, layout) in layouts.iter().enumerate() {
        let wanted = layout.id.unwrap_or(index as u64 + 1).max(1);
        let id = if used.insert(wanted) {
            wanted
        } else {
            let free = used.iter().copied().max().unwrap_or(0) + 1;
            used.insert(free);
            free
        };
        ids.push(WindowId(id));
    }
    ids
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
        unresolved: std::collections::BTreeSet::new(),
        warnings: Vec::new(),
    };
    if collect_ambiguities {
        collect_field_refs(&doc, &mut resolver);
        if !resolver.ambiguities.is_empty() {
            return Err(resolver.ambiguities.into_values().collect());
        }
    }
    let workspace = workspace_from_layout(&doc.workspace, &mut resolver, WindowId::MAIN);
    let ids = window_ids(&doc.windows);
    let windows = doc
        .windows
        .iter()
        .zip(ids)
        .map(|(layout, id)| {
            let mut window = ExtendedWindow::restored(id);
            window.title = layout.title.clone();
            window.size = [
                layout.size[0].max(MIN_WINDOW_SIZE[0]),
                layout.size[1].max(MIN_WINDOW_SIZE[1]),
            ];
            window.workspace = workspace_from_layout(
                &WorkspaceLayout {
                    root: plots_only(&layout.root),
                },
                &mut resolver,
                id,
            );
            window
        })
        .collect::<Vec<_>>();
    let vehicles = doc
        .vehicles
        .iter()
        .filter_map(|v| vehicle_from_layout(v, &mut resolver))
        .collect::<Vec<_>>();

    let report = LayoutReport {
        ambiguous: resolver.ambiguities.into_values().collect(),
        unresolved: resolver.unresolved.into_iter().collect(),
        warnings: resolver.warnings,
    };

    Ok(LayoutApply {
        workspace,
        windows,
        fit_all: true,
        speed: doc.playback.speed,
        follow_live: doc.playback.follow_live,
        vehicles,
        diagnostics: resolver.diagnostics,
        report,
    })
}

fn plots_only(node: &LayoutNode) -> LayoutNode {
    match node {
        LayoutNode::Scene3d(_) => LayoutNode::Plot {
            traces: Vec::new(),
            show_legend: true,
            show_tooltip: true,
            annotations: Vec::new(),
        },
        LayoutNode::Split { split, children } => LayoutNode::Split {
            split: *split,
            children: children.iter().map(plots_only).collect(),
        },
        LayoutNode::Plot { .. } => node.clone(),
    }
}

fn workspace_from_layout(
    doc: &WorkspaceLayout,
    resolver: &mut Resolver<'_>,
    window: WindowId,
) -> Workspace {
    let mut tiles = egui_tiles::Tiles::default();
    let root = insert_node(&mut tiles, &doc.root, resolver)
        .unwrap_or_else(|| tiles.insert_pane(Pane::Plot(PlotPane::default())));
    Workspace {
        tree: egui_tiles::Tree::new(egui::Id::new(("plot_workspace", window.0)), root, tiles),
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
            annotations,
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
            for annotation in annotations {
                restore_annotation(&mut pane, annotation, resolver);
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
            trail_mode: trail_mode_from_layout(scene.trail_mode),
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

fn restore_annotation(pane: &mut PlotPane, layout: &AnnotationLayout, resolver: &mut Resolver<'_>) {
    let Some(geom) = geometry_from_layout(layout) else {
        let warning = format!(
            "annotation \"{}\" has {} point(s), which doesn't match kind \"{}\"; skipped",
            layout.label,
            layout.points.len(),
            layout.kind
        );
        resolver
            .diagnostics
            .push(Diag::warning("layout", warning.clone()));
        resolver.warnings.push(warning);
        return;
    };
    let id = pane.annotations.add_geometry(geom);
    let restored = pane
        .annotations
        .get_mut(id)
        .expect("the annotation was just inserted");
    restored.label = layout.label.clone();
    restored.style = Style {
        color: layout.color,
        stroke_px: layout.stroke_px,
        fill_opacity: layout.fill_opacity,
        font_px: layout.font_px,
        arrow: layout.arrow,
    };
    restored.owner = layout.owner.clone().map(|name| AnnotationOwner {
        name,
        generation: 0,
    });
}

fn trace_from_layout(trace: &TraceLayout, resolver: &mut Resolver<'_>) -> Option<TraceRef> {
    Some(TraceRef {
        field: resolver.resolve(&trace.field)?,
        color: trace.color,
        width_px: trace.width_px,
        mode: trace.mode.into(),
        visible: trace.visible,
        label_override: None,
        #[cfg(feature = "scripting")]
        owner: None,
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
