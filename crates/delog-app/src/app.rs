//! Top-level eframe application state.

use delog_cache::CacheManager;
use delog_core::time::TimeRange;
use std::sync::mpsc;

use crate::about;
use crate::browser::{self, BrowserModel};
use crate::gpu::GpuBridge;
use crate::layout::{LayoutApply, LayoutDoc, LayoutError, LoadOutcome, PendingLayout};
use crate::live::ConnectionDialog;
use crate::plot::ViewX;
use crate::session::Session;
use crate::timeline::Playback;
use crate::workspace::{PlotServices, Workspace};

struct TrajectoryBuildResult {
    epoch: u64,
    vehicle_revision: u64,
    trajectories: Vec<Vec<[f32; 3]>>,
}

type LayoutImportResult = Result<LayoutDoc, LayoutError>;
type LayoutExportResult = Result<std::path::PathBuf, LayoutError>;

#[derive(Default)]
struct SaveLayoutDialog {
    open: bool,
    name: String,
}

#[derive(Default)]
struct LoadLayoutDialog {
    open: bool,
    layouts: Vec<String>,
    selected: Option<usize>,
}

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
    picked_files: mpsc::Receiver<Vec<std::path::PathBuf>>,
    picked_files_tx: mpsc::Sender<Vec<std::path::PathBuf>>,
    imported_layouts: mpsc::Receiver<LayoutImportResult>,
    imported_layouts_tx: mpsc::Sender<LayoutImportResult>,
    exported_layouts: mpsc::Receiver<LayoutExportResult>,
    exported_layouts_tx: mpsc::Sender<LayoutExportResult>,
    browser_query: String,
    browser_selection: browser::Selection,
    offset_dialog: Option<(delog_core::identity::SourceId, i64)>,
    show_about: bool,
    save_layout_dialog: SaveLayoutDialog,
    load_layout_dialog: LoadLayoutDialog,
    pending_layout: Option<PendingLayout>,
    show_connection_dialog: bool,
    connection_dialog: ConnectionDialog,
    /// Configured vehicles for the 3D view (TDV-03); empty until one is added.
    vehicles: Vec<crate::vehicle::VehicleConfig>,
    vehicle_dialog: crate::vehicle_dialog::VehicleDialog,
    /// Cached render-space trajectories, parallel to `vehicles`, rebuilt on a
    /// worker when the data epoch or vehicle set changes (TDV-04/11).
    vehicle_trajectories: Vec<Vec<[f32; 3]>>,
    traj_epoch: u64,
    traj_vehicle_revision: u64,
    vehicle_revision: u64,
    traj_dirty: bool,
    traj_building: Option<(u64, u64)>,
    traj_results: mpsc::Receiver<TrajectoryBuildResult>,
    traj_results_tx: mpsc::Sender<TrajectoryBuildResult>,
}

