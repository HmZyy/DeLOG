//! Tiled plot workspace (PLAN.md §10.1, PLT-01).
//!
//! The tile tree owns plot pane state while the app owns the global X view.
//! `egui_tiles` supplies split/tab/drag behavior; this module adapts each tile
//! to DeLOG's plot painting and emits pane-level actions for the app shell.

use std::sync::Arc;

use delog_cache::CacheManager;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use crate::axes;
use crate::gpu::{self, GpuBridge, PaneView};
use crate::hover::{self, HoverTarget};
use crate::legend;
use crate::plot::{PlotPane, ViewX};

pub type TileTree = egui_tiles::Tree<Pane>;

#[derive(Debug)]
pub enum Pane {
    Plot(PlotPane),
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

pub struct Workspace {
    pub tree: TileTree,
}

impl Workspace {
    pub fn new() -> Self {
        let mut tiles = egui_tiles::Tiles::default();
        let root = tiles.insert_pane(Pane::Plot(PlotPane::default()));
        Self {
            tree: egui_tiles::Tree::new("plot_workspace", root, tiles),
        }
    }

    pub fn fields(&self) -> impl Iterator<Item = FieldId> + '_ {
        self.plot_panes().flat_map(PlotPane::fields)
    }

    pub fn add_trace_to_first_plot(&mut self, field: FieldId) -> bool {
        self.plot_panes_mut()
            .next()
            .is_some_and(|pane| pane.add_trace(field))
    }

    pub fn split_plot(&mut self, tile_id: egui_tiles::TileId, direction: SplitDirection) {
        let new_pane = self.tree.tiles.insert_pane(Pane::Plot(PlotPane::default()));
        let kind = match direction {
            SplitDirection::Horizontal => egui_tiles::ContainerKind::Horizontal,
            SplitDirection::Vertical => egui_tiles::ContainerKind::Vertical,
        };

        if self.tree.root() == Some(tile_id) {
            let root = self
                .tree
                .tiles
                .insert_container(egui_tiles::Container::new(kind, vec![tile_id, new_pane]));
            self.tree.root = Some(root);
            return;
        }

        if let Some(parent_id) = self.tree.tiles.parent_of(tile_id) {
            let wrap_in_new_container = {
                let Some(egui_tiles::Tile::Container(parent)) = self.tree.tiles.get_mut(parent_id)
                else {
                    return;
                };

                if parent.kind() == kind {
                    match parent {
                        egui_tiles::Container::Linear(linear) => {
                            let index = linear
                                .children
                                .iter()
                                .position(|id| *id == tile_id)
                                .map_or(linear.children.len(), |i| i + 1);
                            linear.children.insert(index, new_pane);
                        }
                        egui_tiles::Container::Tabs(tabs) => {
                            tabs.add_child(new_pane);
                            tabs.set_active(new_pane);
                        }
                        egui_tiles::Container::Grid(grid) => grid.add_child(new_pane),
                    }
                    false
                } else {
                    parent.remove_child(tile_id).is_some()
                }
            };

            if wrap_in_new_container {
                let replacement = self
                    .tree
                    .tiles
                    .insert_container(egui_tiles::Container::new(kind, vec![tile_id, new_pane]));
                if let Some(egui_tiles::Tile::Container(parent)) =
                    self.tree.tiles.get_mut(parent_id)
                {
                    parent.add_child(replacement);
                    if let egui_tiles::Container::Tabs(tabs) = parent {
                        tabs.set_active(replacement);
                    }
                }
            }
        }
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
            egui_tiles::Tile::Container(_) => None,
        })
    }

    fn plot_panes_mut(&mut self) -> impl Iterator<Item = &mut PlotPane> + '_ {
        self.tree.tiles.tiles_mut().filter_map(|tile| match tile {
            egui_tiles::Tile::Pane(Pane::Plot(pane)) => Some(pane),
            egui_tiles::Tile::Container(_) => None,
        })
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
pub struct WorkspaceActions {
    pub split: Option<(egui_tiles::TileId, SplitDirection)>,
    pub close: Option<egui_tiles::TileId>,
    pub remove_trace: Vec<FieldId>,
}

