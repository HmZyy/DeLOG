//! Tiled plot workspace.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use delog_cache::CacheManager;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::axes;
use crate::camera::OrbitCamera;
use crate::gpu::{self, GpuBridge, PaneView, VehicleDraw};
use crate::hover::{self, HoverTarget};
use crate::legend;
use crate::map::mercator;
use crate::map::provider::{MapProviderId, provider};
use crate::map::worker::{MapScopeId, TileFailureClass, TileManager, TileRequest};
use crate::plot::{GhostTrace, PlotPane, TraceMode, TraceRef, ViewX};
use crate::vehicle;

pub type TileTree = egui_tiles::Tree<Pane>;

#[derive(Debug)]
pub enum Pane {
    Plot(PlotPane),
    Scene3D(Scene3dPane),
}

#[derive(Debug)]
pub struct Scene3dPane {
    pub(crate) map_scope: MapScopeId,
    pub camera: OrbitCamera,
    pub tracked_vehicle: Option<usize>,
    /// When true, each vehicle's path is clipped to the playhead time; when
    /// false, the full flight path is drawn.
    pub trail_to_playhead: bool,
    pub(crate) map_selection: Option<(usize, MapProviderId, [u64; 3])>,
    pub(crate) map_generation: u64,
    pub(crate) map_zoom: Option<u8>,
    pub(crate) map_previous_zoom: Option<u8>,
    pub(crate) map_tiles: Vec<(crate::map::provider::TileId, i32)>,
    pub(crate) map_previous_tiles: Vec<crate::map::provider::TileId>,
}

impl Scene3dPane {
    fn update_map_selection(&mut self, selection: Option<(usize, MapProviderId, [u64; 3])>) -> u64 {
        if self.map_selection != selection {
            self.map_selection = selection;
            self.map_generation = self.map_generation.wrapping_add(1).max(1);
        }
        self.map_generation
    }
}

impl Default for Scene3dPane {
    fn default() -> Self {
        static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
        Self {
            map_scope: MapScopeId(NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)),
            camera: OrbitCamera::default(),
            tracked_vehicle: None,
            trail_to_playhead: true,
            map_selection: None,
            map_generation: 0,
            map_zoom: None,
            map_previous_zoom: None,
            map_tiles: Vec::new(),
            map_previous_tiles: Vec::new(),
        }
    }
}

impl Default for Pane {
    fn default() -> Self {
        Self::Plot(PlotPane::default())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropEdge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy)]
struct PlotDebug {
    plot_rect: egui::Rect,
    x_range: (f32, f32),
    y_range: (f32, f32),
    y_query_us: f32,
    paint_us: f32,
}

impl DropEdge {
    fn from_pos(rect: egui::Rect, pos: egui::Pos2) -> Option<Self> {
        if !rect.contains(pos) {
            return None;
        }

        let edge_w = (rect.width() * 0.18).clamp(24.0, 72.0);
        let edge_h = (rect.height() * 0.18).clamp(24.0, 72.0);
        let distances = [
            (Self::Left, pos.x - rect.left(), edge_w),
            (Self::Right, rect.right() - pos.x, edge_w),
            (Self::Top, pos.y - rect.top(), edge_h),
            (Self::Bottom, rect.bottom() - pos.y, edge_h),
        ];
        distances
            .into_iter()
            .filter(|(_, distance, limit)| *distance <= *limit)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(edge, _, _)| edge)
    }

    fn split_direction(self) -> SplitDirection {
        match self {
            Self::Left | Self::Right => SplitDirection::Horizontal,
            Self::Top | Self::Bottom => SplitDirection::Vertical,
        }
    }

    fn insert_before(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }
}

pub struct Workspace {
    pub tree: TileTree,
    pub focused: Option<egui_tiles::TileId>,
    /// Widest Y-axis gutter any plot pane needed last frame; every pane uses at
    /// least this much left margin so stacked plots stay vertically aligned even
    /// when their Y ranges differ wildly.
    pub shared_y_gutter: f32,
    /// Legend visibility seeded into newly created panes; the per-pane toggle
    /// overrides it afterwards.
    pub default_show_legend: bool,
}

impl Workspace {
    pub fn new() -> Self {
        let mut tiles = egui_tiles::Tiles::default();
        let root = tiles.insert_pane(Pane::Plot(PlotPane::default()));
        Self {
            tree: egui_tiles::Tree::new("plot_workspace", root, tiles),
            focused: None,
            shared_y_gutter: 0.0,
            default_show_legend: true,
        }
    }

    pub fn focused_first_field(&self) -> Option<FieldId> {
        let tile_id = self.focused?;
        match self.tree.tiles.get(tile_id) {
            Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) => pane.traces.first().map(|t| t.field),
            _ => None,
        }
    }

    pub fn fields(&self) -> impl Iterator<Item = FieldId> + '_ {
        self.plot_panes().flat_map(PlotPane::fields)
    }

    pub fn map_scopes(&self) -> Vec<MapScopeId> {
        self.tree
            .tiles
            .iter()
            .filter_map(|(_, tile)| match tile {
                egui_tiles::Tile::Pane(Pane::Scene3D(pane)) => Some(pane.map_scope),
                _ => None,
            })
            .collect()
    }

    pub fn resolve_ghosts(&mut self, snapshot: &StoreSnapshot) -> usize {
        let mut resolved = 0;
        for pane in self.plot_panes_mut() {
            let ghosts = std::mem::take(&mut pane.ghosts);
            for ghost in ghosts {
                if let Some(field) = resolve_ghost(snapshot, &ghost) {
                    if !pane.traces.iter().any(|t| t.field == field) {
                        pane.traces.push(TraceRef {
                            field,
                            color: ghost.color,
                            width_px: ghost.width_px,
                            mode: ghost.mode,
                            visible: ghost.visible,
                        });
                        apply_ghost_text_state(pane, &ghost, field);
                        resolved += 1;
                    }
                } else {
                    pane.ghosts.push(ghost);
                }
            }
        }
        resolved
    }

    pub fn prune_removed_fields(&mut self, snapshot: &StoreSnapshot) -> Vec<FieldId> {
        let mut removed = Vec::new();
        for pane in self.plot_panes_mut() {
            let mut i = 0;
            while i < pane.traces.len() {
                let field = pane.traces[i].field;
                if snapshot.is_field_live(field) {
                    i += 1;
                } else if let Some(replacement) = resolve_recreated_script_field(snapshot, field) {
                    if pane.traces.iter().any(|t| t.field == replacement) {
                        pane.traces.remove(i);
                    } else {
                        pane.traces[i].field = replacement;
                        rebind_text_state(pane, field, replacement);
                        i += 1;
                    }
                } else if let Some(ghost) = script_ghost_from_removed_trace(snapshot, pane, i) {
                    pane.traces.remove(i);
                    pane.add_ghost(ghost);
                    removed.push(field);
                } else {
                    pane.traces.remove(i);
                    removed.push(field);
                }
            }
        }
        removed
    }

    pub fn add_trace_to_first_plot(&mut self, field: FieldId) -> bool {
        self.plot_panes_mut()
            .next()
            .is_some_and(|pane| pane.add_trace(field))
    }

    pub fn all_plot_legends_visible(&self) -> bool {
        self.plot_panes().all(|pane| pane.show_legend)
    }

    pub fn set_all_plot_legends(&mut self, visible: bool) {
        self.default_show_legend = visible;
        for pane in self.plot_panes_mut() {
            pane.show_legend = visible;
        }
    }

    pub fn equalize_plot_heights(&mut self) {
        let container_ids = self
            .tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| matches!(tile, egui_tiles::Tile::Container(_)).then_some(*id))
            .collect::<Vec<_>>();
        for id in container_ids {
            let Some(egui_tiles::Tile::Container(container)) = self.tree.tiles.get_mut(id) else {
                continue;
            };
            match container {
                egui_tiles::Container::Linear(linear) => {
                    for child in linear.children.clone() {
                        linear.shares.set_share(child, 1.0);
                    }
                }
                egui_tiles::Container::Grid(grid) => {
                    for share in &mut grid.col_shares {
                        *share = 1.0;
                    }
                    for share in &mut grid.row_shares {
                        *share = 1.0;
                    }
                }
                egui_tiles::Container::Tabs(_) => {}
            }
        }
    }

    pub fn split_plot(&mut self, tile_id: egui_tiles::TileId, direction: SplitDirection) {
        self.split_plot_at(tile_id, direction, false);
    }

    pub fn split_plot_with_traces(
        &mut self,
        tile_id: egui_tiles::TileId,
        edge: DropEdge,
        fields: &[FieldId],
    ) -> Vec<FieldId> {
        if fields.is_empty() {
            return Vec::new();
        }
        let Some(new_pane) =
            self.split_plot_at(tile_id, edge.split_direction(), edge.insert_before())
        else {
            return Vec::new();
        };
        fields
            .iter()
            .copied()
            .filter(|&field| self.add_trace_to_plot(new_pane, field))
            .collect()
    }

    pub fn scene_pane_id(&self) -> Option<egui_tiles::TileId> {
        self.tree.tiles.iter().find_map(|(id, tile)| {
            matches!(tile, egui_tiles::Tile::Pane(Pane::Scene3D(_))).then_some(*id)
        })
    }

    pub fn toggle_scene_pane(&mut self) {
        if let Some(id) = self.scene_pane_id() {
            let closing_root = self.tree.root() == Some(id);
            self.tree.remove_recursively(id);
            if closing_root || self.tree.tiles.tiles().next().is_none() {
                *self = Self::new();
            }
            return;
        }
        let pane = self
            .tree
            .tiles
            .insert_pane(Pane::Scene3D(Scene3dPane::default()));
        match self
            .tree
            .root()
            .and_then(|root| self.attach_split(root, pane, SplitDirection::Horizontal, false))
        {
            Some(_) => {}
            None => self.tree.root = Some(pane),
        }
        self.focused = Some(pane);
    }

    fn split_plot_at(
        &mut self,
        tile_id: egui_tiles::TileId,
        direction: SplitDirection,
        before: bool,
    ) -> Option<egui_tiles::TileId> {
        let new_pane = self.tree.tiles.insert_pane(Pane::Plot(PlotPane {
            show_legend: self.default_show_legend,
            ..PlotPane::default()
        }));
        self.attach_split(tile_id, new_pane, direction, before)
    }

    fn attach_split(
        &mut self,
        tile_id: egui_tiles::TileId,
        new_pane: egui_tiles::TileId,
        direction: SplitDirection,
        before: bool,
    ) -> Option<egui_tiles::TileId> {
        let kind = match direction {
            SplitDirection::Horizontal => egui_tiles::ContainerKind::Horizontal,
            SplitDirection::Vertical => egui_tiles::ContainerKind::Vertical,
        };

        if self.tree.root() == Some(tile_id) {
            let children = ordered_pair(tile_id, new_pane, before);
            let root = self
                .tree
                .tiles
                .insert_container(egui_tiles::Container::new(kind, children));
            self.tree.root = Some(root);
            return Some(new_pane);
        }

        if let Some(parent_id) = self.tree.tiles.parent_of(tile_id) {
            // `Some(index)` = pane removed from parent; wrap it in a new `kind`
            // container and put it back at `index`.
            let wrap_at = {
                let Some(egui_tiles::Tile::Container(parent)) = self.tree.tiles.get_mut(parent_id)
                else {
                    return None;
                };

                if parent.kind() == kind {
                    match parent {
                        egui_tiles::Container::Linear(linear) => {
                            let index = linear
                                .children
                                .iter()
                                .position(|id| *id == tile_id)
                                .map_or(linear.children.len(), |i| i + usize::from(!before));
                            linear.children.insert(index, new_pane);
                        }
                        egui_tiles::Container::Tabs(tabs) => {
                            tabs.add_child(new_pane);
                            tabs.set_active(new_pane);
                        }
                        egui_tiles::Container::Grid(grid) => grid.add_child(new_pane),
                    }
                    None
                } else {
                    parent.remove_child(tile_id)
                }
            };

            if let Some(index) = wrap_at {
                let children = ordered_pair(tile_id, new_pane, before);
                let replacement = self
                    .tree
                    .tiles
                    .insert_container(egui_tiles::Container::new(kind, children));
                if let Some(egui_tiles::Tile::Container(parent)) =
                    self.tree.tiles.get_mut(parent_id)
                {
                    // Insert at `index`; appending would shuffle the pane to
                    // the end of the parent.
                    insert_child_at(parent, index, replacement);
                    if let egui_tiles::Container::Tabs(tabs) = parent {
                        tabs.set_active(replacement);
                    }
                }
            }
            return Some(new_pane);
        }

        None
    }

    fn add_trace_to_plot(&mut self, tile_id: egui_tiles::TileId, field: FieldId) -> bool {
        let Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) = self.tree.tiles.get_mut(tile_id)
        else {
            return false;
        };
        pane.add_trace(field)
    }

    pub fn close_plot(&mut self, tile_id: egui_tiles::TileId) -> Vec<FieldId> {
        let closing_root = self.tree.root() == Some(tile_id);
        let removed = self.tree.remove_recursively(tile_id);
        if closing_root || self.plot_panes().next().is_none() {
            *self = Self::new();
        }
        removed
            .into_iter()
            .flat_map(fields_from_removed_tile)
            .collect()
    }

    fn plot_panes(&self) -> impl Iterator<Item = &PlotPane> + '_ {
        self.tree.tiles.tiles().filter_map(|tile| match tile {
            egui_tiles::Tile::Pane(Pane::Plot(pane)) => Some(pane),
            egui_tiles::Tile::Pane(Pane::Scene3D(_)) | egui_tiles::Tile::Container(_) => None,
        })
    }

    fn plot_panes_mut(&mut self) -> impl Iterator<Item = &mut PlotPane> + '_ {
        self.tree.tiles.tiles_mut().filter_map(|tile| match tile {
            egui_tiles::Tile::Pane(Pane::Plot(pane)) => Some(pane),
            egui_tiles::Tile::Pane(Pane::Scene3D(_)) | egui_tiles::Tile::Container(_) => None,
        })
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WorkspaceImageAction {
    CopyPlot { rect: egui::Rect },
    ExportPlot { rect: egui::Rect },
}

