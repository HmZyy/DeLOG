//! Top-level eframe application state.

use delog_cache::CacheManager;

use crate::about;
use crate::browser::{self, BrowserModel};
use crate::gpu::GpuBridge;
use crate::live::ConnectionDialog;
use crate::plot::ViewX;
use crate::session::Session;
use crate::timeline::Playback;
use crate::workspace::{PlotServices, Workspace};

pub struct DelogApp {
    session: Session,
    gpu: GpuBridge,
    caches: CacheManager,
    workspace: Workspace,
    playback: Playback,
    view: Option<ViewX>,
    hover_mode: delog_core::field_view::SampleMode,
    show_legend: bool,
    frame: u64,
    last_epoch: u64,
    origin_us: i64,
    path_input: String,
    browser_query: String,
    browser_selection: browser::Selection,
    show_about: bool,
    show_connection_dialog: bool,
    connection_dialog: ConnectionDialog,
    configured_endpoint: Option<delog_stream::Endpoint>,
}

impl DelogApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        Self {
            session: Session::new(cc.egui_ctx.clone()),
            gpu: GpuBridge::from_creation_context(cc),
            caches: CacheManager::new(),
            workspace: Workspace::new(),
            playback: Playback::default(),
            view: None,
            hover_mode: delog_core::field_view::SampleMode::Prev,
            show_legend: true,
            frame: 0,
            last_epoch: u64::MAX,
            origin_us: 0,
            path_input: String::new(),
            browser_query: String::new(),
            browser_selection: browser::Selection::default(),
            show_about: false,
            show_connection_dialog: false,
            connection_dialog: ConnectionDialog::default(),
            configured_endpoint: None,
        }
    }

    /// Open every log dropped onto the window this frame (minimal UIX-08).
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = file.path {
                self.session.open_path(path);
            }
        }
    }
}

