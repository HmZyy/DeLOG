//! Top-level eframe application state.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use delog_cache::CacheManager;
use delog_core::time::TimeRange;

use crate::about;
use crate::browser::{self, BrowserModel};
use crate::gpu::GpuBridge;
use crate::layout::{LayoutApply, LayoutDoc, LayoutError, LoadOutcome, PendingLayout};
use crate::live::ConnectionDialog;
use crate::plot::ViewX;
use crate::session::Session;
use crate::settings::{AppSettings, SettingsDialog};
use crate::timeline::Playback;
use crate::workspace::{PlotServices, Workspace};

struct TrajectoryBuildResult {
    epoch: u64,
    vehicle_revision: u64,
    trajectories: Vec<Vec<[f32; 3]>>,
}

type LayoutImportResult = Result<LayoutDoc, LayoutError>;
type LayoutExportResult = Result<std::path::PathBuf, LayoutError>;
const SESSION_AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);

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

#[derive(Default)]
struct LayoutManagerDialog {
    open: bool,
    layouts: Vec<String>,
    selected: Option<usize>,
    rename_to: String,
    duplicate_to: String,
}

enum LayoutManagerAction {
    Load(String),
    Rename { from: String, to: String },
    Duplicate { from: String, to: String },
    Delete(String),
}

pub struct DelogApp {
    session: Session,
    gpu: GpuBridge,
    caches: CacheManager,
    workspace: Workspace,
    playback: Playback,
    view: Option<ViewX>,
    /// Whether `view` has been fit to real data yet. Stays false while the
    /// session is empty (the view is a pan/zoomable placeholder), so the
    /// first loaded log replaces the placeholder by fitting to its range.
    view_fitted: bool,
    hover_mode: delog_core::field_view::SampleMode,
    frame: u64,
    last_epoch: u64,
    origin_us: i64,
    /// Exponentially-smoothed frame rate for the corner FPS indicator
    /// (PRF-05). Only meaningful while frames are continuous; reads `None`
    /// when the app is idle/event-driven so we don't display a misleading
    /// rate built from a single stale frame (§11 idle policy).
    fps_ema: Option<f32>,
    /// Wall-clock instant of the previous frame, used to measure the real
    /// frame-to-frame gap that feeds `fps_ema`.
    last_frame_at: Option<Instant>,
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
    layout_manager_dialog: LayoutManagerDialog,
    settings: AppSettings,
    settings_dialog: SettingsDialog,
    theme_needs_apply: bool,
    pending_layout: Option<PendingLayout>,
    deferred_layout_doc: Option<LayoutDoc>,
    last_session_autosave: Instant,
    last_session_autosave_json: Option<String>,
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
        let settings = crate::layout::load_session_doc()
            .map(|doc| doc.settings)
            .unwrap_or_default();
        settings.theme.apply(&cc.egui_ctx);
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
            view_fitted: false,
            hover_mode: delog_core::field_view::SampleMode::Prev,
            frame: 0,
            last_epoch: u64::MAX,
            origin_us: 0,
            fps_ema: None,
            last_frame_at: None,
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
            layout_manager_dialog: LayoutManagerDialog::default(),
            settings,
            settings_dialog: SettingsDialog::default(),
            theme_needs_apply: false,
            pending_layout: None,
            deferred_layout_doc: None,
            last_session_autosave: Instant::now(),
            last_session_autosave_json: None,
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
                Ok(doc) => self.apply_layout_doc(doc, &snapshot, "layout-import"),
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

    fn snapshot_has_fields(snapshot: &delog_core::snapshot::StoreSnapshot) -> bool {
        snapshot.fields.iter().any(|field| !field.removed)
    }