impl WorkspaceImageAction {
    #[cfg(test)]
    pub fn rect(self) -> egui::Rect {
        match self {
            Self::CopyPlot { rect } | Self::ExportPlot { rect } => rect,
        }
    }
}

#[derive(Default)]
pub struct WorkspaceActions {
    pub split: Option<(egui_tiles::TileId, SplitDirection)>,
    pub edge_drop: Option<(egui_tiles::TileId, DropEdge, Vec<FieldId>)>,
    pub close: Option<egui_tiles::TileId>,
    pub remove_trace: Vec<FieldId>,
    pub focus: Option<egui_tiles::TileId>,
    pub scrub_to: Option<i64>,
    pub image: Option<WorkspaceImageAction>,
    /// Manual X-view change (pan/zoom/reset); unlocks live-tail mode.
    pub view_changed: bool,
    pub open_vehicle_config: bool,
    pub export_kml: bool,
    pub inspect_field_stats: Option<FieldId>,
    /// Widest Y gutter any pane needed; fed into `Workspace::shared_y_gutter`.
    pub max_y_gutter: f32,
}

pub struct PlotServices<'a> {
    pub frame: &'a eframe::Frame,
    pub snapshot: &'a Arc<StoreSnapshot>,
    pub metrics: &'a Arc<delog_core::metrics::MetricsRegistry>,
    pub gpu: &'a mut GpuBridge,
    pub tile_manager: Option<&'a mut TileManager>,
    pub caches: &'a mut CacheManager,
    pub view: &'a mut Option<ViewX>,
    pub origin_us: i64,
    pub hover_mode: &'a mut delog_core::field_view::SampleMode,
    /// When set, Alt+hover holds a sample until the cursor crosses to the next.
    pub snap_playhead: &'a mut bool,
    /// Shared measurement marker time.
    pub marker_us: &'a mut Option<i64>,
    pub render_tuning: crate::settings::RenderTuning,
    pub scene3d: crate::settings::Scene3dSettings,
    pub accent: egui::Color32,
    /// Playhead cursor time; `None` before any data loads.
    pub playhead_us: Option<i64>,
    pub playing: bool,
    pub vehicles: &'a [crate::vehicle::VehicleConfig],
    /// Render-space trajectories (points + per-point timestamps), parallel to
    /// `vehicles`.
    pub trajectories: &'a [crate::vehicle::VehicleTrajectory],
    /// Vehicle revision the cached trajectories were built at; lets the GPU
    /// upload only appended tail points.
    pub traj_generation: u64,
    pub shared_y_gutter: f32,
    pub plot_display: crate::settings::PlotDisplay,
    pub markers: &'a [crate::markers::Marker],
}

pub struct Behavior<'a> {
    services: PlotServices<'a>,
    actions: WorkspaceActions,
}

impl<'a> Behavior<'a> {
    pub fn new(services: PlotServices<'a>) -> Self {
        Self {
            services,
            actions: WorkspaceActions::default(),
        }
    }

    pub fn into_actions(self) -> WorkspaceActions {
        self.actions
    }
}

impl egui_tiles::Behavior<Pane> for Behavior<'_> {
    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        tile_id: egui_tiles::TileId,
        pane: &mut Pane,
    ) -> egui_tiles::UiResponse {
        // Wraps the whole callback (incl. the plot drop-zone, which sits outside
        // `pane_total`). `workspace_tree − Σ pane_ui` isolates egui_tiles' own
        // layout/tab/drag cost; `Σ pane_ui − Σ pane_total` is our per-pane
        // wrapper.
        let _pane_ui = self.services.metrics.scope("pane_ui");
        match pane {
            Pane::Plot(pane) => self.plot_ui(ui, tile_id, pane),
            Pane::Scene3D(pane) => self.scene_ui(ui, tile_id, pane),
        }
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        match pane {
            Pane::Plot(pane) if pane.is_empty() => "Plot".into(),
            Pane::Plot(pane) => {
                let count = pane.traces.len() + pane.ghosts.len();
                format!("Plot ({count})").into()
            }
            Pane::Scene3D(_) => "3D View".into(),
        }
    }

    fn tab_title_for_tile(
        &mut self,
        tiles: &egui_tiles::Tiles<Pane>,
        tile_id: egui_tiles::TileId,
    ) -> egui::WidgetText {
        match tiles.get(tile_id) {
            Some(egui_tiles::Tile::Pane(pane)) => self.tab_title_for_pane(pane),
            Some(egui_tiles::Tile::Container(_)) | None => "".into(),
        }
    }

    fn is_tab_closable(
        &self,
        tiles: &egui_tiles::Tiles<Pane>,
        tile_id: egui_tiles::TileId,
    ) -> bool {
        tiles.len() > 1 && tiles.get_pane(&tile_id).is_some()
    }

    fn on_tab_close(
        &mut self,
        tiles: &mut egui_tiles::Tiles<Pane>,
        tile_id: egui_tiles::TileId,
    ) -> bool {
        if let Some(Pane::Plot(pane)) = tiles.get_pane(&tile_id) {
            for field in pane.fields().collect::<Vec<_>>() {
                self.services.caches.unpin(field);
                self.actions.remove_trace.push(field);
            }
        }
        true
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions {
            prune_empty_tabs: true,
            prune_empty_containers: true,
            prune_single_child_tabs: true,
            prune_single_child_containers: true,
            all_panes_must_have_tabs: false,
            join_nested_linear_containers: true,
        }
    }
}

