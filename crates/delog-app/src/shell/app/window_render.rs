use std::sync::Arc;

use delog_core::snapshot::StoreSnapshot;

use crate::shell::app::command_palette::PaletteEntry;
use crate::shell::app::viewport_actions::{
    ViewportAction, collect_shortcut_actions, discard_picker_actions_from,
};
use crate::shell::app::{DelogApp, central_workspace_frame, collapsed_data_browser_width};
use crate::shell::windows::{ExtendedWindow, MIN_WINDOW_SIZE, WindowId};
use crate::shell::workspace::{PlotServices, Workspace};

impl DelogApp {
    pub(crate) fn render_extended_windows(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        snapshot: &Arc<StoreSnapshot>,
        palette_entries: &[PaletteEntry],
    ) -> Vec<ViewportAction> {
        let mut actions = Vec::new();
        if self.windows.is_empty() {
            return actions;
        }
        let mut windows = std::mem::take(&mut self.windows);
        let model = self.browser_model.take();
        let mut closed = Vec::new();
        for index in 0..windows.len() {
            let id = windows[index].id;
            let mut window =
                std::mem::replace(&mut windows[index], ExtendedWindow::placeholder(id));
            let builder = egui::ViewportBuilder::default()
                .with_title(window.title.clone())
                .with_inner_size(window.size)
                .with_min_inner_size(MIN_WINDOW_SIZE);
            ctx.show_viewport_immediate(id.viewport_id(), builder, |ui, _class| {
                if window.browser.collapsed {
                    let tokens = crate::ui::design_tokens::DesignTokens::from_style(ui.style());
                    let collapsed_frame =
                        egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::ZERO);
                    egui::Panel::left(id.id_salt().with("browser_collapsed"))
                        .resizable(false)
                        .show_separator_line(false)
                        .frame(collapsed_frame)
                        .exact_size(collapsed_data_browser_width(ui.style()))
                        .show_inside(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.add_space(15.0);
                                ui.horizontal(|ui| {
                                    ui.add_space(tokens.space_sm);
                                    if crate::plotting::browser::data_browser_toggle_button(
                                        ui,
                                        crate::ui::icons::panel_left_open(),
                                        "Show data browser",
                                    )
                                    .clicked()
                                    {
                                        window.browser.collapsed = false;
                                        window.browser.focus_filter = true;
                                    }
                                });
                            });
                        });
                } else {
                    if std::mem::take(&mut window.browser.focus_filter) {
                        ui.ctx().memory_mut(|memory| {
                            memory.request_focus(crate::plotting::browser::filter_id(id.id_salt()))
                        });
                    }
                    egui::Panel::left(id.id_salt().with("browser"))
                        .resizable(true)
                        .min_size(360.0)
                        .default_size(360.0)
                        .show_inside(ui, |ui| {
                            if let Some((epoch, model)) = model.as_ref() {
                                let response = crate::plotting::browser::ui(
                                    ui,
                                    id.id_salt(),
                                    id.0,
                                    *epoch,
                                    model,
                                    &mut window.browser.query,
                                    &mut window.browser.filter,
                                    &mut window.browser.selection,
                                    &mut self.offset_dialog,
                                );
                                if response.collapse_requested {
                                    window.browser.collapsed = true;
                                }
                                self.apply_browser_response(response, snapshot);
                            }
                        });
                }
                self.render_workspace_window(ui, frame, snapshot, id, &mut window.workspace);
                actions.extend(self.show_hosted_picker(ui.ctx(), id, palette_entries));
                actions.extend(collect_shortcut_actions(
                    ui.ctx(),
                    id,
                    self.command_palette.is_open(),
                ));
                if let Some(rect) = ui.ctx().input(|i| i.viewport().inner_rect) {
                    window.size = [rect.width(), rect.height()];
                }
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    closed.push(id);
                }
            });
            windows[index] = window;
        }
        self.browser_model = model;
        for id in closed {
            discard_picker_actions_from(&mut actions, id);
            if self.picker_host.is_some_and(|host| host.window == id) {
                self.close_all_pickers();
            }
            if let Some(index) = windows.iter().position(|window| window.id == id) {
                let window = windows.remove(index);
                for field in
                    crate::shell::windows::fields_only_in(&window, &self.workspace, &windows)
                {
                    self.caches.unpin(field);
                }
            }
        }
        windows.extend(std::mem::take(&mut self.windows));
        self.windows = windows;
        actions
    }

    pub(crate) fn render_workspace_window(
        &mut self,
        ui: &mut egui::Ui,
        frame: &eframe::Frame,
        snapshot: &Arc<StoreSnapshot>,
        window: WindowId,
        workspace: &mut Workspace,
    ) {
        central_workspace_frame(ui.style()).show(ui, |ui| {
            // The workspace renders even before any log loads, so plots can be
            // arranged and the 3D view opened on an empty session.

            let workspace_rect = ui.available_rect_before_wrap();

            // The central panel is a fallback drop zone: dropping a field onto
            // empty workspace space plots it in the first pane.
            let frame_style = egui::Frame::default();
            let mut handled_workspace_drop = false;
            let (_, dropped) =
                ui.dnd_drop_zone::<crate::plotting::browser::FieldDrag, ()>(frame_style, |ui| {
                    // Owned metrics handle: `behavior` borrows `self` mutably
                    // below, so we can't reach `self.session` while it lives.
                    let tree_metrics = self.session.metrics().clone();
                    if window.is_main() {
                        let live_map_scopes = workspace.map_scopes();
                        self.gpu.retain_map_scopes(frame, &live_map_scopes);
                        if let Some(manager) = self.tile_manager.as_mut() {
                            manager.retain_scopes(&live_map_scopes);
                        }
                    }
                    let services = PlotServices {
                        frame,
                        snapshot,
                        metrics: self.session.metrics(),
                        gpu: &mut self.gpu,
                        tile_manager: self.tile_manager.as_mut(),
                        tile_manager_error: self.tile_manager_error.as_deref(),
                        caches: &mut self.caches,
                        view: &mut self.view,
                        origin_us: self.origin_us,
                        hover_mode: &mut self.hover_mode,
                        snap_playhead: &mut self.snap_playhead,
                        marker_us: &mut self.marker_us,
                        armed_tool: &mut self.armed_tool,
                        render_tuning: self.settings.render,
                        scene3d: self.settings.scene3d,
                        playhead_us: snapshot.global_time_range().map(|_| self.playback.t_us),
                        playing: self.playback.playing,
                        lock_readouts: self.lock_readouts,
                        alt_held: self.alt_held,
                        vehicles: &self.vehicles,
                        trajectories: &self.vehicle_trajectories,
                        traj_generation: self.traj_vehicle_revision,
                        shared_y_gutter: workspace.shared_y_gutter,
                        plot_display: self.settings.plot,
                        markers: self.markers.as_slice(),
                        window,
                        allow_image_export: window.is_main(),
                    };
                    let mut behavior = crate::shell::workspace::Behavior::new(services);
                    // `workspace_tree`: the egui_tiles layout + pane rendering.
                    // Profiling (2026-06-28) showed egui_tiles' own machinery is
                    // negligible (~0.02 ms); the cost is the per-pane `pane_ui`
                    // render. `ui_workspace − workspace_tree` is begin/retain +
                    // action handling.
                    let tree_timer = tree_metrics.scope(crate::shell::windows::tree_scope(window));
                    workspace.tree.ui(&mut behavior, ui);
                    drop(tree_timer);
                    let actions = behavior.into_actions();
                    workspace.repair_focus();
                    workspace.enforce_single_annotation_editor();
                    // Share the widest pane gutter so stacked plots align next
                    // frame. Converges in one frame; until then each
                    // pane never drops below its own gutter, so labels never
                    // clip.
                    workspace.shared_y_gutter = actions.max_y_gutter;
                    if let Some((tile_id, direction)) = actions.split {
                        workspace.split_plot(tile_id, direction);
                    }
                    if let Some((tile_id, edge, fields)) = actions.edge_drop {
                        let added = workspace.split_plot_with_traces(tile_id, edge, &fields);
                        if !added.is_empty() {
                            handled_workspace_drop = true;
                            for field in added {
                                self.caches.request(field, snapshot);
                            }
                        }
                    }
                    if let Some(mv) = actions.legend_move {
                        let field = workspace.apply_legend_move(mv);
                        self.caches.request(field, snapshot);
                        handled_workspace_drop = true;
                    }
                    if let Some(tile_id) = actions.close {
                        for field in workspace.close_plot(tile_id) {
                            self.caches.unpin(field);
                        }
                    }
                    if let Some(tile_id) = actions.focus {
                        workspace.focused = Some(tile_id);
                    }
                    if let Some(t_us) = actions.scrub_to
                        && let Some(range) = snapshot.global_time_range()
                    {
                        self.playback.scrub(t_us, range);
                    }
                    if actions.view_changed {
                        self.playback.unlock_live();
                        // Manual pan/zoom drops out of fit-all (like a scrub
                        // disengages live-follow).
                        self.fit_view_all = false;
                    }
                    if actions.open_vehicle_config {
                        self.vehicle_dialog.open = true;
                    }
                    if actions.open_scene_settings {
                        self.settings_dialog.open_scene3d();
                    }
                    if actions.export_kml {
                        self.spawn_export_kml_dialog(ui.ctx(), snapshot);
                    }
                    if let Some(action) = actions.image {
                        match action {
                            crate::shell::workspace::WorkspaceImageAction::CopyPlot { rect } => {
                                self.queue_image_capture(
                                    ui.ctx(),
                                    crate::export::image_export::ImageCaptureIntent::plot(
                                        crate::export::image_export::ImageCaptureAction::Copy,
                                        rect,
                                        self.frame,
                                    ),
                                );
                            }
                            crate::shell::workspace::WorkspaceImageAction::ExportPlot { rect } => {
                                self.queue_image_capture(
                                    ui.ctx(),
                                    crate::export::image_export::ImageCaptureIntent::plot(
                                        crate::export::image_export::ImageCaptureAction::Export,
                                        rect,
                                        self.frame,
                                    ),
                                );
                            }
                        }
                    }
                });
            let dropped = dropped.and_then(|drag| drag.accepted_by(window.0).map(<[_]>::to_vec));
            if let Some(fields) = dropped
                && !handled_workspace_drop
            {
                for &field in fields.iter() {
                    if workspace.add_trace_to_first_plot(field) {
                        self.caches.request(field, snapshot);
                    }
                }
            }
            if window.is_main() {
                self.start_queued_image_capture(ui.ctx(), Some(workspace_rect));
            }
        });
    }
}