impl DelogApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        let (picked_files_tx, picked_files) = mpsc::channel();
        let (traj_results_tx, traj_results) = mpsc::channel();
        let (imported_layouts_tx, imported_layouts) = mpsc::channel();
        let (exported_layouts_tx, exported_layouts) = mpsc::channel();
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
            imported_layouts,
            imported_layouts_tx,
            exported_layouts,
            exported_layouts_tx,
            browser_query: String::new(),
            browser_selection: browser::Selection::default(),
            offset_dialog: None,
            show_about: false,
            save_layout_dialog: SaveLayoutDialog {
                open: false,
                name: "default".into(),
            },
            load_layout_dialog: LoadLayoutDialog::default(),
            pending_layout: None,
            show_connection_dialog: false,
            connection_dialog: ConnectionDialog::default(),
            vehicles: Vec::new(),
            vehicle_dialog: crate::vehicle_dialog::VehicleDialog::default(),
            vehicle_trajectories: Vec::new(),
            traj_epoch: u64::MAX,
            traj_vehicle_revision: u64::MAX,
            vehicle_revision: 0,
            traj_dirty: true,
            traj_building: None,
            traj_results,
            traj_results_tx,
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

    fn handle_layout_io_results(&mut self) {
        let snapshot = self.session.snapshot();
        while let Ok(result) = self.imported_layouts.try_recv() {
            match result {
                Ok(doc) => match crate::layout::load_doc(doc, &snapshot) {
                    Ok(LoadOutcome::Applied(layout)) => self.apply_layout(layout),
                    Ok(LoadOutcome::NeedsMapping(pending)) => {
                        self.pending_layout = Some(pending);
                    }
                    Err(err) => self
                        .session
                        .push_diagnostic(delog_core::diagnostics::Diag::error(
                            "layout-import",
                            err.to_string(),
                        )),
                },
                Err(err) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::error(
                        "layout-import",
                        err.to_string(),
                    )),
            }
        }

        while let Ok(result) = self.exported_layouts.try_recv() {
            match result {
                Ok(path) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::info(
                        "layout-export",
                        format!("exported layout to {}", path.display()),
                    )),
                Err(err) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::error(
                        "layout-export",
                        err.to_string(),
                    )),
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

    fn poll_trajectory_builds(&mut self) {
        while let Ok(result) = self.traj_results.try_recv() {
            self.traj_building = self
                .traj_building
                .filter(|&(epoch, rev)| epoch != result.epoch || rev != result.vehicle_revision);
            if result.epoch == self.traj_epoch && result.vehicle_revision == self.vehicle_revision {
                self.vehicle_trajectories = result.trajectories;
                self.traj_vehicle_revision = result.vehicle_revision;
                self.traj_dirty = false;
            }
        }
    }

    fn ensure_trajectory_build(
        &mut self,
        ctx: &egui::Context,
        snapshot: &std::sync::Arc<delog_core::snapshot::StoreSnapshot>,
    ) {
        let target_epoch = snapshot.epoch;
        let target_revision = self.vehicle_revision;
        let needs_build = self.traj_dirty
            || self.traj_epoch != target_epoch
            || self.traj_vehicle_revision != target_revision;
        if !needs_build {
            return;
        }

        self.traj_epoch = target_epoch;
        self.traj_dirty = true;
        if self.vehicles.is_empty() {
            self.vehicle_trajectories.clear();
            self.traj_vehicle_revision = target_revision;
            self.traj_dirty = false;
            self.traj_building = None;
            return;
        }
        if self.traj_building == Some((target_epoch, target_revision)) {
            return;
        }
        if self.traj_building.is_some() {
            return;
        }

        let tx = self.traj_results_tx.clone();
        let ctx = ctx.clone();
        let snapshot = snapshot.clone();
        let vehicles = self.vehicles.clone();
        self.traj_building = Some((target_epoch, target_revision));
        std::thread::Builder::new()
            .name("delog-trajectory-build".into())
            .spawn(move || {
                let trajectories = vehicles
                    .iter()
                    .map(|v| crate::vehicle::build_trajectory(&snapshot, v))
                    .collect();
                let _ = tx.send(TrajectoryBuildResult {
                    epoch: target_epoch,
                    vehicle_revision: target_revision,
                    trajectories,
                });
                ctx.request_repaint();
            })
            .expect("spawn trajectory build thread");
    }

    fn current_layout_doc(
        &self,
        name: String,
        snapshot: &delog_core::snapshot::StoreSnapshot,
    ) -> LayoutDoc {
        crate::layout::current_doc(crate::layout::CurrentLayout {
            name,
            workspace: &self.workspace,
            snapshot,
            view: self.view,
            speed: self.playback.speed as f64,
            follow_live: self.playback.follow_live,
            show_legend: self.show_legend,
            vehicles: &self.vehicles,
        })
    }

    fn save_layout(&mut self, snapshot: &delog_core::snapshot::StoreSnapshot) {
        let name = if self.save_layout_dialog.name.trim().is_empty() {
            "default"
        } else {
            self.save_layout_dialog.name.trim()
        };
        let doc = self.current_layout_doc(name.to_owned(), snapshot);
        match crate::layout::save_named(name, &doc) {
            Ok(()) => self
                .session
                .push_diagnostic(delog_core::diagnostics::Diag::info(
                    "layout-save",
                    format!("saved layout `{name}`"),
                )),
            Err(err) => self
                .session
                .push_diagnostic(delog_core::diagnostics::Diag::error(
                    "layout-save",
                    err.to_string(),
                )),
        }
    }

    fn spawn_export_layout_dialog(
        &self,
        ctx: &egui::Context,
        snapshot: &delog_core::snapshot::StoreSnapshot,
    ) {
        let name = if self.save_layout_dialog.name.trim().is_empty() {
            "layout".to_owned()
        } else {
            self.save_layout_dialog.name.trim().to_owned()
        };
        let doc = self.current_layout_doc(name.clone(), snapshot);
        let tx = self.exported_layouts_tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("delog-layout-export-dialog".into())
            .spawn(move || {
                let file_name = format!("{name}.json");
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("DeLOG layouts", &["json"])
                    .add_filter("All files", &["*"])
                    .set_title("Export layout JSON")
                    .set_file_name(&file_name)
                    .save_file()
                {
                    let result = crate::layout::export_doc(&path, &doc).map(|_| path);
                    let _ = tx.send(result);
                    ctx.request_repaint();
                }
            })
            .expect("spawn layout export dialog thread");
    }

    fn spawn_import_layout_dialog(&self, ctx: &egui::Context) {
        let tx = self.imported_layouts_tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("delog-layout-import-dialog".into())
            .spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("DeLOG layouts", &["json"])
                    .add_filter("All files", &["*"])
                    .set_title("Import layout JSON")
                    .pick_file()
                {
                    let result = crate::layout::import_doc(&path);
                    let _ = tx.send(result);
                    ctx.request_repaint();
                }
            })
            .expect("spawn layout import dialog thread");
    }

    fn load_layout(&mut self, name: &str, snapshot: &delog_core::snapshot::StoreSnapshot) {
        match crate::layout::load_named(name, snapshot) {
            Ok(LoadOutcome::Applied(layout)) => self.apply_layout(layout),
            Ok(LoadOutcome::NeedsMapping(pending)) => {
                self.pending_layout = Some(pending);
            }
            Err(err) => self
                .session
                .push_diagnostic(delog_core::diagnostics::Diag::error(
                    "layout-load",
                    err.to_string(),
                )),
        }
    }

    fn apply_layout(&mut self, layout: LayoutApply) {
        self.workspace = layout.workspace;
        self.view = layout.view;
        self.playback.set_speed(layout.speed as f32);
        self.playback.follow_live = layout.follow_live;
        self.show_legend = layout.show_legend;
        self.vehicles = layout.vehicles;
        self.vehicle_revision = self.vehicle_revision.wrapping_add(1);
        self.traj_dirty = true;
        for diag in layout.diagnostics {
            self.session.push_diagnostic(diag);
        }
    }

    fn show_layout_windows(&mut self, ctx: &egui::Context) {
        if self.save_layout_dialog.open {
            let mut open = self.save_layout_dialog.open;
            egui::Window::new("Save Layout")
                .open(&mut open)
                .default_width(280.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut self.save_layout_dialog.name);
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            let snapshot = self.session.snapshot();
                            self.save_layout(&snapshot);
                            self.save_layout_dialog.open = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.save_layout_dialog.open = false;
                        }
                    });
                });
            self.save_layout_dialog.open &= open;
        }

        if self.load_layout_dialog.open {
            let mut open = self.load_layout_dialog.open;
            egui::Window::new("Load Layout")
                .open(&mut open)
                .default_width(320.0)
                .show(ctx, |ui| {
                    if self.load_layout_dialog.layouts.is_empty() {
                        ui.weak("No saved layouts.");
                    } else {
                        for (i, name) in self.load_layout_dialog.layouts.iter().enumerate() {
                            ui.selectable_value(
                                &mut self.load_layout_dialog.selected,
                                Some(i),
                                name,
                            );
                        }
                    }
                    ui.horizontal(|ui| {
                        let can_load = self.load_layout_dialog.selected.is_some();
                        if ui
                            .add_enabled(can_load, egui::Button::new("Load"))
                            .clicked()
                            && let Some(i) = self.load_layout_dialog.selected
                            && let Some(name) = self.load_layout_dialog.layouts.get(i).cloned()
                        {
                            let snapshot = self.session.snapshot();
                            self.load_layout(&name, &snapshot);
                            self.load_layout_dialog.open = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.load_layout_dialog.open = false;
                        }
                    });
                });
            self.load_layout_dialog.open &= open;
        }

        if let Some(pending) = &mut self.pending_layout {
            let mut apply = false;
            let mut skip = false;
            egui::Window::new("Map Layout Fields")
                .collapsible(false)
                .default_width(440.0)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "{} ambiguous field(s) in `{}`",
                        pending.ambiguity_count(),
                        pending.name
                    ));
                    ui.separator();
                    for ambiguity in pending.ambiguities_mut() {
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "{}.{}",
                                ambiguity.field.topic, ambiguity.field.field
                            ));
                            let selected = ambiguity
                                .candidates
                                .get(ambiguity.selected)
                                .map(|c| c.label.as_str())
                                .unwrap_or("source");
                            egui::ComboBox::from_id_salt((
                                "layout-field-map",
                                &ambiguity.field.topic,
                                &ambiguity.field.field,
                            ))
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for (i, candidate) in ambiguity.candidates.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut ambiguity.selected,
                                        i,
                                        &candidate.label,
                                    );
                                }
                            });
                        });
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            apply = true;
                        }
                        if ui.button("Skip unresolved").clicked() {
                            skip = true;
                        }
                    });
                });
            if apply && let Some(pending) = self.pending_layout.take() {
                let snapshot = self.session.snapshot();
                let layout = pending.apply(&snapshot);
                self.apply_layout(layout);
            }
            if skip && let Some(pending) = self.pending_layout.take() {
                let snapshot = self.session.snapshot();
                let layout = pending.apply_skipping(&snapshot);
                self.apply_layout(layout);
            }
        }
    }
}