impl Behavior<'_> {
    fn scene_ui(
        &mut self,
        ui: &mut egui::Ui,
        tile_id: egui_tiles::TileId,
        pane: &mut Scene3dPane,
    ) -> egui_tiles::UiResponse {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        if response.clicked() || response.drag_started() || response.secondary_clicked() {
            self.actions.focus = Some(tile_id);
        }

        const SENS: f32 = 0.008;
        if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            pane.camera.orbit(-d.x * SENS, d.y * SENS);
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                pane.camera.zoom_with_max(
                    (0.9985_f32).powf(scroll),
                    self.services.scene3d.resolved_max_camera_distance_m(),
                );
            }
        }
        if response.double_clicked() {
            // Preserve the followed target; reset only the orbit offset.
            let target = pane.camera.target;
            pane.camera = OrbitCamera {
                target,
                ..OrbitCamera::default()
            };
        }

        let snapshot = self.services.snapshot;
        let playhead = self.services.playhead_us;
        let trail_to_playhead = pane.trail_to_playhead;
        // GPS-ref resolution split out of `pose_at` so it runs once per frame
        // and can be profiled (and later cached) separately from the reads.
        let gps_refs: Vec<Option<(f64, f64, f64)>> = {
            let _t = self.services.metrics.scope("scene_gpsref");
            self.services
                .vehicles
                .iter()
                .map(|v| {
                    (v.show && playhead.is_some())
                        .then(|| vehicle::gps_reference(snapshot, v))
                        .flatten()
                })
                .collect()
        };
        let poses: Vec<Option<vehicle::Pose>> = {
            let _t = self.services.metrics.scope("scene_poses");
            self.services
                .vehicles
                .iter()
                .enumerate()
                .map(|(i, v)| match (v.show, playhead) {
                    (true, Some(t)) => vehicle::pose_at_with_ref(snapshot, v, gps_refs[i], t),
                    _ => None,
                })
                .collect()
        };

        let vehicle_count = self.services.vehicles.len();
        if vehicle_count == 0 {
            pane.tracked_vehicle = None;
        } else {
            let fallback = first_visible_vehicle(&poses).unwrap_or(0);
            let tracked = pane
                .tracked_vehicle
                .filter(|&i| i < vehicle_count)
                .unwrap_or(fallback);
            pane.tracked_vehicle = Some(tracked);
        }

        // If the tracked vehicle has no pose, fall back to the first visible
        // pose, then origin.
        let tracked = pane
            .tracked_vehicle
            .and_then(|i| poses.get(i).and_then(|p| p.as_ref()))
            .or_else(|| poses.iter().flatten().next())
            .map(|p| p.pos);
        pane.camera.target = tracked.unwrap_or(glam::Vec3::ZERO);

        let tracked_reference = pane
            .tracked_vehicle
            .and_then(|i| self.services.vehicles.get(i).map(|v| (i, v)))
            .and_then(|(i, v)| vehicle::geodetic_reference(snapshot, v).map(|r| (i, r)));
        let provider_id = self.services.scene3d.map_provider;
        let ready_tiles =
            if let (Some(manager), Some((tracked_index, anchor)), Some(map_provider)) = (
                self.services.tile_manager.as_deref_mut(),
                tracked_reference,
                provider(provider_id),
            ) {
                let selection = (tracked_index, provider_id, anchor.map(f64::to_bits));
                pane.update_map_selection(Some(selection));
                let ppp = ui.ctx().pixels_per_point();
                let viewport = [
                    (rect.width() * ppp).round().max(1.0) as u32,
                    (rect.height() * ppp).round().max(1.0) as u32,
                ];
                let aspect = viewport[0] as f32 / viewport[1] as f32;
                let (_, inverse_relative) = pane
                    .camera
                    .view_proj_and_inverse(aspect, self.services.scene3d.resolved_far_clip_m());
                let inverse = glam::DMat4::from_translation(pane.camera.eye().as_dvec3())
                    * inverse_relative.as_dmat4();
                let visible = mercator::visible_tiles(
                    inverse,
                    viewport,
                    [anchor[0], anchor[1]],
                    map_provider.zoom_range(),
                );
                let requested_zoom = visible.tiles.first().map(|id| id.zoom);
                if requested_zoom != pane.map_zoom {
                    pane.map_previous_zoom = pane.map_zoom;
                    pane.map_previous_tiles = std::mem::take(&mut pane.map_tiles)
                        .into_iter()
                        .map(|(id, _)| id)
                        .collect();
                    pane.map_zoom = requested_zoom;
                }
                pane.map_tiles = visible
                    .tiles
                    .into_iter()
                    .enumerate()
                    .map(|(priority, id)| (id, priority as i32))
                    .collect();
                for (id, priority) in pane.map_tiles.iter().copied() {
                    manager.request(TileRequest {
                        scope: pane.map_scope,
                        provider: provider_id,
                        id,
                        corners: mercator::tile_corners_render(id, anchor),
                        priority,
                        generation: pane.map_generation,
                    });
                }
                manager.poll(pane.map_scope)
            } else {
                pane.update_map_selection(None);
                pane.map_zoom = None;
                pane.map_previous_zoom = None;
                pane.map_tiles.clear();
                pane.map_previous_tiles.clear();
                Vec::new()
            };

        let draws: Vec<VehicleDraw> = self
            .services
            .vehicles
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let pose = poses[i]?;
                let traj = self.services.trajectories.get(i);
                let points: &[[f32; 3]] = traj.map_or(&[], |t| t.points.as_slice());
                // Clip to the prefix of points at or before the playhead. The
                // full path stays resident on the GPU, so toggling never
                // re-uploads.
                let visible_count = match (traj, playhead) {
                    (Some(t), Some(ph)) if trail_to_playhead => {
                        t.times_us.partition_point(|&ts| ts <= ph) as u32
                    }
                    _ => points.len() as u32,
                };
                Some(VehicleDraw {
                    key: i as u32,
                    model: &v.model,
                    model_matrix: pose.model_matrix(v.scale).to_cols_array_2d(),
                    normal_matrix: glam::Mat4::from_mat3(pose.rot).to_cols_array_2d(),
                    color: legend::color32_to_srgb(v.color),
                    path_color: legend::color32_to_srgb(v.path_color),
                    trajectory: points,
                    traj_generation: self.services.traj_generation,
                    visible_count,
                })
            })
            .collect();

        let rendered = {
            let _t = self.services.metrics.scope("3d_frame");
            self.services.gpu.render_scene(
                self.services.frame,
                ui,
                rect,
                &pane.camera,
                self.services.scene3d,
                gpu::MapTileSelection {
                    scope: pane.map_scope,
                    epoch: self
                        .services
                        .tile_manager
                        .as_deref()
                        .map_or(0, |manager| manager.status().epoch),
                    provider: provider_id,
                    generation: pane.map_generation,
                    current_tiles: pane.map_tiles.clone(),
                    previous_tiles: pane.map_previous_tiles.clone(),
                    enabled: pane.map_selection.is_some(),
                },
                &ready_tiles,
                &draws,
            )
        };
        if let Some(tex) = rendered {
            ui.painter().image(
                tex,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        } else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "3D view unavailable",
                egui::FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
        }

        if vehicle_count >= 2 {
            tracked_vehicle_picker(ui, rect, pane, self.services.vehicles);
        }

        if provider_id != MapProviderId::None {
            let map_status = self
                .services
                .tile_manager
                .as_deref()
                .map(TileManager::status);
            let message = scene_map_overlay(
                tracked_reference.is_some(),
                map_status
                    .as_ref()
                    .and_then(|status| status.failure.as_ref().map(|failure| failure.class)),
                map_status.as_ref().is_some_and(|status| status.ready > 0),
            );
            scene_map_status(ui, rect, message);
            if let Some(map_provider) = provider(provider_id) {
                let (label, url) = map_provider.attribution();
                egui::Area::new(ui.make_persistent_id("scene-map-attribution"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(rect.left_bottom() + egui::vec2(8.0, -26.0))
                    .show(ui.ctx(), |ui| {
                        ui.small("Imagery © ");
                        ui.hyperlink_to(label, url);
                    });
            }
        }

        let overlay = scene_overlay_buttons(ui, rect, pane.trail_to_playhead, self.services.accent);
        if overlay.vehicle_config {
            self.actions.open_vehicle_config = true;
        }
        if overlay.toggle_trail {
            pane.trail_to_playhead = !pane.trail_to_playhead;
        }
        if overlay.export_kml {
            self.actions.export_kml = true;
        }

        if response.drag_started_by(egui::PointerButton::Middle) {
            egui_tiles::UiResponse::DragStarted
        } else {
            egui_tiles::UiResponse::None
        }
    }

    fn plot_ui(
        &mut self,
        ui: &mut egui::Ui,
        tile_id: egui_tiles::TileId,
        pane: &mut PlotPane,
    ) -> egui_tiles::UiResponse {
        let frame_style = egui::Frame::default();
        let mut tile_response = egui_tiles::UiResponse::None;
        let (response, dropped) = ui.dnd_drop_zone::<Vec<FieldId>, ()>(frame_style, |ui| {
            tile_response = self.plot_body(ui, tile_id, pane);
        });

        if let Some(fields) = dropped {
            let pointer = response.response.ctx.input(|i| i.pointer.interact_pos());
            if let Some(edge) =
                pointer.and_then(|pos| DropEdge::from_pos(response.response.rect, pos))
            {
                self.actions.edge_drop = Some((tile_id, edge, (*fields).clone()));
            } else {
                for &field in fields.iter() {
                    if pane.add_trace(field) {
                        self.services.caches.request(field, self.services.snapshot);
                    }
                }
            }
        }

        tile_response
    }

    fn plot_body(
        &mut self,
        ui: &mut egui::Ui,
        tile_id: egui_tiles::TileId,
        pane: &mut PlotPane,
    ) -> egui_tiles::UiResponse {
        let _pane_total = self.services.metrics.scope("pane_total");
        let outer = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(outer, egui::Sense::click_and_drag());
        if response.clicked() || response.drag_started() || response.secondary_clicked() {
            self.actions.focus = Some(tile_id);
        }
        let tile_response = if response.drag_started_by(egui::PointerButton::Middle) {
            egui_tiles::UiResponse::DragStarted
        } else {
            egui_tiles::UiResponse::None
        };
        // Use the widest gutter any pane needed last frame, but never below this
        // pane's own need so labels never clip.
        let shared_gutter = self.services.shared_y_gutter;
        let make_plot_rect =
            |ui: &egui::Ui, y_range: (f32, f32), y_unit: Option<&str>| -> (egui::Rect, f32) {
                let plot_height = (outer.height() - axes::X_GUTTER).max(1.0);
                let own_gutter = axes::y_gutter(ui, y_range, y_unit, plot_height);
                let gutter = shared_gutter.max(own_gutter);
                let rect = egui::Rect::from_min_max(
                    egui::pos2(outer.left() + gutter, outer.top() + 4.0),
                    egui::pos2(outer.right() - 4.0, outer.bottom() - axes::X_GUTTER),
                );
                (rect, own_gutter)
            };

        if pane.is_empty() {
            // Draw an empty frame reusing the shared X range so the pane pans
            // and zooms with the rest; neutral 0..1 fallback otherwise.
            let y_range = (0.0, 1.0);
            let (plot_rect, own_gutter) = make_plot_rect(ui, y_range, None);
            self.actions.max_y_gutter = self.actions.max_y_gutter.max(own_gutter);
            self.handle_plot_interaction(&response, plot_rect);
            self.handle_zoom_drag(&response, plot_rect, pane);
            if plot_rect.width() > 8.0 {
                let x_range = (*self.services.view)
                    .map(|v| v.seconds(self.services.origin_us))
                    .unwrap_or((0.0, 1.0));
                axes::draw(ui, plot_rect, x_range, y_range, None);
            }
            self.plot_context_menu(tile_id, &response, pane);
            self.plot_info_window(ui, tile_id, pane, None);
            return tile_response;
        }

        let Some(view) = *self.services.view else {
            let (plot_rect, own_gutter) = make_plot_rect(ui, (0.0, 1.0), None);
            self.actions.max_y_gutter = self.actions.max_y_gutter.max(own_gutter);
            self.handle_plot_interaction(&response, plot_rect);
            self.handle_zoom_drag(&response, plot_rect, pane);
            self.plot_context_menu(tile_id, &response, pane);
            self.plot_info_window(ui, tile_id, pane, None);
            return tile_response;
        };
        let pane_setup_timer = self.services.metrics.scope("pane_setup");
        let mut x_range = view.seconds(self.services.origin_us);
        let y_start = Instant::now();
        let mut y_range = gpu::visible_y_range(self.services.caches, pane, x_range.0, x_range.1);
        let mut y_query_us = y_start.elapsed().as_secs_f32() * 1_000_000.0;
        let y_unit = y_unit(self.services.snapshot.as_ref(), pane);
        let (mut plot_rect, own_gutter) = make_plot_rect(ui, y_range, y_unit.as_deref());
        self.actions.max_y_gutter = self.actions.max_y_gutter.max(own_gutter);
        let view_before_interaction = *self.services.view;
        // Marker drag takes priority over panning so a grab near the marker
        // moves it instead of scrolling the view.
        let marker_active = self.handle_marker_drag(&response, plot_rect, x_range, pane);
        if !marker_active {
            self.handle_plot_interaction(&response, plot_rect);
        }
        self.handle_zoom_drag(&response, plot_rect, pane);
        // Ctrl+hover scrubs an existing marker to the cursor, with no precise
        // grab on the line needed.
        if self.marker_us(pane).is_some()
            && ui.input(|i| i.modifiers.ctrl)
            && let Some(pos) = response.hover_pos()
            && plot_rect.contains(pos)
        {
            let frac = ((pos.x - plot_rect.left()) / plot_rect.width().max(1.0)).clamp(0.0, 1.0);
            let t_sec = x_range.0 as f64 + frac as f64 * (x_range.1 - x_range.0) as f64;
            let mut t_us = self.services.origin_us + (t_sec * 1e6).round() as i64;
            if let Some(range) = self.services.snapshot.global_time_range() {
                t_us = t_us.clamp(range.min_us, range.max_us);
            }
            self.set_marker_us(pane, Some(t_us));
        }
        if *self.services.view != view_before_interaction
            && let Some(view) = *self.services.view
        {
            x_range = view.seconds(self.services.origin_us);
            let y_start = Instant::now();
            y_range = gpu::visible_y_range(self.services.caches, pane, x_range.0, x_range.1);
            y_query_us += y_start.elapsed().as_secs_f32() * 1_000_000.0;
            let (rect, own_gutter) = make_plot_rect(ui, y_range, y_unit.as_deref());
            plot_rect = rect;
            self.actions.max_y_gutter = self.actions.max_y_gutter.max(own_gutter);
        }
        drop(pane_setup_timer);

        if !self.services.gpu.is_available() || plot_rect.width() <= 8.0 {
            self.plot_context_menu(tile_id, &response, pane);
            self.plot_info_window(ui, tile_id, pane, None);
            return tile_response;
        }

        let pane_axes_timer = self.services.metrics.scope("pane_axes");
        axes::draw(ui, plot_rect, x_range, y_range, y_unit.as_deref());
        drop(pane_axes_timer);
        let pview = PaneView {
            rect: plot_rect,
            x_range,
            y_range,
        };
        // Inter-marker region shading, painted behind the traces so it reads as
        // a background band. The last region stops at the
        // log's final timestamp rather than the pane edge.
        if self.services.plot_display.marker_shade_regions
            && let Some(range) = self.services.snapshot.global_time_range()
        {
            hover::draw_marker_regions(
                ui,
                pview,
                self.services.origin_us,
                self.services.markers,
                range.max_us,
                self.services.plot_display.marker_shade_opacity,
            );
        }
        let paint_start = Instant::now();
        self.services.gpu.render_pane(
            ui,
            self.services.frame,
            self.services.caches,
            pane,
            pview,
            self.services.render_tuning,
            self.services.metrics,
        );
        let paint_us = paint_start.elapsed().as_secs_f32() * 1_000_000.0;
        // Timers (ms): auto-Y range query and the CPU paint/encode prep.
        self.services.metrics.record("yquery", y_query_us / 1_000.0);
        self.services
            .metrics
            .record("plot_paint_cpu", paint_us / 1_000.0);
        let debug = PlotDebug {
            plot_rect,
            x_range,
            y_range,
            y_query_us,
            paint_us,
        };

        let pane_overlay_timer = self.services.metrics.scope("pane_overlay");
        if let Some(anchor_us) = pane.zoom_drag_anchor_us
            && let Some(p) = response.interact_pointer_pos()
        {
            let anchor_x = zoom_drag_anchor_x(view, plot_rect, anchor_us)
                .clamp(plot_rect.left(), plot_rect.right());
            let cursor_x = p.x.clamp(plot_rect.left(), plot_rect.right());
            let (lo, hi) = (anchor_x.min(cursor_x), anchor_x.max(cursor_x));
            let painter = ui.painter();
            let shade = egui::Color32::from_black_alpha(120);
            painter.rect_filled(
                egui::Rect::from_min_max(plot_rect.left_top(), egui::pos2(lo, plot_rect.bottom())),
                0.0,
                shade,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(hi, plot_rect.top()), plot_rect.right_bottom()),
                0.0,
                shade,
            );
            let edge = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(160));
            painter.vline(lo, plot_rect.y_range(), edge);
            painter.vline(hi, plot_rect.y_range(), edge);
        }
        self.plot_context_menu(tile_id, &response, pane);

        // Measurement marker (delta cursor): a dashed second
        // vertical with a ΔT readout vs the playhead. The per-trace ΔY computed
        // here is routed to either the legend or the value readout per the
        // `marker_delta_readout` setting. Both need a playhead to reference.
        let marker_deltas = match (self.marker_us(pane), self.services.playhead_us) {
            (Some(marker_us), Some(playhead)) => {
                hover::draw_marker(ui, pview, self.services.origin_us, marker_us, playhead);
                hover::marker_deltas(
                    self.services.snapshot.as_ref(),
                    pane,
                    marker_us,
                    playhead,
                    *self.services.hover_mode,
                )
            }
            _ => std::collections::HashMap::new(),
        };
        let no_deltas = std::collections::HashMap::new();
        let (legend_deltas, readout_deltas) = match self.services.plot_display.marker_delta_readout
        {
            crate::settings::MarkerDeltaReadout::Legend => (&marker_deltas, &no_deltas),
            crate::settings::MarkerDeltaReadout::Hover => (&no_deltas, &marker_deltas),
        };

        hover::draw_session_markers(
            ui,
            pview,
            self.services.origin_us,
            self.services.markers,
            self.services.plot_display.marker_line_opacity,
            self.services.plot_display.marker_line_width,
            self.services.plot_display.marker_show_label,
        );

        // String fields drawn as labels at each sample's timestamp.
        crate::text_overlay::draw(
            ui,
            &response,
            pview,
            self.services.origin_us,
            self.services.snapshot.as_ref(),
            &pane.traces,
            &mut pane.text_offsets,
            &pane.text_filters,
            crate::text_overlay::TextLabelStyle {
                cap: self.services.plot_display.text_label_cap,
                bottom_up: self.services.plot_display.text_labels_bottom_up,
                spacing_px: self.services.plot_display.text_label_spacing,
                line_width: self.services.plot_display.text_line_width,
                line_opacity: self.services.plot_display.text_line_opacity,
            },
        );

        // Playhead cursor + value readout on every pane. During
        // playback the hover tooltip is suppressed, so every pane (including the
        // hovered one) shows the playhead readout. While alt-scrubbing the
        // hovered pane keeps its hover tooltip and only the others read out.
        if let Some(t_us) = self.services.playhead_us {
            let hovered = response
                .hover_pos()
                .is_some_and(|pos| plot_rect.contains(pos));
            let alt = ui.input(|i| i.modifiers.alt);
            let readout =
                (self.services.playing || (alt && !hovered)).then_some(*self.services.hover_mode);
            hover::draw_playhead(
                ui,
                HoverTarget {
                    id: egui::Id::new(("playhead", tile_id)),
                    view: pview,
                },
                self.services.snapshot.as_ref(),
                pane,
                self.services.origin_us,
                t_us,
                readout,
                readout_deltas,
                self.services.plot_display.hover_show_field_name,
                self.services.plot_display.hover_show_time,
                self.services.plot_display.hover_opacity,
            );
        }

        if pane.show_tooltip && !ui.ctx().any_popup_open() {
            // Alt+hover drags the playhead along with the cursor. With
            // snap enabled it lands on the nearest data point instead, so the
            // playhead holds a sample until the cursor crosses to the next one.
            if ui.input(|i| i.modifiers.alt)
                && let Some(pos) = response.hover_pos()
                && plot_rect.contains(pos)
            {
                let frac = (pos.x - plot_rect.left()) / plot_rect.width().max(1.0);
                let t_sec = x_range.0 as f64 + frac as f64 * (x_range.1 - x_range.0) as f64;
                let cursor_us = self.services.origin_us + (t_sec * 1e6).round() as i64;
                let target = if *self.services.snap_playhead {
                    hover::nearest_sample_us(self.services.snapshot.as_ref(), pane, cursor_us)
                        .unwrap_or(cursor_us)
                } else {
                    cursor_us
                };
                self.actions.scrub_to = Some(target);
            }

            hover::draw(
                ui,
                HoverTarget {
                    id: egui::Id::new(("plot_hover", tile_id)),
                    view: pview,
                },
                &response,
                self.services.snapshot.as_ref(),
                pane,
                self.services.origin_us,
                *self.services.hover_mode,
                !self.services.playing,
                readout_deltas,
                self.services.plot_display.hover_show_field_name,
                self.services.plot_display.hover_show_time,
                self.services.plot_display.hover_opacity,
            );
        }

        if pane.show_legend {
            let labels: Vec<_> = pane
                .traces
                .iter()
                .map(|t| {
                    (
                        t.field,
                        legend::trace_label(self.services.snapshot.as_ref(), t.field),
                    )
                })
                .collect();
            if let Some(removed) = legend::ui(
                ui,
                egui::Id::new(("plot_legend", tile_id)),
                plot_rect,
                self.services.plot_display.legend_position,
                self.services.plot_display.legend_opacity,
                pane,
                &labels,
                legend_deltas,
                self.services.snapshot.as_ref(),
            ) {
                pane.remove_trace(removed);
                self.services.caches.unpin(removed);
                self.actions.remove_trace.push(removed);
            }
        }

        self.plot_info_window(ui, tile_id, pane, Some(debug));
        drop(pane_overlay_timer);
        tile_response
    }

    fn plot_context_menu(
        &mut self,
        tile_id: egui_tiles::TileId,
        response: &egui::Response,
        pane: &mut PlotPane,
    ) {
        response.context_menu(|ui| {
            if ui
                .add(egui::Button::image_and_text(
                    menu_icon(ui, crate::icons::trash()),
                    "Clear all traces",
                ))
                .clicked()
            {
                for field in pane.fields().collect::<Vec<_>>() {
                    self.services.caches.unpin(field);
                    self.actions.remove_trace.push(field);
                }
                pane.clear();
                ui.close();
            }

            ui.menu_image_text_button(menu_icon(ui, crate::icons::ban()), "Remove trace", |ui| {
                let entries: Vec<_> = pane
                    .traces
                    .iter()
                    .map(|t| {
                        (
                            t.field,
                            legend::trace_label(self.services.snapshot.as_ref(), t.field),
                            t.color32(),
                        )
                    })
                    .collect();
                if entries.is_empty() {
                    ui.add_enabled(false, egui::Button::new("No traces"));
                }
                for (field, label, color) in entries {
                    let clicked = ui
                        .horizontal(|ui| {
                            color_swatch(ui, color);
                            ui.button(label).clicked()
                        })
                        .inner;
                    if clicked {
                        pane.remove_trace(field);
                        self.services.caches.unpin(field);
                        self.actions.remove_trace.push(field);
                        ui.close();
                    }
                }
            });

            ui.menu_image_text_button(menu_icon(ui, crate::icons::info()), "Field stats", |ui| {
                let entries: Vec<_> = pane
                    .traces
                    .iter()
                    .map(|t| {
                        (
                            t.field,
                            legend::trace_label(self.services.snapshot.as_ref(), t.field),
                            t.color32(),
                        )
                    })
                    .collect();
                if entries.is_empty() {
                    ui.add_enabled(false, egui::Button::new("No traces"));
                }
                for (field, label, color) in entries {
                    let clicked = ui
                        .horizontal(|ui| {
                            color_swatch(ui, color);
                            ui.button(label).clicked()
                        })
                        .inner;
                    if clicked {
                        self.actions.inspect_field_stats = Some(field);
                        ui.close();
                    }
                }
            });

            ui.menu_image_text_button(menu_icon(ui, crate::icons::pencil()), "Edit trace", |ui| {
                let entries: Vec<_> = pane
                    .traces
                    .iter()
                    .map(|t| {
                        (
                            t.field,
                            legend::trace_label(self.services.snapshot.as_ref(), t.field),
                            t.color32(),
                        )
                    })
                    .collect();
                if entries.is_empty() {
                    ui.add_enabled(false, egui::Button::new("No traces"));
                }
                for (field, label, color) in entries {
                    let Some(trace) = pane.trace_mut(field) else {
                        continue;
                    };
                    ui.menu_button(label, |ui| {
                        ui.horizontal(|ui| {
                            let mut color = color;
                            if egui::color_picker::color_edit_button_srgba(
                                ui,
                                &mut color,
                                egui::color_picker::Alpha::Opaque,
                            )
                            .changed()
                            {
                                trace.color = legend::color32_to_srgb(color);
                            }
                            ui.weak("Color / mode");
                        });
                        for mode in TraceMode::ALL {
                            ui.radio_value(&mut trace.mode, mode, mode.label());
                        }
                        ui.add(
                            egui::Slider::new(&mut trace.width_px, 1.0..=12.0)
                                .text("Width")
                                .suffix(" px"),
                        );
                    });
                }
            });

            ui.separator();

            if ui
                .add(egui::Button::image_and_text(
                    menu_icon(ui, crate::icons::copy()),
                    "Copy Image",
                ))
                .clicked()
            {
                self.actions.image = Some(WorkspaceImageAction::CopyPlot {
                    rect: response.rect,
                });
                ui.close();
            }
            if ui
                .add(egui::Button::image_and_text(
                    menu_icon(ui, crate::icons::export()),
                    "Export PNG...",
                ))
                .clicked()
            {
                self.actions.image = Some(WorkspaceImageAction::ExportPlot {
                    rect: response.rect,
                });
                ui.close();
            }

            ui.separator();

            if ui
                .add(egui::Button::image_and_text(
                    menu_icon(ui, crate::icons::columns()),
                    "Split horizontally",
                ))
                .clicked()
            {
                self.actions.split = Some((tile_id, SplitDirection::Horizontal));
                ui.close();
            }
            if ui
                .add(egui::Button::image_and_text(
                    menu_icon(ui, crate::icons::rows()),
                    "Split vertically",
                ))
                .clicked()
            {
                self.actions.split = Some((tile_id, SplitDirection::Vertical));
                ui.close();
            }

            ui.separator();

            ui.checkbox(&mut pane.show_tooltip, "Show tooltip");

            if ui
                .add(egui::Button::image_and_text(
                    menu_icon(ui, crate::icons::info()),
                    "Plot Info",
                ))
                .clicked()
            {
                pane.show_info = true;
                ui.close();
            }

            ui.separator();

            if ui
                .add(egui::Button::image_and_text(
                    menu_icon(ui, crate::icons::close()),
                    "Close",
                ))
                .clicked()
            {
                self.actions.close = Some(tile_id);
                ui.close();
            }
        });
    }

    /// Open state lives on the pane so it survives across frames.
    fn plot_info_window(
        &mut self,
        ui: &egui::Ui,
        tile_id: egui_tiles::TileId,
        pane: &mut PlotPane,
        debug: Option<PlotDebug>,
    ) {
        if !pane.show_info {
            return;
        }
        let mut open = pane.show_info;
        egui::Window::new("Plot Info")
            .id(egui::Id::new(("plot-info", tile_id)))
            .open(&mut open)
            .collapsible(false)
            .default_pos(ui.ctx().content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .resizable(true)
            .default_width(320.0)
            .show(ui.ctx(), |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.debug_ui(ui, pane, debug);
                });
            });
        pane.show_info = open;
    }

    fn debug_ui(&mut self, ui: &mut egui::Ui, pane: &PlotPane, debug: Option<PlotDebug>) {
        ui.label(format!("traces: {}", pane.traces.len()));
        ui.label(format!("ghost traces: {}", pane.ghosts.len()));
        ui.label(format!("visible traces: {}", pane.visible_traces().count()));

        if let Some(debug) = debug {
            ui.separator();
            ui.label(format!(
                "plot rect: {:.0} x {:.0} px",
                debug.plot_rect.width(),
                debug.plot_rect.height()
            ));
            ui.label(format!(
                "visible x: {:.3} .. {:.3} s",
                debug.x_range.0, debug.x_range.1
            ));
            ui.label(format!(
                "visible y: {:.4} .. {:.4}",
                debug.y_range.0, debug.y_range.1
            ));
            ui.label(format!("yquery: {:.1} us", debug.y_query_us));
            ui.label(format!("paint encode: {:.1} us", debug.paint_us));
        }

        ui.separator();
        for trace in &pane.traces {
            let label = legend::trace_label(self.services.snapshot.as_ref(), trace.field);
            ui.collapsing(label, |ui| {
                ui.label(format!("field id: {}", trace.field.0));
                ui.label(format!("mode: {}", trace.mode.label()));
                ui.label(format!("width: {:.1} px", trace.width_px));
                ui.label(format!("visible: {}", trace.visible));

                let cache_status = if self.services.caches.is_ready(trace.field) {
                    "ready"
                } else if self.services.caches.is_building(trace.field) {
                    "building"
                } else {
                    "missing"
                };
                ui.label(format!("cache: {cache_status}"));
                ui.label(format!(
                    "cache cpu: {}",
                    format_bytes(self.services.caches.field_mem(trace.field).cache_cpu)
                ));
                ui.label(format!(
                    "gpu: {}",
                    format_bytes(
                        self.services
                            .gpu
                            .field_gpu_bytes(self.services.frame, trace.field)
                    )
                ));

                if let Some(cache) = self.services.caches.get(trace.field) {
                    ui.label(format!("samples: {}", cache.samples()));
                    if let Some(debug) = debug {
                        let (a, b) = cache.index_range(debug.x_range.0, debug.x_range.1);
                        ui.label(format!("visible samples: {}", b.saturating_sub(a)));
                    }
                }
            });
        }
    }

    fn handle_plot_interaction(&mut self, response: &egui::Response, rect: egui::Rect) {
        let Some(mut view) = *self.services.view else {
            return;
        };

        if response.double_clicked() {
            if let Some(range) = self.services.snapshot.global_time_range() {
                *self.services.view = Some(ViewX::from_range(range));
                self.actions.view_changed = true;
            }
            return;
        }

        let mut changed = false;
        if response.dragged_by(egui::PointerButton::Primary) {
            gpu::apply_pan(&mut view, response.drag_delta().x, rect.width());
            changed = true;
        }

        if response.hovered() {
            let scroll = response.ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let cursor_frac = response
                    .hover_pos()
                    .map(|p| (p.x - rect.left()) / rect.width().max(1.0))
                    .unwrap_or(0.5);
                gpu::apply_zoom(&mut view, cursor_frac, scroll);
                changed = true;
            }
        }

        if changed {
            self.actions.view_changed = true;
        }
        *self.services.view = Some(view);
    }

    /// The effective measurement marker time is always shared across plot panes.
    fn marker_us(&self, _pane: &PlotPane) -> Option<i64> {
        *self.services.marker_us
    }

    /// Set or clear the shared measurement marker.
    fn set_marker_us(&mut self, _pane: &mut PlotPane, value: Option<i64>) {
        *self.services.marker_us = value;
    }

    /// Drag the measurement marker line along X. A primary drag that starts
    /// within a few pixels of the marker grabs it. Returns whether the drag was
    /// consumed, so the caller skips panning.
    fn handle_marker_drag(
        &mut self,
        response: &egui::Response,
        rect: egui::Rect,
        x_range: (f32, f32),
        pane: &mut PlotPane,
    ) -> bool {
        let Some(marker_us) = self.marker_us(pane) else {
            return false;
        };
        let (x0, x1) = x_range;
        if x1 <= x0 || rect.width() <= 0.0 {
            return false;
        }
        let origin = self.services.origin_us;
        let marker_sec = ((marker_us - origin) as f64 * 1e-6) as f32;
        let marker_x = rect.left() + (marker_sec - x0) / (x1 - x0) * rect.width();

        if response.drag_started_by(egui::PointerButton::Primary) {
            pane.marker_drag = response
                .interact_pointer_pos()
                .is_some_and(|p| rect.contains(p) && (p.x - marker_x).abs() <= 6.0);
        }
        if response.drag_stopped() {
            let was = pane.marker_drag;
            pane.marker_drag = false;
            if was {
                return true; // consume the release frame so it never pans
            }
        }
        if pane.marker_drag {
            if let Some(p) = response.interact_pointer_pos() {
                let frac = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                let t_sec = x0 as f64 + frac as f64 * (x1 - x0) as f64;
                let mut t_us = origin + (t_sec * 1e6).round() as i64;
                if let Some(range) = self.services.snapshot.global_time_range() {
                    t_us = t_us.clamp(range.min_us, range.max_us);
                }
                self.set_marker_us(pane, Some(t_us));
            }
            return true;
        }
        false
    }

    /// Right-button drag zooms the shared X view to the dragged window. The
    /// zoom applies on release; a plain right-click never sets the anchor and so
    /// still opens the context menu.
    fn handle_zoom_drag(
        &mut self,
        response: &egui::Response,
        rect: egui::Rect,
        pane: &mut PlotPane,
    ) {
        let Some(view) = *self.services.view else {
            return;
        };
        if response.drag_started_by(egui::PointerButton::Secondary)
            && let Some(p) = response.interact_pointer_pos()
            && rect.contains(p)
        {
            let frac = ((p.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0) as f64;
            pane.zoom_drag_anchor_us = Some(view.min_us + (frac * view.span_us() as f64) as i64);
        }
        if response.drag_stopped_by(egui::PointerButton::Secondary)
            && let Some(anchor_us) = pane.zoom_drag_anchor_us.take()
            && let Some(p) = response.interact_pointer_pos()
        {
            let anchor_x = zoom_drag_anchor_x(view, rect, anchor_us);
            if let Some(new_view) =
                gpu::zoom_drag_view(view, rect.left(), rect.width(), anchor_x, p.x)
            {
                *self.services.view = Some(new_view);
                self.actions.view_changed = true;
            }
        }
    }
}