    fn apply_layout_doc(
        &mut self,
        doc: LayoutDoc,
        snapshot: &delog_core::snapshot::StoreSnapshot,
        code: &'static str,
    ) {
        let should_defer = !Self::snapshot_has_fields(snapshot);
        match crate::layout::load_doc(doc.clone(), snapshot) {
            Ok(LoadOutcome::Applied(layout)) => {
                self.apply_layout(layout);
                if should_defer {
                    self.deferred_layout_doc = Some(doc);
                    self.session
                        .push_diagnostic(delog_core::diagnostics::Diag::info(
                            "layout-defer",
                            "layout will bind when a log finishes loading",
                        ));
                } else {
                    self.deferred_layout_doc = None;
                }
            }
            Ok(LoadOutcome::NeedsMapping(pending)) => {
                self.deferred_layout_doc = None;
                self.pending_layout = Some(pending);
            }
            Err(err) => self
                .session
                .push_diagnostic(delog_core::diagnostics::Diag::error(code, err.to_string())),
        }
    }

    fn try_apply_deferred_layout(&mut self, snapshot: &delog_core::snapshot::StoreSnapshot) {
        if !Self::snapshot_has_fields(snapshot) {
            return;
        }
        let Some(doc) = self.deferred_layout_doc.take() else {
            return;
        };
        self.apply_layout_doc(doc, snapshot, "layout-bind");
    }

    fn autosave_session(
        &mut self,
        snapshot: &delog_core::snapshot::StoreSnapshot,
        force: bool,
    ) -> Result<bool, LayoutError> {
        if !force && self.last_session_autosave.elapsed() < SESSION_AUTOSAVE_INTERVAL {
            return Ok(false);
        }

        let doc = self.current_layout_doc("session".to_owned(), snapshot);
        let json = crate::layout::doc_json(&doc)?;
        if !force && self.last_session_autosave_json.as_deref() == Some(json.as_str()) {
            self.last_session_autosave = Instant::now();
            return Ok(false);
        }

        crate::layout::save_session_json(&json)?;
        self.last_session_autosave = Instant::now();
        self.last_session_autosave_json = Some(json);
        Ok(true)
    }

    fn maybe_autosave_session(&mut self, snapshot: &delog_core::snapshot::StoreSnapshot) {
        if let Err(err) = self.autosave_session(snapshot, false) {
            self.last_session_autosave = Instant::now();
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::warning(
                    "session-save",
                    err.to_string(),
                ));
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
            vehicles: &self.vehicles,
            settings: &self.settings,
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
        match crate::layout::load_named_doc(name) {
            Ok(doc) => self.apply_layout_doc(doc, snapshot, "layout-load"),
            Err(err) => self
                .session
                .push_diagnostic(delog_core::diagnostics::Diag::error(
                    "layout-load",
                    err.to_string(),
                )),
        }
    }

    fn open_layout_manager(&mut self) {
        self.layout_manager_dialog.open = true;
        self.refresh_layout_manager(None);
    }

    fn refresh_layout_manager(&mut self, preferred: Option<String>) {
        self.layout_manager_dialog.layouts = crate::layout::list_layouts();
        self.layout_manager_dialog.selected = preferred
            .as_deref()
            .and_then(|name| {
                self.layout_manager_dialog
                    .layouts
                    .iter()
                    .position(|candidate| candidate == name)
            })
            .or_else(|| {
                self.layout_manager_dialog
                    .selected
                    .filter(|&i| i < self.layout_manager_dialog.layouts.len())
            });
        if let Some(i) = self.layout_manager_dialog.selected
            && let Some(name) = self.layout_manager_dialog.layouts.get(i)
        {
            self.layout_manager_dialog.rename_to = name.clone();
            self.layout_manager_dialog.duplicate_to = format!("{name}_copy");
        } else {
            self.layout_manager_dialog.rename_to.clear();
            self.layout_manager_dialog.duplicate_to.clear();
        }
    }