impl eframe::App for DelogApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.handle_dropped_files(ui.ctx());
        self.handle_picked_files();
        self.handle_layout_io_results();
        self.session.prune_finished();
        self.poll_trajectory_builds();
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
            let resolved = self.workspace.resolve_ghosts(&snapshot);
            if resolved > 0 {
                self.session
                    .push_diagnostic(delog_core::diagnostics::Diag::info(
                        "layout-bind",
                        format!("bound {resolved} layout trace(s)"),
                    ));
            }
            self.last_epoch = snapshot.epoch;
        }
        self.ensure_trajectory_build(ui.ctx(), &snapshot);
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
                ui.menu_button("Layout", |ui| {
                    if ui.button("Save Layout...").clicked() {
                        self.save_layout_dialog.open = true;
                        ui.close();
                    }
                    if ui.button("Load Layout...").clicked() {
                        self.load_layout_dialog.layouts = crate::layout::list_layouts();
                        self.load_layout_dialog.selected = None;
                        self.load_layout_dialog.open = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Export Layout JSON...").clicked() {
                        self.spawn_export_layout_dialog(ui.ctx(), &snapshot);
                        ui.close();
                    }
                    if ui.button("Import Layout JSON...").clicked() {
                        self.spawn_import_layout_dialog(ui.ctx());
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
                let scene_open = self.workspace.scene_pane_id().is_some();
                let mut scene_button = egui::Button::new("3D");
                if scene_open {
                    scene_button = scene_button.fill(ui.visuals().selection.bg_fill);
                }
                if ui
                    .add(scene_button)
                    .on_hover_text("Show or hide the 3D scene view")
                    .clicked()
                {
                    self.workspace.toggle_scene_pane();
                }
                if ui
                    .button("Vehicles")
                    .on_hover_text("Configure 3D vehicles")
                    .clicked()
                {
                    self.vehicle_dialog.open = !self.vehicle_dialog.open;
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
                let (space, home, end, left, right, save_layout, load_layout) =
                    ui.ctx().input(|i| {
                        (
                            i.key_pressed(egui::Key::Space),
                            i.key_pressed(egui::Key::Home),
                            i.key_pressed(egui::Key::End),
                            i.key_pressed(egui::Key::ArrowLeft),
                            i.key_pressed(egui::Key::ArrowRight),
                            i.modifiers.command && i.key_pressed(egui::Key::S),
                            i.modifiers.command && i.key_pressed(egui::Key::L),
                        )
                    });
                if save_layout {
                    self.save_layout_dialog.open = true;
                }
                if load_layout {
                    self.load_layout_dialog.layouts = crate::layout::list_layouts();
                    self.load_layout_dialog.selected = None;
                    self.load_layout_dialog.open = true;
                }
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
                        metrics: self.session.metrics(),
                        gpu: &mut self.gpu,
                        caches: &mut self.caches,
                        view: &mut self.view,
                        origin_us: self.origin_us,
                        hover_mode: &mut self.hover_mode,
                        show_legend: &mut self.show_legend,
                        playhead_us: snapshot.global_time_range().map(|_| self.playback.t_us),
                        playing: self.playback.playing,
                        vehicles: &self.vehicles,
                        trajectories: &self.vehicle_trajectories,
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
        self.show_layout_windows(ui.ctx());
        if crate::vehicle_dialog::show(
            ui.ctx(),
            &mut self.vehicle_dialog,
            &mut self.vehicles,
            &self.session.snapshot(),
        ) {
            self.vehicle_revision = self.vehicle_revision.wrapping_add(1);
            self.traj_dirty = true;
            self.ensure_trajectory_build(ui.ctx(), &snapshot);
        }
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