fn scene_map_overlay(
    reference_available: bool,
    failure: Option<TileFailureClass>,
    cached: bool,
) -> Option<&'static str> {
    if !reference_available {
        Some("Map unavailable: no georeference")
    } else if failure == Some(TileFailureClass::Cache) {
        Some("Map cache error")
    } else if failure == Some(TileFailureClass::NetworkTransient) && cached {
        Some("Map tiles offline — showing cached imagery")
    } else {
        None
    }
}

fn scene_map_status(ui: &egui::Ui, rect: egui::Rect, message: Option<&str>) {
    if let Some(message) = message {
        ui.painter().text(
            rect.center_bottom() - egui::vec2(0.0, 12.0),
            egui::Align2::CENTER_BOTTOM,
            message,
            egui::FontId::proportional(13.0),
            ui.visuals().warn_fg_color,
        );
    }
}

fn apply_ghost_text_state(pane: &mut PlotPane, ghost: &GhostTrace, field: FieldId) {
    if let Some(filter) = ghost.text_filter.as_ref() {
        pane.text_filters.insert(field, filter.clone());
    }
    for &(time_us, offset) in &ghost.text_offsets {
        pane.text_offsets.insert((field, time_us), offset);
    }
}

fn rebind_text_state(pane: &mut PlotPane, old_field: FieldId, new_field: FieldId) {
    if let Some(filter) = pane.text_filters.remove(&old_field) {
        pane.text_filters.insert(new_field, filter);
    }

    let offsets: Vec<_> = pane
        .text_offsets
        .iter()
        .filter_map(|(&(field, time_us), &offset)| {
            (field == old_field).then_some((time_us, offset))
        })
        .collect();
    for (time_us, offset) in offsets {
        pane.text_offsets.remove(&(old_field, time_us));
        pane.text_offsets.insert((new_field, time_us), offset);
    }
}

