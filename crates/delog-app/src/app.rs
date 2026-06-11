//! Top-level eframe application state.

use delog_cache::CacheManager;
use delog_core::time::TimeRange;

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
    /// Paths picked in the native open dialog, sent from its worker thread
    /// (the dialog must never block the UI thread, §19.6).
    picked_files: std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>,
    picked_files_tx: std::sync::mpsc::Sender<Vec<std::path::PathBuf>>,
    browser_query: String,
    browser_selection: browser::Selection,
    offset_dialog: Option<(delog_core::identity::SourceId, i64)>,
    show_about: bool,
    show_connection_dialog: bool,
    connection_dialog: ConnectionDialog,
}

impl DelogApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        let (picked_files_tx, picked_files) = std::sync::mpsc::channel();
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
            picked_files,
            picked_files_tx,
            browser_query: String::new(),
            browser_selection: browser::Selection::default(),
            offset_dialog: None,
            show_about: false,
            show_connection_dialog: false,
            connection_dialog: ConnectionDialog::default(),
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

    /// Show the native open dialog on a worker thread (UIX-02 open; §19.6
    /// never-block) and queue the picked logs for the next frame.
    fn spawn_open_dialog(&self, ctx: &egui::Context) {
        let tx = self.picked_files_tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("delog-open-dialog".into())
            .spawn(move || {
                let picked = rfd::FileDialog::new()
                    .add_filter("Flight logs", &["bin", "BIN", "ulg", "ulog", "tlog"])
                    .add_filter("All files", &["*"])
                    .set_title("Open flight logs")
                    .pick_files();
                if let Some(paths) = picked {
                    let _ = tx.send(paths);
                    ctx.request_repaint();
                }
            })
            .expect("spawn file dialog thread");
    }

    /// Drain dialog results queued by the worker thread.
    fn handle_picked_files(&mut self) {
        while let Ok(paths) = self.picked_files.try_recv() {
            for path in paths {
                self.session.open_path(path);
            }
        }
    }

    fn lock_to_live(&mut self, range: TimeRange) {
        self.playback.lock_to_live(range);
        self.pin_view_to_live(range);
    }

    fn pin_view_to_live(&mut self, range: TimeRange) {
        let span = self
            .view
            .map(|view| view.span_us())
            .unwrap_or_else(|| (range.max_us - range.min_us).max(1));
        self.view = Some(ViewX::locked_to_tail(range, span));
    }
}

impl eframe::App for DelogApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.handle_dropped_files(ui.ctx());
        self.handle_picked_files();
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
            let dt = ui.ctx().input(|i| i.stable_dt) as f64;
            self.playback.clamp_to(range);
            self.playback.advance(dt, range);
            if self.session.has_live_links() && self.playback.follow_live {
                self.pin_view_to_live(range);
            }

            // Idle-aware repaint policy (§11, TLN-06): continuous frames only
            // while playing (later: or a link is Connected, M7). Everything
            // else is event-driven — ingest progress, epoch changes and
            // diagnostics each request their own repaint — so a static plot
            // idles at 0% GPU.
            if self.playback.playing || self.session.has_connected_live() {
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
                if ui
                    .button("Open…")
                    .on_hover_text("Open flight logs (or drop files anywhere)")
                    .clicked()
                {
                    self.spawn_open_dialog(ui.ctx());
                }
                if ui.button("Stream").clicked() {
                    self.show_connection_dialog = true;
                }
                let live_statuses = self.session.live_statuses();
                if !live_statuses.is_empty() {
                    let lock_label = if self.playback.follow_live {
                        "Live locked"
                    } else {
                        "Lock live"
                    };
                    let mut lock_button = egui::Button::new(lock_label);
                    if !self.playback.follow_live {
                        lock_button =
                            lock_button.fill(ui.visuals().warn_fg_color.gamma_multiply(0.25));
                    }
                    let lock_response = ui
                        .add(lock_button)
                        .on_hover_text("Lock X view and playhead to the live tail (End)");
                    if lock_response.clicked()
                        && let Some(range) = snapshot.global_time_range()
                    {
                        self.lock_to_live(range);
                    }
                    for status in live_statuses {
                        ui.separator();
                        ui.weak(format!(
                            "{} {} frames {} rows {}{}",
                            status.state.label(),
                            status.endpoint,
                            status.link.rx_frames,
                            status.ingest.rows,
                            status.recording.as_ref().map(|_| " rec").unwrap_or("")
                        ));
                    }
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
                let action = crate::timeline::ui(
                    ui,
                    &mut self.playback,
                    range,
                    None,
                    self.session.has_live_links(),
                );
                if action.lock_live {
                    self.lock_to_live(range);
                }
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
                    if self.session.has_live_links() {
                        self.lock_to_live(range);
                    } else {
                        self.playback.jump_end(range);
                    }
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
                // Offset edits go through the ingest thread (the single
                // registry writer) and come back as a new epoch (BRW-07).
                if let Some((source, offset_us)) = browser::ui(
                    ui,
                    &model,
                    &mut self.browser_query,
                    &mut self.browser_selection,
                    &mut self.offset_dialog,
                ) {
                    self.session.set_source_offset(source, offset_us);
                }
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
                    if actions.view_changed {
                        self.playback.unlock_live();
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
            && let Err(err) = self.session.start_live(endpoint, None)
        {
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::error("live-open", err));
        }
    }
}