pub struct PlotServices<'a> {
    pub frame: &'a eframe::Frame,
    pub snapshot: &'a Arc<StoreSnapshot>,
    pub gpu: &'a mut GpuBridge,
    pub caches: &'a mut CacheManager,
    pub view: &'a mut Option<ViewX>,
    pub origin_us: i64,
    pub hover_mode: &'a mut delog_core::field_view::SampleMode,
    pub show_legend: &'a mut bool,
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
        match pane {
            Pane::Plot(pane) => self.plot_ui(ui, tile_id, pane),
        }
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        match pane {
            Pane::Plot(pane) if pane.traces.is_empty() => "Plot".into(),
            Pane::Plot(pane) => format!("Plot ({})", pane.traces.len()).into(),
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
    fn plot_ui(
        &mut self,
        ui: &mut egui::Ui,
        tile_id: egui_tiles::TileId,
        pane: &mut PlotPane,
    ) -> egui_tiles::UiResponse {
        let frame_style = egui::Frame::default();
        let (response, dropped) =
            ui.dnd_drop_zone::<FieldId, ()>(frame_style, |ui| self.plot_body(ui, tile_id, pane));

        if let Some(field) = dropped
            && pane.add_trace(*field)
        {
            self.services.caches.request(*field, self.services.snapshot);
        }

        if response
            .response
            .drag_started_by(egui::PointerButton::Middle)
        {
            egui_tiles::UiResponse::DragStarted
        } else {
            egui_tiles::UiResponse::None
        }
    }

    fn plot_body(&mut self, ui: &mut egui::Ui, tile_id: egui_tiles::TileId, pane: &mut PlotPane) {
        let outer = ui.available_rect_before_wrap();
        let plot_rect = egui::Rect::from_min_max(
            egui::pos2(outer.left() + axes::Y_GUTTER, outer.top() + 4.0),
            egui::pos2(outer.right() - 4.0, outer.bottom() - axes::X_GUTTER),
        );
        let response = ui.allocate_rect(outer, egui::Sense::click_and_drag());
        self.handle_plot_interaction(&response, plot_rect);
        self.plot_context_menu(tile_id, &response, pane);

        if pane.is_empty() {
            ui.painter().text(
                outer.center(),
                egui::Align2::CENTER_CENTER,
                "Drag a field here to plot it",
                egui::FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
            return;
        }

        let Some(view) = *self.services.view else {
            return;
        };
        if !self.services.gpu.is_available() || plot_rect.width() <= 8.0 {
            return;
        }

        let x_range = view.seconds(self.services.origin_us);
        let y_range = gpu::visible_y_range(self.services.caches, pane, x_range.0, x_range.1);
        let y_unit = y_unit(self.services.snapshot.as_ref(), pane);
        axes::draw(ui, plot_rect, x_range, y_range, y_unit.as_deref());
        let pview = PaneView {
            rect: plot_rect,
            x_range,
            y_range,
        };
        self.services
            .gpu
            .render_pane(ui, self.services.frame, self.services.caches, pane, pview);

        if !ui.ctx().any_popup_open() {
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
            );
        }

        if *self.services.show_legend {
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
                pane,
                &labels,
            ) {
                pane.remove_trace(removed);
                self.services.caches.unpin(removed);
                self.actions.remove_trace.push(removed);
            }
        }
    }

    fn plot_context_menu(
        &mut self,
        tile_id: egui_tiles::TileId,
        response: &egui::Response,
        pane: &mut PlotPane,
    ) {
        response.context_menu(|ui| {
            if ui.button("Reset view").clicked() {
                if let Some(range) = self.services.snapshot.global_time_range() {
                    *self.services.view = Some(ViewX::from_range(range));
                }
                ui.close();
            }

            ui.menu_button("Split", |ui| {
                if ui.button("Horizontal").clicked() {
                    self.actions.split = Some((tile_id, SplitDirection::Horizontal));
                    ui.close();
                }
                if ui.button("Vertical").clicked() {
                    self.actions.split = Some((tile_id, SplitDirection::Vertical));
                    ui.close();
                }
            });

            ui.menu_button("Clear traces", |ui| {
                if ui.button("All").clicked() {
                    for field in pane.fields().collect::<Vec<_>>() {
                        self.services.caches.unpin(field);
                        self.actions.remove_trace.push(field);
                    }
                    pane.clear();
                    ui.close();
                }
                ui.separator();
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
                for (field, label, color) in entries {
                    let clicked = ui
                        .horizontal(|ui| {
                            ui.colored_label(color, "■");
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

            ui.separator();
            ui.menu_button("Hover mode", |ui| {
                use delog_core::field_view::SampleMode::{Linear, Next, Prev};
                ui.radio_value(self.services.hover_mode, Prev, "Previous");
                ui.radio_value(self.services.hover_mode, Next, "Next");
                ui.radio_value(self.services.hover_mode, Linear, "Linear");
            });
            ui.checkbox(self.services.show_legend, "Show legend");

            ui.separator();
            if ui.button("Close").clicked() {
                self.actions.close = Some(tile_id);
                ui.close();
            }
        });
    }

    fn handle_plot_interaction(&mut self, response: &egui::Response, rect: egui::Rect) {
        let Some(mut view) = *self.services.view else {
            return;
        };

        if response.double_clicked() {
            if let Some(range) = self.services.snapshot.global_time_range() {
                *self.services.view = Some(ViewX::from_range(range));
            }
            return;
        }

        if response.dragged() {
            gpu::apply_pan(&mut view, response.drag_delta().x, rect.width());
        }

        if response.hovered() {
            let scroll = response.ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let cursor_frac = response
                    .hover_pos()
                    .map(|p| (p.x - rect.left()) / rect.width().max(1.0))
                    .unwrap_or(0.5);
                gpu::apply_zoom(&mut view, cursor_frac, scroll);
            }
        }

        *self.services.view = Some(view);
    }
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

fn fields_from_removed_tile(tile: egui_tiles::Tile<Pane>) -> Vec<FieldId> {
    match tile {
        egui_tiles::Tile::Pane(Pane::Plot(pane)) => pane.fields().collect(),
        egui_tiles::Tile::Container(_) => Vec::new(),
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