fn zoom_drag_anchor_x(view: ViewX, rect: egui::Rect, anchor_us: i64) -> f32 {
    let frac = (anchor_us - view.min_us) as f64 / view.span_us() as f64;
    rect.left() + frac as f32 * rect.width()
}

fn take_text_state(pane: &mut PlotPane, field: FieldId) -> (Option<String>, Vec<(i64, f32)>) {
    let filter = pane.text_filters.remove(&field);
    let offsets: Vec<_> = pane
        .text_offsets
        .iter()
        .filter_map(|(&(offset_field, time_us), &offset)| {
            (offset_field == field).then_some((time_us, offset))
        })
        .collect();
    for &(time_us, _) in &offsets {
        pane.text_offsets.remove(&(field, time_us));
    }
    (filter, offsets)
}

fn script_ghost_from_removed_trace(
    snapshot: &StoreSnapshot,
    pane: &mut PlotPane,
    trace_index: usize,
) -> Option<GhostTrace> {
    let trace = pane.traces.get(trace_index).copied()?;
    let field = snapshot
        .fields
        .get(trace.field.index())
        .filter(|field| field.id == trace.field && field.removed)?;
    let topic = snapshot.topic(field.topic)?;
    let source = snapshot.source(topic.entry.source)?;
    if !source.entry.removed || !source.entry.label.starts_with("script:") {
        return None;
    }
    let (text_filter, text_offsets) = take_text_state(pane, trace.field);
    Some(GhostTrace {
        source: Some(source.entry.label.clone()),
        topic: topic.entry.name.clone(),
        field: field.name.clone(),
        color: trace.color,
        width_px: trace.width_px,
        mode: trace.mode,
        visible: trace.visible,
        text_filter,
        text_offsets,
    })
}