    fn apply_layout_manager_action(
        &mut self,
        action: LayoutManagerAction,
        snapshot: &delog_core::snapshot::StoreSnapshot,
    ) {
        match action {
            LayoutManagerAction::Load(name) => self.load_layout(&name, snapshot),
            LayoutManagerAction::Rename { from, to } => {
                let display = to.trim().to_owned();
                match crate::layout::rename_named(&from, &display) {
                    Ok(()) => {
                        self.refresh_layout_manager(Some(display.clone()));
                        self.session
                            .push_diagnostic(delog_core::diagnostics::Diag::info(
                                "layout-manager",
                                format!("renamed layout `{from}` to `{display}`"),
                            ));
                    }
                    Err(err) => self
                        .session
                        .push_diagnostic(delog_core::diagnostics::Diag::error(
                            "layout-manager",
                            err.to_string(),
                        )),
                }
            }
            LayoutManagerAction::Duplicate { from, to } => {
                let display = to.trim().to_owned();
                match crate::layout::duplicate_named(&from, &display) {
                    Ok(()) => {
                        self.refresh_layout_manager(Some(display.clone()));
                        self.session
                            .push_diagnostic(delog_core::diagnostics::Diag::info(
                                "layout-manager",
                                format!("duplicated layout `{from}` to `{display}`"),
                            ));
                    }
                    Err(err) => self
                        .session
                        .push_diagnostic(delog_core::diagnostics::Diag::error(
                            "layout-manager",
                            err.to_string(),
                        )),
                }
            }
            LayoutManagerAction::Delete(name) => match crate::layout::delete_named(&name) {
                Ok(()) => {
                    self.refresh_layout_manager(None);
                    self.session
                        .push_diagnostic(delog_core::diagnostics::Diag::info(
                            "layout-manager",
                            format!("deleted layout `{name}`"),
                        ));
                }
                Err(err) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::error(
                        "layout-manager",
                        err.to_string(),
                    )),
            },
        }
    }

    fn apply_layout(&mut self, layout: LayoutApply) {
        self.workspace = layout.workspace;
        self.view = layout.view;
        // A restored view is authoritative — don't let the data-fit refit
        // overwrite it on the next frame.
        self.view_fitted = layout.view.is_some();
        self.playback.set_speed(layout.speed as f32);
        self.playback.follow_live = layout.follow_live;
        // Legend/tooltip visibility is restored per-pane via the workspace.
        self.vehicles = layout.vehicles;
        if self.settings != layout.settings {
            self.settings = layout.settings;
            self.theme_needs_apply = true;
        }
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

        if self.layout_manager_dialog.open {
            let mut open = self.layout_manager_dialog.open;
            let mut action = None;
            egui::Window::new("Manage Layouts")
                .open(&mut open)
                .default_width(520.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_min_width(180.0);
                            if self.layout_manager_dialog.layouts.is_empty() {
                                ui.weak("No saved layouts.");
                            } else {
                                for (i, name) in
                                    self.layout_manager_dialog.layouts.iter().enumerate()
                                {
                                    if ui
                                        .selectable_label(
                                            self.layout_manager_dialog.selected == Some(i),
                                            name,
                                        )
                                        .clicked()
                                    {
                                        self.layout_manager_dialog.selected = Some(i);
                                        self.layout_manager_dialog.rename_to = name.clone();
                                        self.layout_manager_dialog.duplicate_to =
                                            format!("{name}_copy");
                                    }
                                }
                            }
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            let selected = self
                                .layout_manager_dialog
                                .selected
                                .and_then(|i| self.layout_manager_dialog.layouts.get(i).cloned());
                            let Some(name) = selected else {
                                ui.weak("Select a layout.");
                                return;
                            };

                            ui.strong(&name);
                            if ui.button("Load").clicked() {
                                action = Some(LayoutManagerAction::Load(name.clone()));
                            }
                            ui.separator();
                            ui.label("Rename to");
                            ui.text_edit_singleline(&mut self.layout_manager_dialog.rename_to);
                            let can_rename =
                                !self.layout_manager_dialog.rename_to.trim().is_empty()
                                    && self.layout_manager_dialog.rename_to.trim() != name;
                            if ui
                                .add_enabled(can_rename, egui::Button::new("Rename"))
                                .clicked()
                            {
                                action = Some(LayoutManagerAction::Rename {
                                    from: name.clone(),
                                    to: self.layout_manager_dialog.rename_to.clone(),
                                });
                            }
                            ui.separator();
                            ui.label("Duplicate as");
                            ui.text_edit_singleline(&mut self.layout_manager_dialog.duplicate_to);
                            let can_duplicate =
                                !self.layout_manager_dialog.duplicate_to.trim().is_empty();
                            if ui
                                .add_enabled(can_duplicate, egui::Button::new("Duplicate"))
                                .clicked()
                            {
                                action = Some(LayoutManagerAction::Duplicate {
                                    from: name.clone(),
                                    to: self.layout_manager_dialog.duplicate_to.clone(),
                                });
                            }
                            ui.separator();
                            if ui.button("Delete").clicked() {
                                action = Some(LayoutManagerAction::Delete(name));
                            }
                        });
                    });
                });
            self.layout_manager_dialog.open &= open;
            if let Some(action) = action {
                let snapshot = self.session.snapshot();
                self.apply_layout_manager_action(action, &snapshot);
            }
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
    fn on_exit(&mut self) {
        let snapshot = self.session.snapshot();
        let _ = self.autosave_session(&snapshot, true);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.handle_dropped_files(ui.ctx());
        self.handle_picked_files();
        self.handle_layout_io_results();
        self.session.prune_finished();
        self.poll_trajectory_builds();
        self.frame = self.frame.wrapping_add(1);

        // FPS indicator (PRF-05): measure the real wall-clock gap between
        // frames. While the app renders continuously (playback, live, or any
        // interaction egui repaints for) the gaps are small and we show a
        // smoothed rate. When event-driven and truly idle the next frame is
        // far apart (§11 idle policy) — a rate from that gap is meaningless,
        // so the corner badge reads "idle" instead.
        let now = Instant::now();
        if let Some(prev) = self.last_frame_at.replace(now) {
            let gap = now.duration_since(prev).as_secs_f32();
            // Treat gaps slower than ~5 FPS as idle, not a frame rate.
            if (0.0..0.2).contains(&gap) && gap > 0.0 {
                let inst = 1.0 / gap;
                self.fps_ema = Some(match self.fps_ema {
                    Some(prev) => prev * 0.9 + inst * 0.1,
                    None => inst,
                });
            } else {
                self.fps_ema = None;
            }
        }

        let snapshot = self.session.snapshot();

        // Cache lifecycle: shared origin, frame recency, drain builds, and an
        // epoch-driven incremental append + GC (§8.5).
        if let Some(range) = snapshot.global_time_range() {
            self.origin_us = range.min_us;
            self.caches.set_origin(self.origin_us);
            // Fit the view to the data the first time real data appears,
            // replacing any empty-session placeholder; afterwards the user
            // owns the view (pan/zoom persists).
            if !self.view_fitted {
                self.view = Some(ViewX::from_range(range));
                self.view_fitted = true;
            }

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
        } else {
            // Empty session: a default 0..10 s window so empty plots can be
            // panned and zoomed before any log is loaded.
            self.origin_us = 0;
            self.caches.set_origin(0);
            self.view.get_or_insert(ViewX::new(0, 10_000_000));
        }
        self.caches.begin_frame(self.frame);
        self.caches.poll_builds();
        if snapshot.epoch != self.last_epoch {
            self.caches.on_epoch(&snapshot);
            self.try_apply_deferred_layout(&snapshot);
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
        self.maybe_autosave_session(&snapshot);
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
                ui.menu_button("File", |ui| {
                    if ui
                        .button("Open File")
                        .on_hover_text("Open flight logs (or drop files anywhere)")
                        .clicked()
                    {
                        self.spawn_open_dialog(ui.ctx());
                        ui.close();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Settings...").clicked() {
                        self.settings_dialog.open();
                        ui.close();
                    }
                });
                ui.menu_button("Layout", |ui| {
                    if ui.button("Save Layout...").clicked() {
                        self.save_layout_dialog.open = true;
                        ui.close();
                    }
                    ui.menu_button("Load Layout", |ui| {
                        let layouts = crate::layout::list_layouts();
                        if layouts.is_empty() {
                            ui.add_enabled(false, egui::Button::new("No saved layouts"));
                        } else {
                            for name in layouts {
                                if ui.button(&name).clicked() {
                                    self.load_layout(&name, &snapshot);
                                    ui.close();
                                }
                            }
                        }
                    });
                    if ui.button("Manage Layouts...").clicked() {
                        self.open_layout_manager();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Export JSON...").clicked() {
                        self.spawn_export_layout_dialog(ui.ctx(), &snapshot);
                        ui.close();
                    }
                    if ui.button("Import JSON...").clicked() {
                        self.spawn_import_layout_dialog(ui.ctx());
                        ui.close();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });

                // FPS badge pinned to the far right (PRF-05).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match self.fps_ema {
                        Some(fps) => {
                            // Green >60, orange 30..=60, red <30.
                            let color = if fps > 60.0 {
                                self.settings.theme.success()
                            } else if fps >= 30.0 {
                                self.settings.theme.warning()
                            } else {
                                self.settings.theme.error()
                            };
                            ui.colored_label(color, format!("{fps:.0} FPS"));
                        }
                        None => {
                            ui.weak("idle");
                        }
                    }
                });
            });
        });

        // Icon toolbar directly under the menu bar: streaming + 3D view
        // toggles, plus live/loading status.
        egui::Panel::top("tool_icons").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let streaming = self.session.has_live_links();
                // Blue when a live link is active, neutral otherwise.
                let stream_tint = if streaming {
                    self.settings.theme.accent()
                } else {
                    ui.visuals().weak_text_color()
                };
                if icon_button(
                    ui,
                    "toolbar-stream",
                    crate::icons::satellite_dish(),
                    stream_tint,
                    streaming,
                )
                .on_hover_text("Connect to a MAVLink telemetry stream")
                .clicked()
                {
                    self.show_connection_dialog = true;
                }

                let scene_open = self.workspace.scene_pane_id().is_some();
                let cube_tint = if scene_open {
                    self.settings.theme.accent()
                } else {
                    ui.visuals().weak_text_color()
                };
                if icon_button(
                    ui,
                    "toolbar-3d",
                    crate::icons::cube(),
                    cube_tint,
                    scene_open,
                )
                .on_hover_text("Show or hide the 3D scene view")
                .clicked()
                {
                    self.workspace.toggle_scene_pane();
                }

                let mut disconnect = None;
                for (i, status) in self.session.live_statuses().into_iter().enumerate() {
                    ui.separator();
                    ui.weak(format!(
                        "{} {} · {} frames · {} rows{}",
                        status.state.label(),
                        status.endpoint,
                        status.link.rx_frames,
                        status.ingest.rows,
                        status.recording.as_ref().map(|_| " · rec").unwrap_or("")
                    ));
                    if ui
                        .button("Disconnect")
                        .on_hover_text("Stop this telemetry stream")
                        .clicked()
                    {
                        disconnect = Some(i);
                    }
                }
                if let Some(i) = disconnect {
                    self.session.stop_live(i);
                }

                if self.session.has_active_loads() {
                    ui.separator();
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
                    self.settings.theme,
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
            // The workspace renders even before any log loads, so plots can be
            // arranged and the 3D view opened on an empty session.

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
                        render_tuning: self.settings.render,
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
                    if actions.open_vehicle_config {
                        self.vehicle_dialog.open = true;
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
        let settings_change = self.settings_dialog.show(ui.ctx(), &mut self.settings);
        if settings_change.theme_changed || self.theme_needs_apply {
            self.settings.theme.apply(ui.ctx());
            self.theme_needs_apply = false;
        }
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

/// Fixed footprint of a toolbar icon button. Allocating an explicit size
/// keeps the button's rect independent of the SVG's load state, so the
/// toolbar's height can't change between egui's layout passes (which would
/// otherwise shift every panel below and spam "changed id between passes").
const ICON_BUTTON_SIZE: egui::Vec2 = egui::vec2(28.0, 24.0);

/// A compact toolbar icon button rendering one of the bundled SVG icons.
/// `salt` gives the button a stable id; `tint` colors the (white) glyph;
/// `active` draws a selected background.
fn icon_button(
    ui: &mut egui::Ui,
    salt: &str,
    icon: egui::ImageSource<'_>,
    tint: egui::Color32,
    active: bool,
) -> egui::Response {
    let image = egui::Image::new(icon)
        .fit_to_exact_size(egui::vec2(18.0, 18.0))
        .tint(tint);
    ui.push_id(salt, |ui| {
        ui.add_sized(
            ICON_BUTTON_SIZE,
            egui::Button::image(image).selected(active),
        )
    })
    .inner
}