impl eframe::App for DelogApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.handle_dropped_files(ui.ctx());
        self.session.prune_finished();
        self.frame = self.frame.wrapping_add(1);

        let snapshot = self.session.snapshot();

        // Cache lifecycle: shared origin, frame recency, drain builds, and an
        // epoch-driven incremental append + GC (§8.5).
        if let Some(range) = snapshot.global_time_range() {
            self.origin_us = range.min_us;
            self.caches.set_origin(self.origin_us);
            self.view.get_or_insert_with(|| ViewX::from_range(range));

            // Advance the playhead — the single time authority (§11, TLN-01).
            self.playback.clamp_to(range);
            let dt = ui.ctx().input(|i| i.stable_dt) as f64;
            self.playback.advance(dt, range);

            // Idle-aware repaint policy (§11, TLN-06): continuous frames only
            // while playing (later: or a link is Connected, M7). Everything
            // else is event-driven — ingest progress, epoch changes and
            // diagnostics each request their own repaint — so a static plot
            // idles at 0% GPU.
            if self.playback.playing {
                ui.ctx().request_repaint();
            }
        }
        self.caches.begin_frame(self.frame);
        self.caches.poll_builds();
        if snapshot.epoch != self.last_epoch {
            self.caches.on_epoch(&snapshot);
            self.last_epoch = snapshot.epoch;
        }
        // Keep every plotted trace's cache requested/warm.
        for field in self.workspace.fields().collect::<Vec<_>>() {
            self.caches.request(field, &snapshot);
        }
        self.caches.evict_over_budget();

        // Surface captured wgpu errors as diagnostics (GPU-12).
        for message in self.gpu.drain_gpu_errors(frame) {
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::error("gpu", message));
        }

        egui::Panel::top("main_menu").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });

                ui.separator();
                // Minimal open affordance until the toolbar + native dialog
                // (UIX-02) land: type or paste a path, or drop a file anywhere.
                ui.label("Open:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.path_input)
                        .hint_text("path/to/log.BIN")
                        .desired_width(280.0),
                );
                let submit = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Open").clicked() || submit) && !self.path_input.trim().is_empty() {
                    self.session.open_path(self.path_input.trim().to_owned());
                    self.path_input.clear();
                }
                if ui.button("Stream").clicked() {
                    self.show_connection_dialog = true;
                }
                if let Some(endpoint) = &self.configured_endpoint {
                    ui.weak(endpoint.to_string());
                }

                if self.session.has_active_loads() {
                    if ui.button("Cancel").clicked() {
                        self.session.cancel_all();
                    }
                    ui.label(format!(
                        "loading {}",
                        self.session.active_labels().join(", ")
                    ));
                    if let Some(frac) = self.session.overall_progress() {
                        ui.add(egui::ProgressBar::new(frac).desired_width(120.0));
                    } else {
                        ui.spinner();
                    }
                }
            });
        });

        // Global timeline bar (§11, TLN-02/03). `utc_offset_us` stays None
        // until a parser captures a UTC reference (BIN GPS week / ULog
        // time_ref_utc — M6); `any_live` stays false until live links exist
        // (M7): the snapshot has no streaming flag yet.
        if let Some(range) = snapshot.global_time_range() {
            egui::Panel::bottom("timeline").show_inside(ui, |ui| {
                crate::timeline::ui(ui, &mut self.playback, range, None, false);
            });

            // Transport keys (§11, TLN-04) — skipped while a widget owns the
            // keyboard (e.g. the browser filter box).
            if !ui.ctx().egui_wants_keyboard_input() {
                let (space, home, end, left, right) = ui.ctx().input(|i| {
                    (
                        i.key_pressed(egui::Key::Space),
                        i.key_pressed(egui::Key::Home),
                        i.key_pressed(egui::Key::End),
                        i.key_pressed(egui::Key::ArrowLeft),
                        i.key_pressed(egui::Key::ArrowRight),
                    )
                });
                if space {
                    self.playback.toggle();
                }
                if home {
                    self.playback.jump_start(range);
                }
                if end {
                    self.playback.jump_end(range);
                }
                if left || right {
                    let reference = self.workspace.focused_first_field();
                    let target = crate::timeline::step_target(
                        &snapshot,
                        reference,
                        self.playback.t_us,
                        right,
                    );
                    self.playback.scrub(target, range);
                }
            }
        }

        let diagnostics = self.session.diagnostics();
        if let Some(last) = diagnostics.last() {
            egui::Panel::bottom("status").show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.weak(format!("{} notice(s)", diagnostics.len()));
                    ui.separator();
                    ui.label(format!("[{}] {}", last.code, last.message));
                });
            });
        }

        let model = BrowserModel::from_snapshot(&snapshot);
        egui::Panel::left("data_browser")
            .resizable(true)
            .default_size(280.0)
            .show_inside(ui, |ui| {
                browser::ui(
                    ui,
                    &model,
                    &mut self.browser_query,
                    &mut self.browser_selection,
                );
            });

        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            if model.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak("Drop a flight log to begin.");
                });
                return;
            }

            // The central panel is a fallback drop zone: dropping a field onto
            // empty workspace space plots it in the first pane (PLT-13).
            let frame_style = egui::Frame::default();
            let mut handled_workspace_drop = false;
            let (_, dropped) =
                ui.dnd_drop_zone::<Vec<delog_core::identity::FieldId>, ()>(frame_style, |ui| {
                    self.gpu.begin_plot_frame(frame);
                    let services = PlotServices {
                        frame,
                        snapshot: &snapshot,
                        gpu: &mut self.gpu,
                        caches: &mut self.caches,
                        view: &mut self.view,
                        origin_us: self.origin_us,
                        hover_mode: &mut self.hover_mode,
                        show_legend: &mut self.show_legend,
                        playhead_us: snapshot.global_time_range().map(|_| self.playback.t_us),
                        playing: self.playback.playing,
                    };
                    let mut behavior = crate::workspace::Behavior::new(services);
                    self.workspace.tree.ui(&mut behavior, ui);
                    let actions = behavior.into_actions();
                    if let Some((tile_id, direction)) = actions.split {
                        self.workspace.split_plot(tile_id, direction);
                    }
                    if let Some((tile_id, edge, fields)) = actions.edge_drop {
                        let added = self
                            .workspace
                            .split_plot_with_traces(tile_id, edge, &fields);
                        if !added.is_empty() {
                            handled_workspace_drop = true;
                            for field in added {
                                self.caches.request(field, &snapshot);
                            }
                        }
                    }
                    if let Some(tile_id) = actions.close {
                        for field in self.workspace.close_plot(tile_id) {
                            self.caches.unpin(field);
                        }
                    }
                    if let Some(tile_id) = actions.focus {
                        self.workspace.focused = Some(tile_id);
                    }
                    if let Some(t_us) = actions.scrub_to
                        && let Some(range) = snapshot.global_time_range()
                    {
                        self.playback.scrub(t_us, range);
                    }
                });
            if let Some(fields) = dropped
                && !handled_workspace_drop
            {
                for &field in fields.iter() {
                    if self.workspace.add_trace_to_first_plot(field) {
                        self.caches.request(field, &snapshot);
                    }
                }
            }
            let plotted: Vec<_> = self.workspace.fields().collect();
            self.gpu.retain_plotted_buffers(frame, &plotted);
        });

        about::window(ui.ctx(), &mut self.show_about);
        if let Some(endpoint) = self
            .connection_dialog
            .ui(ui.ctx(), &mut self.show_connection_dialog)
        {
            self.configured_endpoint = Some(endpoint);
        }
    }
}