fn resolve_ghost(snapshot: &StoreSnapshot, ghost: &GhostTrace) -> Option<FieldId> {
    match ghost.source.as_deref() {
        Some(source) => resolve_source_field(snapshot, source, &ghost.topic, &ghost.field),
        None => resolve_source_agnostic(snapshot, &ghost.topic, &ghost.field),
    }
}

fn resolve_source_agnostic(
    snapshot: &StoreSnapshot,
    topic_name: &str,
    field_name: &str,
) -> Option<FieldId> {
    let mut found = None;
    for source in snapshot.sources.iter().filter(|s| !s.entry.removed) {
        for topic_id in source.topics.iter().copied() {
            let topic = snapshot.topic(topic_id)?;
            if topic.entry.removed || topic.entry.name != topic_name {
                continue;
            }
            for field in snapshot
                .fields
                .iter()
                .filter(|f| f.topic == topic_id && !f.removed && f.name == field_name)
            {
                if found.is_some() {
                    return None;
                }
                found = Some(field.id);
            }
        }
    }
    found
}

fn resolve_source_field(
    snapshot: &StoreSnapshot,
    source_label: &str,
    topic_name: &str,
    field_name: &str,
) -> Option<FieldId> {
    for source in snapshot
        .sources
        .iter()
        .filter(|source| !source.entry.removed && source.entry.label == source_label)
    {
        for topic_id in source.topics.iter().copied() {
            let topic = snapshot.topic(topic_id)?;
            if topic.entry.removed || topic.entry.name != topic_name {
                continue;
            }
            for field in snapshot
                .fields
                .iter()
                .filter(|f| f.topic == topic_id && !f.removed && f.name == field_name)
            {
                return Some(field.id);
            }
        }
    }
    None
}

fn resolve_recreated_script_field(snapshot: &StoreSnapshot, old_field: FieldId) -> Option<FieldId> {
    let field = snapshot
        .fields
        .get(old_field.index())
        .filter(|field| field.id == old_field && field.removed)?;
    let topic = snapshot.topic(field.topic)?;
    let source = snapshot.source(topic.entry.source)?;
    let source_label = source.entry.label.as_str();
    let topic_name = topic.entry.name.as_str();
    let field_name = field.name.as_str();
    if !source.entry.removed || !source_label.starts_with("script:") {
        return None;
    }
    resolve_source_field(snapshot, source_label, topic_name, field_name)
}

fn y_unit(snapshot: &StoreSnapshot, pane: &PlotPane) -> Option<String> {
    let field = pane.traces.first()?.field;
    let entry = snapshot
        .fields
        .get(field.index())
        .filter(|f| f.id == field)?;
    let store = snapshot.topic(entry.topic)?.store.as_ref()?;
    store.schema.field_by_name(&entry.name)?.unit.clone()
}

/// Insert `child` at `index` in any container kind (clamped to the child
/// count). `Container::add_child` only appends.
fn insert_child_at(container: &mut egui_tiles::Container, index: usize, child: egui_tiles::TileId) {
    match container {
        egui_tiles::Container::Linear(linear) => {
            let index = index.min(linear.children.len());
            linear.children.insert(index, child);
        }
        egui_tiles::Container::Tabs(tabs) => {
            let index = index.min(tabs.children.len());
            tabs.children.insert(index, child);
        }
        egui_tiles::Container::Grid(grid) => grid.insert_at(index, child),
    }
}

fn ordered_pair(
    existing: egui_tiles::TileId,
    new_pane: egui_tiles::TileId,
    before: bool,
) -> Vec<egui_tiles::TileId> {
    if before {
        vec![new_pane, existing]
    } else {
        vec![existing, new_pane]
    }
}

fn fields_from_removed_tile(tile: egui_tiles::Tile<Pane>) -> Vec<FieldId> {
    match tile {
        egui_tiles::Tile::Pane(Pane::Plot(pane)) => pane.fields().collect(),
        egui_tiles::Tile::Pane(Pane::Scene3D(_)) | egui_tiles::Tile::Container(_) => Vec::new(),
    }
}

fn first_visible_vehicle(poses: &[Option<vehicle::Pose>]) -> Option<usize> {
    poses.iter().position(Option::is_some)
}

fn tracked_vehicle_picker(
    ui: &mut egui::Ui,
    scene_rect: egui::Rect,
    pane: &mut Scene3dPane,
    vehicles: &[vehicle::VehicleConfig],
) {
    let id = ui.make_persistent_id("scene-tracked-vehicle");
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(scene_rect.min + egui::vec2(8.0, 8.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.weak("Track");
                    let selected = pane
                        .tracked_vehicle
                        .and_then(|i| vehicles.get(i).map(|v| v.label.as_str()))
                        .unwrap_or("Vehicle");
                    egui::ComboBox::from_id_salt("scene-tracked-vehicle-combo")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for (i, vehicle) in vehicles.iter().enumerate() {
                                let label = if vehicle.show {
                                    vehicle.label.clone()
                                } else {
                                    format!("{} (hidden)", vehicle.label)
                                };
                                ui.selectable_value(&mut pane.tracked_vehicle, Some(i), label);
                            }
                        });
                });
            });
        });
}

/// Which scene-overlay button was clicked this frame.
#[derive(Default)]
struct SceneOverlayClicks {
    vehicle_config: bool,
    toggle_trail: bool,
    export_kml: bool,
}

fn scene_overlay_buttons(
    ui: &mut egui::Ui,
    scene_rect: egui::Rect,
    trail_to_playhead: bool,
    accent: egui::Color32,
) -> SceneOverlayClicks {
    let id = ui.make_persistent_id("scene-overlay-buttons");
    let mut clicks = SceneOverlayClicks::default();
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(scene_rect.right_top() + egui::vec2(-36.0, 8.0))
        .show(ui.ctx(), |ui| {
            ui.vertical(|ui| {
                let gear = egui::Image::new(crate::icons::gear())
                    .fit_to_exact_size(egui::vec2(18.0, 18.0))
                    .tint(ui.visuals().weak_text_color());
                clicks.vehicle_config = ui
                    .add_sized(egui::vec2(28.0, 24.0), egui::Button::image(gear))
                    .clicked();

                // Route toggle: accent when the path is clipped to the playhead,
                // dimmed when the full path is shown (mirrors the toolbar's
                // active/inactive icon-tint convention).
                let route_tint = if trail_to_playhead {
                    accent
                } else {
                    ui.visuals().weak_text_color()
                };
                let route = egui::Image::new(crate::icons::route())
                    .fit_to_exact_size(egui::vec2(18.0, 18.0))
                    .tint(route_tint);
                clicks.toggle_trail = ui
                    .add_sized(egui::vec2(28.0, 24.0), egui::Button::image(route))
                    .clicked();

                let earth = egui::Image::new(crate::icons::earth())
                    .fit_to_exact_size(egui::vec2(18.0, 18.0))
                    .tint(ui.visuals().weak_text_color());
                clicks.export_kml = ui
                    .add_sized(egui::vec2(28.0, 24.0), egui::Button::image(earth))
                    .on_hover_text("Export trajectories to KML")
                    .clicked();
            });
        });
    clicks
}

/// A 16px menu icon tinted to the current text colour (the bundled SVGs are
/// authored white, so the tint multiply colours them).
fn menu_icon(ui: &egui::Ui, src: egui::ImageSource<'static>) -> egui::Image<'static> {
    egui::Image::new(src)
        .fit_to_exact_size(egui::vec2(16.0, 16.0))
        .tint(ui.visuals().text_color())
}

fn color_swatch(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color);
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.2} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_starts_with_one_plot_pane() {
        let workspace = Workspace::new();
        assert_eq!(workspace.plot_panes().count(), 1);
        assert!(workspace.fields().next().is_none());
    }

    #[test]
    fn workspace_image_actions_carry_plot_rects() {
        let rect = egui::Rect::from_min_size(egui::pos2(4.0, 8.0), egui::vec2(120.0, 80.0));

        let copy = WorkspaceImageAction::CopyPlot { rect };
        let export = WorkspaceImageAction::ExportPlot { rect };

        assert_eq!(copy.rect(), rect);
        assert_eq!(export.rect(), rect);
    }

    #[test]
    fn prune_removed_fields_drops_traces_for_removed_sources() {
        let mut identity = delog_core::identity::IdentityRegistry::new();
        let keep_source = identity.add_source("keep");
        let drop_source = identity.add_source("drop");
        let keep_topic = identity.add_topic(keep_source, "POS").unwrap();
        let drop_topic = identity.add_topic(drop_source, "POS").unwrap();
        let keep_field = identity.add_field(keep_topic, "Alt").unwrap();
        let drop_field = identity.add_field(drop_topic, "Alt").unwrap();
        identity.remove_source(drop_source);
        let snapshot = StoreSnapshot::from_registry(&identity, [], 1).unwrap();

        let mut workspace = Workspace::new();
        assert!(workspace.add_trace_to_first_plot(keep_field));
        assert!(workspace.add_trace_to_first_plot(drop_field));

        let removed = workspace.prune_removed_fields(&snapshot);

        assert_eq!(removed, vec![drop_field]);
        assert_eq!(workspace.fields().collect::<Vec<_>>(), vec![keep_field]);
    }

    #[test]
    fn prune_removed_fields_rebinds_script_traces_to_recreated_fields() {
        let mut identity = delog_core::identity::IdentityRegistry::new();
        let old_source = identity.add_source("script:calc");
        let old_topic = identity.add_topic(old_source, "Derived").unwrap();
        let old_field = identity.add_field(old_topic, "value").unwrap();
        identity.remove_source(old_source);
        let new_source = identity.add_source("script:calc");
        let new_topic = identity.add_topic(new_source, "Derived").unwrap();
        let new_field = identity.add_field(new_topic, "value").unwrap();
        let snapshot = StoreSnapshot::from_registry(&identity, [], 1).unwrap();

        let mut workspace = Workspace::new();
        assert!(workspace.add_trace_to_first_plot(old_field));
        {
            let pane = workspace.plot_panes_mut().next().unwrap();
            let trace = pane.trace_mut(old_field).unwrap();
            trace.color = [0.1, 0.2, 0.3, 0.4];
            trace.width_px = 3.0;
            trace.mode = TraceMode::Step;
            trace.visible = false;
            pane.text_filters.insert(old_field, "armed".into());
            pane.text_offsets.insert((old_field, 42), 0.75);
        }

        let removed = workspace.prune_removed_fields(&snapshot);

        assert!(removed.is_empty());
        assert_eq!(workspace.fields().collect::<Vec<_>>(), vec![new_field]);
        let pane = workspace.plot_panes().next().unwrap();
        assert_eq!(
            pane.traces[0],
            TraceRef {
                field: new_field,
                color: [0.1, 0.2, 0.3, 0.4],
                width_px: 3.0,
                mode: TraceMode::Step,
                visible: false,
            }
        );
        assert_eq!(pane.text_filters.get(&new_field).unwrap(), "armed");
        assert_eq!(pane.text_offsets.get(&(new_field, 42)), Some(&0.75));
        assert!(!pane.text_filters.contains_key(&old_field));
        assert!(
            !pane
                .text_offsets
                .keys()
                .any(|(field, _time)| *field == old_field)
        );
    }

    #[test]
    fn prune_removed_fields_keeps_script_trace_until_recreated_field_appears() {
        let mut identity = delog_core::identity::IdentityRegistry::new();
        let old_source = identity.add_source("script:calc");
        let old_topic = identity.add_topic(old_source, "Derived").unwrap();
        let old_field = identity.add_field(old_topic, "value").unwrap();

        let mut workspace = Workspace::new();
        assert!(workspace.add_trace_to_first_plot(old_field));
        {
            let pane = workspace.plot_panes_mut().next().unwrap();
            let trace = pane.trace_mut(old_field).unwrap();
            trace.color = [0.4, 0.3, 0.2, 0.1];
            trace.width_px = 4.0;
            trace.mode = TraceMode::Scatter;
            trace.visible = false;
            pane.text_filters.insert(old_field, "armed".into());
            pane.text_offsets.insert((old_field, 42), 0.75);
        }

        identity.remove_source(old_source);
        let removed_snapshot = StoreSnapshot::from_registry(&identity, [], 1).unwrap();
        let removed = workspace.prune_removed_fields(&removed_snapshot);

        assert_eq!(removed, vec![old_field]);
        assert!(workspace.fields().next().is_none());
        {
            let pane = workspace.plot_panes().next().unwrap();
            assert_eq!(pane.ghosts.len(), 1);
            assert_eq!(pane.ghosts[0].source.as_deref(), Some("script:calc"));
            assert_eq!(pane.ghosts[0].topic, "Derived");
            assert_eq!(pane.ghosts[0].field, "value");
            assert_eq!(pane.ghosts[0].color, [0.4, 0.3, 0.2, 0.1]);
            assert_eq!(pane.ghosts[0].width_px, 4.0);
            assert_eq!(pane.ghosts[0].mode, TraceMode::Scatter);
            assert!(!pane.ghosts[0].visible);
            assert_eq!(pane.ghosts[0].text_filter.as_deref(), Some("armed"));
            assert_eq!(pane.ghosts[0].text_offsets, vec![(42, 0.75)]);
        }

        let new_source = identity.add_source("script:calc");
        let new_topic = identity.add_topic(new_source, "Derived").unwrap();
        let new_field = identity.add_field(new_topic, "value").unwrap();
        let recreated_snapshot = StoreSnapshot::from_registry(&identity, [], 2).unwrap();

        assert_eq!(workspace.resolve_ghosts(&recreated_snapshot), 1);

        let pane = workspace.plot_panes().next().unwrap();
        assert!(pane.ghosts.is_empty());
        assert_eq!(pane.traces.len(), 1);
        assert_eq!(
            pane.traces[0],
            TraceRef {
                field: new_field,
                color: [0.4, 0.3, 0.2, 0.1],
                width_px: 4.0,
                mode: TraceMode::Scatter,
                visible: false,
            }
        );
        assert_eq!(pane.text_filters.get(&new_field).unwrap(), "armed");
        assert_eq!(pane.text_offsets.get(&(new_field, 42)), Some(&0.75));
    }

    #[test]
    fn scene_pane_toggles_a_single_instance_on_and_off() {
        fn scene_count(w: &Workspace) -> usize {
            w.tree
                .tiles
                .tiles()
                .filter(|t| matches!(t, egui_tiles::Tile::Pane(Pane::Scene3D(_))))
                .count()
        }

        let mut workspace = Workspace::new();
        assert!(workspace.scene_pane_id().is_none());

        workspace.toggle_scene_pane();
        let id = workspace.scene_pane_id().expect("scene pane should exist");
        assert_eq!(scene_count(&workspace), 1);
        assert_eq!(workspace.plot_panes().count(), 1);

        workspace.toggle_scene_pane();
        assert!(workspace.scene_pane_id().is_none());
        assert_eq!(workspace.plot_panes().count(), 1);

        // A fresh show reuses the single-instance path (never two) with a new id.
        workspace.toggle_scene_pane();
        assert_eq!(scene_count(&workspace), 1);
        assert_ne!(workspace.scene_pane_id(), Some(id));
    }

    #[test]
    fn scene_splits_at_root_not_inside_the_focused_pane() {
        let mut workspace = Workspace::new();
        let pane1 = workspace.tree.root().unwrap();
        workspace.split_plot(pane1, SplitDirection::Vertical);
        let inner = workspace.tree.root().unwrap();

        // Focus the nested plot: the buggy path split here instead of globally.
        workspace.focused = Some(pane1);
        workspace.toggle_scene_pane();

        let scene = workspace.scene_pane_id().expect("scene pane should exist");
        let root = workspace.tree.root().unwrap();
        assert_eq!(
            workspace.tree.tiles.parent_of(scene),
            Some(root),
            "scene must sit directly under the root, beside the whole layout",
        );
        let Some(egui_tiles::Tile::Container(root_container)) = workspace.tree.tiles.get(root)
        else {
            panic!("root should be a container wrapping the layout and the scene");
        };
        assert_eq!(root_container.num_children(), 2);
        // The previous layout stays intact as a single sibling of the scene.
        assert_eq!(workspace.tree.tiles.parent_of(pane1), Some(inner));
    }

    #[test]
    fn first_visible_vehicle_skips_missing_poses() {
        let poses = [
            None,
            Some(vehicle::Pose {
                pos: glam::Vec3::X,
                rot: glam::Mat3::IDENTITY,
            }),
            Some(vehicle::Pose {
                pos: glam::Vec3::Y,
                rot: glam::Mat3::IDENTITY,
            }),
        ];

        assert_eq!(first_visible_vehicle(&poses), Some(1));
        assert_eq!(first_visible_vehicle(&[None, None]), None);
    }

    #[test]
    fn scene_map_overlay_only_reports_actionable_states() {
        assert_eq!(
            scene_map_overlay(false, None, false),
            Some("Map unavailable: no georeference")
        );
        assert_eq!(
            scene_map_overlay(true, Some(TileFailureClass::Cache), true),
            Some("Map cache error")
        );
        assert_eq!(
            scene_map_overlay(true, Some(TileFailureClass::NetworkTransient), true),
            Some("Map tiles offline — showing cached imagery")
        );
        assert_eq!(scene_map_overlay(true, None, true), None);
    }

    #[test]
    fn scene_map_tracked_vehicle_switch_changes_generation() {
        let mut pane = Scene3dPane::default();
        let first = pane.update_map_selection(Some((0, MapProviderId::BingSatellite, [0; 3])));
        assert_eq!(
            pane.update_map_selection(Some((0, MapProviderId::BingSatellite, [0; 3]))),
            first
        );
        assert!(pane.update_map_selection(Some((1, MapProviderId::BingSatellite, [0; 3]))) > first);
    }

    #[test]
    fn scene_map_none_provider_or_reference_produces_no_selection() {
        let mut pane = Scene3dPane::default();
        assert_eq!(pane.update_map_selection(None), 0);
        assert!(pane.map_selection.is_none());
    }

    #[test]
    fn ghost_trace_resolves_when_matching_field_loads() {
        let mut workspace = Workspace::new();
        let root = workspace.tree.root().unwrap();
        let Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) = workspace.tree.tiles.get_mut(root)
        else {
            panic!("root should be a plot");
        };
        pane.add_ghost(crate::plot::GhostTrace {
            source: None,
            topic: "ATT".into(),
            field: "Roll".into(),
            color: [1.0, 0.0, 0.0, 1.0],
            width_px: 2.0,
            mode: TraceMode::Step,
            visible: false,
            text_filter: None,
            text_offsets: Vec::new(),
        });

        let mut ids = delog_core::identity::IdentityRegistry::new();
        let source = ids.add_source("flight");
        let topic = ids.add_topic(source, "ATT").unwrap();
        let field = ids.add_field(topic, "Roll").unwrap();
        let snapshot = StoreSnapshot::from_registry(&ids, [], 0).unwrap();

        assert_eq!(workspace.resolve_ghosts(&snapshot), 1);
        let pane = match workspace.tree.tiles.get(root).unwrap() {
            egui_tiles::Tile::Pane(Pane::Plot(pane)) => pane,
            _ => panic!("root should remain a plot"),
        };
        assert!(pane.ghosts.is_empty());
        assert_eq!(pane.traces.len(), 1);
        assert_eq!(pane.traces[0].field, field);
        assert_eq!(pane.traces[0].mode, TraceMode::Step);
        assert!(!pane.traces[0].visible);
    }

    #[test]
    fn ghost_trace_stays_missing_when_field_is_ambiguous() {
        let mut workspace = Workspace::new();
        let root = workspace.tree.root().unwrap();
        let Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) = workspace.tree.tiles.get_mut(root)
        else {
            panic!("root should be a plot");
        };
        pane.add_ghost(crate::plot::GhostTrace {
            source: None,
            topic: "ATT".into(),
            field: "Roll".into(),
            color: [0.0, 1.0, 0.0, 1.0],
            width_px: 1.0,
            mode: TraceMode::Line,
            visible: true,
            text_filter: None,
            text_offsets: Vec::new(),
        });

        let mut ids = delog_core::identity::IdentityRegistry::new();
        for source_name in ["left", "right"] {
            let source = ids.add_source(source_name);
            let topic = ids.add_topic(source, "ATT").unwrap();
            ids.add_field(topic, "Roll").unwrap();
        }
        let snapshot = StoreSnapshot::from_registry(&ids, [], 0).unwrap();

        assert_eq!(workspace.resolve_ghosts(&snapshot), 0);
        let pane = match workspace.tree.tiles.get(root).unwrap() {
            egui_tiles::Tile::Pane(Pane::Plot(pane)) => pane,
            _ => panic!("root should remain a plot"),
        };
        assert!(pane.traces.is_empty());
        assert_eq!(pane.ghosts.len(), 1);
        assert_eq!(pane.ghosts[0].topic, "ATT");
        assert_eq!(pane.ghosts[0].field, "Roll");
    }

    #[test]
    fn split_root_adds_a_second_plot_pane_under_linear_root() {
        let mut workspace = Workspace::new();
        let root = workspace.tree.root().unwrap();

        workspace.split_plot(root, SplitDirection::Horizontal);

        assert_eq!(workspace.plot_panes().count(), 2);
        let root = workspace.tree.root().unwrap();
        let Some(egui_tiles::Tile::Container(container)) = workspace.tree.tiles.get(root) else {
            panic!("root should be a container after split");
        };
        assert_eq!(container.kind(), egui_tiles::ContainerKind::Horizontal);
        assert_eq!(container.num_children(), 2);
    }

    #[test]
    fn split_child_with_new_direction_wraps_the_pane() {
        let mut workspace = Workspace::new();
        let root = workspace.tree.root().unwrap();
        workspace.split_plot(root, SplitDirection::Horizontal);
        let first_pane = workspace
            .tree
            .tiles
            .iter()
            .find_map(|(id, tile)| matches!(tile, egui_tiles::Tile::Pane(_)).then_some(*id))
            .unwrap();

        workspace.split_plot(first_pane, SplitDirection::Vertical);

        assert_eq!(workspace.plot_panes().count(), 3);
        assert!(workspace.tree.tiles.tiles().any(|tile| matches!(
            tile,
            egui_tiles::Tile::Container(container)
                if container.kind() == egui_tiles::ContainerKind::Vertical
                    && container.num_children() == 2
        )));
    }

    #[test]
    fn cross_direction_split_keeps_the_wrapped_pane_in_its_slot() {
        // Root vertical: pane 1 on top, pane 2 on the bottom.
        let mut workspace = Workspace::new();
        let pane1 = workspace.tree.root().unwrap();
        workspace.split_plot(pane1, SplitDirection::Vertical);

        let root = workspace.tree.root().unwrap();
        let top_children = match workspace.tree.tiles.get(root) {
            Some(egui_tiles::Tile::Container(c)) => c.children_vec(),
            _ => panic!("root should be a vertical container"),
        };
        assert_eq!(top_children[0], pane1, "pane 1 starts on top");

        // Split the TOP pane horizontally: the new horizontal wrapper must
        // stay in the top slot, not get appended to the bottom.
        workspace.split_plot(pane1, SplitDirection::Horizontal);

        let children = match workspace.tree.tiles.get(root) {
            Some(egui_tiles::Tile::Container(c)) => c.children_vec(),
            _ => panic!("root should still be a vertical container"),
        };
        assert_eq!(children.len(), 2);
        let Some(egui_tiles::Tile::Container(wrapper)) = workspace.tree.tiles.get(children[0])
        else {
            panic!("the top slot should hold the new horizontal wrapper");
        };
        assert_eq!(wrapper.kind(), egui_tiles::ContainerKind::Horizontal);
        assert!(wrapper.has_child(pane1), "pane 1 stays inside its wrapper");
        assert_eq!(children[1], top_children[1], "pane 2 stays on the bottom");
    }

    #[test]
    fn edge_drop_splits_root_and_adds_all_dropped_traces_to_new_pane() {
        let mut workspace = Workspace::new();
        let root = workspace.tree.root().unwrap();

        let added =
            workspace.split_plot_with_traces(root, DropEdge::Left, &[FieldId(7), FieldId(9)]);
        assert_eq!(added, vec![FieldId(7), FieldId(9)]);

        let root = workspace.tree.root().unwrap();
        let Some(egui_tiles::Tile::Container(container)) = workspace.tree.tiles.get(root) else {
            panic!("root should be a container after edge split");
        };
        assert_eq!(container.kind(), egui_tiles::ContainerKind::Horizontal);
        let children = container.children_vec();
        let new_pane = children[0];
        let Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) = workspace.tree.tiles.get(new_pane)
        else {
            panic!("left child should be the new plot pane");
        };
        assert_eq!(
            pane.fields().collect::<Vec<_>>(),
            vec![FieldId(7), FieldId(9)]
        );

        let before = workspace.plot_panes().count();
        assert!(
            workspace
                .split_plot_with_traces(new_pane, DropEdge::Right, &[])
                .is_empty()
        );
        assert_eq!(workspace.plot_panes().count(), before);
    }

    #[test]
    fn set_all_plot_legends_updates_existing_and_future_plots() {
        let mut workspace = Workspace::new();
        let root = workspace.tree.root().unwrap();
        workspace.split_plot(root, SplitDirection::Horizontal);

        workspace.set_all_plot_legends(false);
        let root = workspace.tree.root().unwrap();
        workspace.split_plot(root, SplitDirection::Vertical);
        assert!(workspace.plot_panes().all(|pane| !pane.show_legend));

        workspace.set_all_plot_legends(true);
        let root = workspace.tree.root().unwrap();
        workspace.split_plot(root, SplitDirection::Vertical);
        assert!(workspace.plot_panes().all(|pane| pane.show_legend));
    }

    #[test]
    fn all_plot_legends_visible_requires_every_plot() {
        let mut workspace = Workspace::new();
        let root = workspace.tree.root().unwrap();
        workspace.split_plot(root, SplitDirection::Horizontal);
        assert!(workspace.all_plot_legends_visible());

        workspace.plot_panes_mut().next().unwrap().show_legend = false;

        assert!(!workspace.all_plot_legends_visible());
    }

    #[test]
    fn equalize_plot_heights_resets_vertical_split_shares() {
        let mut workspace = Workspace::new();
        let root = workspace.tree.root().unwrap();
        workspace.split_plot(root, SplitDirection::Vertical);
        let root = workspace.tree.root().unwrap();
        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
            workspace.tree.tiles.get_mut(root)
        else {
            panic!("root should be a linear split");
        };
        assert_eq!(linear.dir, egui_tiles::LinearDir::Vertical);
        let children = linear.children.clone();
        linear.shares.set_share(children[0], 4.0);
        linear.shares.set_share(children[1], 1.0);

        workspace.equalize_plot_heights();

        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
            workspace.tree.tiles.get(root)
        else {
            panic!("root should remain a linear split");
        };
        assert_eq!(linear.shares[children[0]], 1.0);
        assert_eq!(linear.shares[children[1]], 1.0);
    }

    #[test]
    fn equalize_plot_heights_resets_horizontal_split_shares() {
        let mut workspace = Workspace::new();
        let root = workspace.tree.root().unwrap();
        workspace.split_plot(root, SplitDirection::Horizontal);
        let root = workspace.tree.root().unwrap();
        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
            workspace.tree.tiles.get_mut(root)
        else {
            panic!("root should be a linear split");
        };
        assert_eq!(linear.dir, egui_tiles::LinearDir::Horizontal);
        let children = linear.children.clone();
        linear.shares.set_share(children[0], 1.0);
        linear.shares.set_share(children[1], 6.0);

        workspace.equalize_plot_heights();

        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
            workspace.tree.tiles.get(root)
        else {
            panic!("root should remain a linear split");
        };
        assert_eq!(linear.shares[children[0]], 1.0);
        assert_eq!(linear.shares[children[1]], 1.0);
    }

    #[test]
    fn equalize_plot_heights_resets_grid_row_shares() {
        let mut tiles = egui_tiles::Tiles::default();
        let panes = (0..4)
            .map(|_| tiles.insert_pane(Pane::Plot(PlotPane::default())))
            .collect::<Vec<_>>();
        let root = tiles.insert_container(egui_tiles::Container::new(
            egui_tiles::ContainerKind::Grid,
            panes,
        ));
        let mut workspace = Workspace {
            tree: egui_tiles::Tree::new("plot_workspace", root, tiles),
            focused: None,
            shared_y_gutter: 0.0,
            default_show_legend: true,
        };
        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Grid(grid))) =
            workspace.tree.tiles.get_mut(root)
        else {
            panic!("root should be a grid");
        };
        grid.row_shares = vec![3.0, 1.0];

        workspace.equalize_plot_heights();

        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Grid(grid))) =
            workspace.tree.tiles.get(root)
        else {
            panic!("root should remain a grid");
        };
        assert_eq!(grid.row_shares, vec![1.0, 1.0]);
    }

    #[test]
    fn equalize_plot_heights_resets_grid_column_shares() {
        let mut tiles = egui_tiles::Tiles::default();
        let panes = (0..4)
            .map(|_| tiles.insert_pane(Pane::Plot(PlotPane::default())))
            .collect::<Vec<_>>();
        let root = tiles.insert_container(egui_tiles::Container::new(
            egui_tiles::ContainerKind::Grid,
            panes,
        ));
        let mut workspace = Workspace {
            tree: egui_tiles::Tree::new("plot_workspace", root, tiles),
            focused: None,
            shared_y_gutter: 0.0,
            default_show_legend: true,
        };
        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Grid(grid))) =
            workspace.tree.tiles.get_mut(root)
        else {
            panic!("root should be a grid");
        };
        grid.col_shares = vec![1.0, 4.0];

        workspace.equalize_plot_heights();

        let Some(egui_tiles::Tile::Container(egui_tiles::Container::Grid(grid))) =
            workspace.tree.tiles.get(root)
        else {
            panic!("root should remain a grid");
        };
        assert_eq!(grid.col_shares, vec![1.0, 1.0]);
    }

    #[test]
    fn equalize_plot_heights_descends_into_multiple_columns() {
        let mut tiles = egui_tiles::Tiles::default();
        let left_a = tiles.insert_pane(Pane::Plot(PlotPane::default()));
        let left_b = tiles.insert_pane(Pane::Plot(PlotPane::default()));
        let right_a = tiles.insert_pane(Pane::Plot(PlotPane::default()));
        let right_b = tiles.insert_pane(Pane::Plot(PlotPane::default()));
        let left = tiles.insert_container(egui_tiles::Container::new(
            egui_tiles::ContainerKind::Vertical,
            vec![left_a, left_b],
        ));
        let right = tiles.insert_container(egui_tiles::Container::new(
            egui_tiles::ContainerKind::Vertical,
            vec![right_a, right_b],
        ));
        let root = tiles.insert_container(egui_tiles::Container::new(
            egui_tiles::ContainerKind::Horizontal,
            vec![left, right],
        ));
        let mut workspace = Workspace {
            tree: egui_tiles::Tree::new("plot_workspace", root, tiles),
            focused: None,
            shared_y_gutter: 0.0,
            default_show_legend: true,
        };
        for (id, first, second) in [(left, left_a, left_b), (right, right_a, right_b)] {
            let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
                workspace.tree.tiles.get_mut(id)
            else {
                panic!("column should be a linear split");
            };
            linear.shares.set_share(first, 2.0);
            linear.shares.set_share(second, 5.0);
        }

        workspace.equalize_plot_heights();

        for (id, first, second) in [(left, left_a, left_b), (right, right_a, right_b)] {
            let Some(egui_tiles::Tile::Container(egui_tiles::Container::Linear(linear))) =
                workspace.tree.tiles.get(id)
            else {
                panic!("column should remain a linear split");
            };
            assert_eq!(linear.shares[first], 1.0);
            assert_eq!(linear.shares[second], 1.0);
        }
    }

    #[test]
    fn drop_edge_prefers_the_nearest_edge_inside_the_threshold() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 100.0));
        assert_eq!(
            DropEdge::from_pos(rect, egui::pos2(3.0, 50.0)),
            Some(DropEdge::Left)
        );
        assert_eq!(
            DropEdge::from_pos(rect, egui::pos2(197.0, 50.0)),
            Some(DropEdge::Right)
        );
        assert_eq!(DropEdge::from_pos(rect, rect.center()), None);
    }

    #[test]
    fn close_plot_removes_its_fields_and_keeps_a_workspace_alive() {
        let mut workspace = Workspace::new();
        let root = workspace.tree.root().unwrap();
        let Some(egui_tiles::Tile::Pane(Pane::Plot(pane))) = workspace.tree.tiles.get_mut(root)
        else {
            panic!("root should start as a pane");
        };
        pane.add_trace(FieldId(42));

        let removed = workspace.close_plot(root);

        assert_eq!(removed, vec![FieldId(42)]);
        assert_eq!(workspace.plot_panes().count(), 1);
        assert!(workspace.fields().next().is_none());
    }
}
