use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use delog_cache::CacheManager;
use delog_core::diagnostics::{DiagRecord, Severity};
use delog_core::time::TimeRange;
use egui_extras::{Column, TableBuilder};
use serde::Serialize;

use crate::browser::{self, BrowserFilterCache, BrowserModel};
use crate::diagnostics::DiagnosticsDock;
use crate::docks::{AppDockController, AppDockTab};
use crate::field_stats::{FieldStatsController, StatsRequestKey, StatsTab};
use crate::gpu::GpuBridge;
use crate::layout::{LayoutApply, LayoutDoc, LayoutError, LoadOutcome, PendingLayout};
use crate::live::ConnectionDialog;
use crate::logging::{LogLevel, LogRecord, LoggingDock, PendingLog};
use crate::map::worker::{CacheActionKind, CacheActionStatus, TileManager};
use crate::performance::{PerformanceDock, PerformanceSnapshot, ResourceSummary, TraceSummary};
use crate::plot::ViewX;
#[cfg(feature = "scripting")]
use crate::scripts;
use crate::session::Session;
use crate::settings::{AppSettings, RenderMode, SettingsDialog, TileCacheUiState};

fn tile_cache_needs_repaint(clear_submitted: bool, cache_action_pending: bool) -> bool {
    clear_submitted || cache_action_pending
}
use crate::timeline::Playback;
use crate::workspace::{PlotServices, Workspace};

struct TrajectoryBuildResult {
    epoch: u64,
    vehicle_revision: u64,
    trajectories: Vec<crate::vehicle::VehicleTrajectory>,
}

type LayoutImportResult = Result<LayoutDoc, LayoutError>;
type LayoutExportResult = Result<std::path::PathBuf, LayoutError>;
type DiagnosticsExportResult = Result<std::path::PathBuf, String>;
type ProfilingExportResult = Result<std::path::PathBuf, String>;
type CsvExportResult = Result<(std::path::PathBuf, u64), String>;
const SESSION_AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);
const PERFORMANCE_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const LOG_RETENTION: usize = 1_000;
const EMPTY_SESSION_TIMELINE_RANGE: TimeRange = TimeRange {
    min_us: 0,
    max_us: 10_000_000,
};
const DEFAULT_FIT_VIEW_ALL: bool = true;

struct CombinedLoadState {
    active: bool,
    parser_active: bool,
    native_labels: Vec<String>,
    parser_label: Option<String>,
}

fn combined_load_state(
    native_active: bool,
    native_labels: Vec<String>,
    parser_label: Option<&str>,
) -> CombinedLoadState {
    let parser_label = parser_label
        .filter(|label| !label.is_empty())
        .map(str::to_owned);
    // Drop native labels that duplicate the parser phrase (rendered separately).
    let native_labels = native_labels
        .into_iter()
        .filter(|label| parser_label.as_deref() != Some(label.as_str()))
        .collect();
    CombinedLoadState {
        active: native_active || parser_label.is_some(),
        parser_active: parser_label.is_some(),
        native_labels,
        parser_label,
    }
}

fn should_auto_open_diagnostics(enabled: bool, last_seen: Option<u64>, newest: u64) -> bool {
    enabled && last_seen.is_none_or(|prev| newest > prev)
}

fn timeline_range_for_ui(snapshot_range: Option<TimeRange>) -> TimeRange {
    snapshot_range.unwrap_or(EMPTY_SESSION_TIMELINE_RANGE)
}

#[derive(Serialize)]
struct DiagnosticsExportDoc {
    delog_diagnostics: u32,
    exported_at_unix_ms: u128,
    records: Vec<DiagnosticsExportRecord>,
}

#[derive(Serialize)]
struct DiagnosticsExportRecord {
    seq: u64,
    count: u64,
    severity: &'static str,
    code: &'static str,
    source_id: Option<u32>,
    source_label: Option<String>,
    time_us: Option<i64>,
    byte_offset: Option<u64>,
    message: String,
}

#[derive(Serialize)]
struct ProfilingExportDoc {
    delog_profiling: u32,
    exported_at_unix_ms: u128,
    resources: ProfilingResources,
    metrics: Vec<ProfilingMetric>,
    traces: Vec<ProfilingTrace>,
}

#[derive(Serialize)]
struct ProfilingResources {
    gpu_buffer_count: usize,
    gpu_bytes: u64,
    cache_ready_count: usize,
    cache_cpu_bytes: u64,
}

/// Timers are milliseconds; gauges carry their call-site unit (e.g. bytes).
#[derive(Serialize)]
struct ProfilingMetric {
    name: &'static str,
    last: f32,
    avg: f32,
    min: f32,
    max: f32,
    p99: f32,
    samples: u64,
    counter: u64,
}

#[derive(Serialize)]
struct ProfilingTrace {
    label: String,
    samples: Option<usize>,
    visible_samples: Option<usize>,
    cache_cpu_bytes: u64,
    gpu_bytes: u64,
}

fn profiling_export_doc(
    snapshot: &PerformanceSnapshot,
    exported_at_unix_ms: u128,
) -> ProfilingExportDoc {
    let metrics = snapshot
        .metrics
        .iter()
        .map(|(name, stats)| ProfilingMetric {
            name,
            last: stats.last,
            avg: stats.avg,
            min: stats.min,
            max: stats.max,
            p99: stats.p99,
            samples: stats.n,
            counter: stats.counter,
        })
        .collect();
    let traces = snapshot
        .traces
        .iter()
        .map(|trace| ProfilingTrace {
            label: trace.label.clone(),
            samples: trace.samples,
            visible_samples: trace.visible_samples,
            cache_cpu_bytes: trace.cache_cpu_bytes,
            gpu_bytes: trace.gpu_bytes,
        })
        .collect();
    ProfilingExportDoc {
        delog_profiling: 1,
        exported_at_unix_ms,
        resources: ProfilingResources {
            gpu_buffer_count: snapshot.resources.gpu_buffer_count,
            gpu_bytes: snapshot.resources.gpu_bytes,
            cache_ready_count: snapshot.resources.cache_ready_count,
            cache_cpu_bytes: snapshot.resources.cache_cpu_bytes,
        },
        metrics,
        traces,
    }
}

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
    #[cfg(feature = "scripting")]
    scripts: scripts::ScriptsPanel,
    gpu: GpuBridge,
    caches: CacheManager,
    workspace: Workspace,
    playback: Playback,
    view: Option<ViewX>,
    /// False while the session is empty (view is a pan/zoomable placeholder), so
    /// the first loaded log replaces the placeholder by fitting to its range.
    view_fitted: bool,
    /// When set, every frame pins the X view to the full data range. Disengaged
    /// by manual pan/zoom.
    fit_view_all: bool,
    hover_mode: delog_core::field_view::SampleMode,
    /// Shared measurement-marker time when the marker scope is Global. Per-pane
    /// markers live on the pane.
    marker_us: Option<i64>,
    markers: crate::markers::Markers,
    snap_playhead: bool,
    frame: u64,
    last_epoch: u64,
    origin_us: i64,
    /// `None` when idle/event-driven, so the badge doesn't show a misleading
    /// rate built from a single stale frame.
    fps_ema: Option<f32>,
    last_frame_at: Option<Instant>,
    /// Picked on a worker thread — the dialog must never block the UI thread.
    picked_files: mpsc::Receiver<Vec<std::path::PathBuf>>,
    picked_files_tx: mpsc::Sender<Vec<std::path::PathBuf>>,
    imported_layouts: mpsc::Receiver<LayoutImportResult>,
    imported_layouts_tx: mpsc::Sender<LayoutImportResult>,
    exported_layouts: mpsc::Receiver<LayoutExportResult>,
    exported_layouts_tx: mpsc::Sender<LayoutExportResult>,
    exported_diagnostics: mpsc::Receiver<DiagnosticsExportResult>,
    exported_diagnostics_tx: mpsc::Sender<DiagnosticsExportResult>,
    exported_kml: mpsc::Receiver<Result<String, String>>,
    exported_kml_tx: mpsc::Sender<Result<String, String>>,
    message_popups: Vec<crate::message_popup::MessagePopup>,
    exported_profiling: mpsc::Receiver<ProfilingExportResult>,
    exported_profiling_tx: mpsc::Sender<ProfilingExportResult>,
    csv_export: crate::csv_export::CsvExportState,
    csv_export_tx: mpsc::Sender<CsvExportResult>,
    csv_export_rx: mpsc::Receiver<CsvExportResult>,
    csv_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    image_export_writes: mpsc::Receiver<crate::image_export::PngWriteRequest>,
    image_export_writes_tx: mpsc::Sender<crate::image_export::PngWriteRequest>,
    pending_image_capture: Option<crate::image_export::PendingImageCapture>,
    queued_image_capture: Option<crate::image_export::ImageCaptureIntent>,
    next_image_capture_id: u64,
    image_clipboard: Option<arboard::Clipboard>,
    browser_collapsed: bool,
    docks: AppDockController,
    diagnostics_dock: DiagnosticsDock,
    last_diagnostic_seq: Option<u64>,
    logging_dock: LoggingDock,
    logs: Vec<LogRecord>,
    next_log_seq: u64,
    log_started_at: Instant,
    performance_dock: PerformanceDock,
    markers_dock: crate::markers::MarkersDock,
    performance_snapshot: PerformanceSnapshot,
    performance_last_refresh: Option<Instant>,
    browser_query: String,
    browser_filter: BrowserFilterCache,
    browser_selection: browser::Selection,
    /// Keyed by snapshot epoch so the O(topics×fields) tree rebuild runs once
    /// per data change, not every frame.
    browser_model: Option<(u64, BrowserModel)>,
    offset_dialog: Option<(delog_core::identity::SourceId, i64)>,
    source_metadata_dialog: Option<delog_core::identity::SourceId>,
    field_metadata_dialog: Option<delog_core::identity::FieldId>,
    field_stats: FieldStatsController,
    generate_markers_dialog: Option<crate::generate_markers::GenerateMarkersDialog>,
    save_layout_dialog: SaveLayoutDialog,
    load_layout_dialog: LoadLayoutDialog,
    layout_manager_dialog: LayoutManagerDialog,
    settings: AppSettings,
    settings_dialog: SettingsDialog,
    tile_manager: Option<TileManager>,
    tile_manager_error: Option<String>,
    theme_needs_apply: bool,
    pending_layout: Option<PendingLayout>,
    deferred_layout_doc: Option<LayoutDoc>,
    last_session_autosave: Instant,
    last_session_autosave_json: Option<String>,
    show_connection_dialog: bool,
    connection_dialog: ConnectionDialog,
    vehicles: Vec<crate::vehicle::VehicleConfig>,
    vehicle_dialog: crate::vehicle_dialog::VehicleDialog,
    /// Parallel to `vehicles`, rebuilt on a worker when the data epoch or
    /// vehicle set changes.
    vehicle_trajectories: Vec<crate::vehicle::VehicleTrajectory>,
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
        let settings = crate::layout::load_app_settings();
        settings.theme.apply(&cc.egui_ctx);
        settings.font.apply(&cc.egui_ctx);
        let connection_dialog = ConnectionDialog::from_settings(&settings.live_connection);
        let (tile_manager, tile_manager_error) =
            match directories::ProjectDirs::from("org", "hmzyy", "DeLOG") {
                Some(dirs) => {
                    let cache_dir = dirs.cache_dir().join("map-tiles");
                    let repaint = cc.egui_ctx.clone();
                    match TileManager::new(
                        cache_dir,
                        settings.scene3d.tile_cache_limit_bytes,
                        move || repaint.request_repaint(),
                    ) {
                        Ok(manager) => (Some(manager), None),
                        Err(error) => {
                            tracing::warn!(%error, "map tile cache unavailable");
                            (None, Some(error.to_string()))
                        }
                    }
                }
                None => (None, Some("cache directory unavailable".to_owned())),
            };
        let (picked_files_tx, picked_files) = mpsc::channel();
        let (traj_results_tx, traj_results) = mpsc::channel();
        let (imported_layouts_tx, imported_layouts) = mpsc::channel();
        let (exported_layouts_tx, exported_layouts) = mpsc::channel();
        let (exported_diagnostics_tx, exported_diagnostics) = mpsc::channel();
        let (exported_kml_tx, exported_kml) = mpsc::channel();
        let (exported_profiling_tx, exported_profiling) = mpsc::channel();
        let (csv_export_tx, csv_export_rx) = mpsc::channel();
        let (image_export_writes_tx, image_export_writes) = mpsc::channel();
        let session = Session::new(cc.egui_ctx.clone());
        // Shared metrics registry so cache metrics land in the same dock.
        let caches = CacheManager::new().with_metrics(std::sync::Arc::clone(session.metrics()));
        Self {
            session,
            #[cfg(feature = "scripting")]
            scripts: {
                let config_dir =
                    crate::layout::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                scripts::ScriptsPanel::new(
                    config_dir.join("scripts"),
                    config_dir.join("parsers"),
                    config_dir.join("script_params.json"),
                )
            },
            gpu: GpuBridge::from_creation_context(cc),
            caches,
            workspace: Workspace::new(),
            playback: Playback::default(),
            view: None,
            view_fitted: false,
            fit_view_all: DEFAULT_FIT_VIEW_ALL,
            hover_mode: delog_core::field_view::SampleMode::Prev,
            marker_us: None,
            markers: crate::markers::Markers::new(),
            snap_playhead: false,
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
            exported_diagnostics,
            exported_diagnostics_tx,
            exported_kml,
            exported_kml_tx,
            message_popups: Vec::new(),
            exported_profiling,
            exported_profiling_tx,
            csv_export: crate::csv_export::CsvExportState::default(),
            csv_export_tx,
            csv_export_rx,
            csv_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            image_export_writes,
            image_export_writes_tx,
            pending_image_capture: None,
            queued_image_capture: None,
            next_image_capture_id: 1,
            image_clipboard: None,
            browser_collapsed: false,
            docks: AppDockController::new_empty(),
            diagnostics_dock: DiagnosticsDock::default(),
            last_diagnostic_seq: None,
            logging_dock: LoggingDock::default(),
            logs: Vec::new(),
            next_log_seq: 0,
            log_started_at: Instant::now(),
            performance_dock: PerformanceDock::default(),
            markers_dock: crate::markers::MarkersDock::default(),
            performance_snapshot: PerformanceSnapshot::default(),
            performance_last_refresh: None,
            browser_query: String::new(),
            browser_filter: BrowserFilterCache::default(),
            browser_selection: browser::Selection::default(),
            browser_model: None,
            offset_dialog: None,
            source_metadata_dialog: None,
            field_metadata_dialog: None,
            field_stats: FieldStatsController::default(),
            generate_markers_dialog: None,
            save_layout_dialog: SaveLayoutDialog {
                open: false,
                name: "default".into(),
            },
            load_layout_dialog: LoadLayoutDialog::default(),
            layout_manager_dialog: LayoutManagerDialog::default(),
            settings,
            settings_dialog: SettingsDialog::default(),
            tile_manager,
            tile_manager_error,
            theme_needs_apply: false,
            pending_layout: None,
            deferred_layout_doc: None,
            last_session_autosave: Instant::now(),
            last_session_autosave_json: None,
            show_connection_dialog: false,
            connection_dialog,
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

    fn dock_open_checkbox(&mut self, ui: &mut egui::Ui, tab: AppDockTab, label: &str) {
        let mut open = self.docks.is_open(tab);
        if ui.checkbox(&mut open, label).clicked() {
            if open {
                self.open_dock(tab);
            } else {
                self.close_dock(tab);
            }
            ui.close();
        }
    }

    fn open_dock(&mut self, tab: AppDockTab) {
        self.set_legacy_dock_open(tab, true);
        self.docks.open_or_focus(tab);
    }

    fn close_dock(&mut self, tab: AppDockTab) {
        self.set_legacy_dock_open(tab, false);
        self.docks.close(tab);
    }

    fn set_legacy_dock_open(&mut self, tab: AppDockTab, open: bool) {
        match tab {
            AppDockTab::Diagnostics => self.diagnostics_dock.open = open,
            AppDockTab::Performance => self.performance_dock.open = open,
            AppDockTab::Markers => self.markers_dock.open = open,
            #[cfg(feature = "scripting")]
            AppDockTab::ScriptingConsole => self.scripts.set_console_open(open),
            AppDockTab::Logging => self.logging_dock.open = open,
        }
    }

    fn sync_dock_from_legacy_flag(&mut self, tab: AppDockTab, open: bool) {
        if open {
            if !self.docks.is_open(tab) {
                self.docks.open_or_focus(tab);
            }
        } else if self.docks.is_open(tab) {
            self.docks.close(tab);
        }
    }

    fn sync_docks_from_legacy_flags(&mut self) {
        self.sync_dock_from_legacy_flag(AppDockTab::Diagnostics, self.diagnostics_dock.open);
        self.sync_dock_from_legacy_flag(AppDockTab::Performance, self.performance_dock.open);
        self.sync_dock_from_legacy_flag(AppDockTab::Markers, self.markers_dock.open);
        #[cfg(feature = "scripting")]
        self.sync_dock_from_legacy_flag(AppDockTab::ScriptingConsole, self.scripts.console_open);
        self.sync_dock_from_legacy_flag(AppDockTab::Logging, self.logging_dock.open);
    }

    fn sync_legacy_dock_flag_from_state(&mut self, tab: AppDockTab) {
        self.set_legacy_dock_open(tab, self.docks.is_open(tab));
    }

    fn sync_legacy_dock_flags_from_state(&mut self) {
        self.sync_legacy_dock_flag_from_state(AppDockTab::Diagnostics);
        self.sync_legacy_dock_flag_from_state(AppDockTab::Performance);
        self.sync_legacy_dock_flag_from_state(AppDockTab::Markers);
        #[cfg(feature = "scripting")]
        self.sync_legacy_dock_flag_from_state(AppDockTab::ScriptingConsole);
        self.sync_legacy_dock_flag_from_state(AppDockTab::Logging);
    }

    /// On a worker thread so the native dialog never blocks the UI.
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

    fn handle_picked_files(&mut self) {
        while let Ok(paths) = self.picked_files.try_recv() {
            for path in paths {
                self.session.open_path(path);
            }
        }
    }

    fn spawn_png_export_dialog(
        &self,
        ctx: &egui::Context,
        kind: crate::image_export::ImageCaptureKind,
        png_bytes: Vec<u8>,
    ) {
        let tx = self.image_export_writes_tx.clone();
        let ctx = ctx.clone();
        let file_name = match kind {
            crate::image_export::ImageCaptureKind::Workspace => "workspace.png",
            crate::image_export::ImageCaptureKind::Plot => "plot.png",
        };
        std::thread::Builder::new()
            .name("delog-image-export-dialog".into())
            .spawn(move || {
                let picked = rfd::FileDialog::new()
                    .add_filter("PNG image", &["png"])
                    .set_file_name(file_name)
                    .set_title("Export PNG")
                    .save_file();
                if let Some(path) = picked {
                    let _ = tx.send(crate::image_export::PngWriteRequest::new(path, png_bytes));
                    ctx.request_repaint();
                }
            })
            .expect("spawn image export dialog thread");
    }

    fn handle_image_export_writes(&mut self) {
        while let Ok(request) = self.image_export_writes.try_recv() {
            match std::fs::write(&request.path, request.png_bytes) {
                Ok(()) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::info(
                        "image-export",
                        format!("exported image to {}", request.path.display()),
                    )),
                Err(err) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::error(
                        "image-export",
                        err.to_string(),
                    )),
            }
        }
    }

    fn copy_captured_image_to_clipboard(
        &mut self,
        kind: crate::image_export::ImageCaptureKind,
        image: &egui::ColorImage,
    ) {
        let what = match kind {
            crate::image_export::ImageCaptureKind::Workspace => "workspace",
            crate::image_export::ImageCaptureKind::Plot => "plot",
        };

        if self.image_clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(clipboard) => self.image_clipboard = Some(clipboard),
                Err(err) => {
                    self.session
                        .push_diagnostic(delog_core::diagnostics::Diag::error(
                            "image-copy",
                            format!("failed to initialize clipboard: {err}"),
                        ));
                    return;
                }
            }
        }

        let Some(clipboard) = self.image_clipboard.as_mut() else {
            return;
        };
        match crate::image_export::copy_image_to_clipboard(clipboard, image) {
            Ok(()) => self
                .session
                .push_diagnostic(delog_core::diagnostics::Diag::info(
                    "image-copy",
                    format!(
                        "copied {what} image to clipboard via arboard ({}x{})",
                        image.size[0], image.size[1]
                    ),
                )),
            Err(err) => {
                self.image_clipboard = None;
                self.session
                    .push_diagnostic(delog_core::diagnostics::Diag::error(
                        "image-copy",
                        format!("failed to copy {what} image to clipboard: {err}"),
                    ));
            }
        }
    }

    fn queue_image_capture(
        &mut self,
        ctx: &egui::Context,
        intent: crate::image_export::ImageCaptureIntent,
    ) {
        if self.pending_image_capture.is_some() || self.queued_image_capture.is_some() {
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::warning(
                    "image-export",
                    "image capture already in progress",
                ));
            return;
        }
        self.queued_image_capture = Some(intent);
        ctx.request_repaint();
    }

    fn start_queued_image_capture(
        &mut self,
        ctx: &egui::Context,
        workspace_rect: Option<egui::Rect>,
    ) {
        let Some(intent) = self.queued_image_capture.take() else {
            return;
        };
        if !intent.is_ready(self.frame) || self.pending_image_capture.is_some() {
            self.queued_image_capture = Some(intent);
            ctx.request_repaint();
            return;
        }
        let Some(rect) = intent.resolve_rect(workspace_rect) else {
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::warning(
                    "image-export",
                    "workspace is not ready to export",
                ));
            return;
        };
        self.request_image_capture(
            ctx,
            intent.action,
            intent.kind,
            rect,
            ctx.pixels_per_point(),
        );
    }

    fn request_image_capture(
        &mut self,
        ctx: &egui::Context,
        action: crate::image_export::ImageCaptureAction,
        kind: crate::image_export::ImageCaptureKind,
        rect: egui::Rect,
        pixels_per_point: f32,
    ) {
        if self.pending_image_capture.is_some() {
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::warning(
                    "image-export",
                    "image capture already in progress",
                ));
            return;
        }
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::warning(
                    "image-export",
                    "nothing to capture",
                ));
            return;
        }
        let id = self.next_image_capture_id;
        self.next_image_capture_id = self.next_image_capture_id.wrapping_add(1).max(1);
        self.pending_image_capture = Some(crate::image_export::PendingImageCapture {
            id,
            action,
            kind,
            rect,
            pixels_per_point,
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(id)));
        ctx.request_repaint();
    }

    fn handle_image_screenshot_events(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|input| input.events.clone());
        for event in events {
            let egui::Event::Screenshot {
                user_data, image, ..
            } = event
            else {
                continue;
            };
            let Some(id) = crate::image_export::screenshot_request_id(&user_data) else {
                continue;
            };
            if self
                .pending_image_capture
                .as_ref()
                .map(|pending| pending.id)
                != Some(id)
            {
                continue;
            }
            let Some(pending) = self.pending_image_capture.take() else {
                continue;
            };
            let Some(cropped) = crate::image_export::crop_color_image(
                &image,
                pending.rect,
                pending.pixels_per_point,
            ) else {
                self.session
                    .push_diagnostic(delog_core::diagnostics::Diag::error(
                        "image-export",
                        "captured image did not overlap the requested area",
                    ));
                continue;
            };

            match pending.action {
                crate::image_export::ImageCaptureAction::Copy => {
                    self.copy_captured_image_to_clipboard(pending.kind, &cropped);
                }
                crate::image_export::ImageCaptureAction::Export => {
                    match crate::image_export::encode_png(&cropped) {
                        Ok(png_bytes) => self.spawn_png_export_dialog(ctx, pending.kind, png_bytes),
                        Err(err) => {
                            self.session
                                .push_diagnostic(delog_core::diagnostics::Diag::error(
                                    "image-export",
                                    err.to_string(),
                                ))
                        }
                    }
                }
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

        while let Ok(result) = self.exported_diagnostics.try_recv() {
            match result {
                Ok(path) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::info(
                        "diagnostics-export",
                        format!("exported diagnostics to {}", path.display()),
                    )),
                Err(err) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::error(
                        "diagnostics-export",
                        err,
                    )),
            }
        }

        while let Ok(result) = self.exported_kml.try_recv() {
            match result {
                Ok(msg) => {
                    self.push_log(PendingLog::with_target(
                        LogLevel::Info,
                        "kml-export",
                        msg.clone(),
                    ));
                    self.message_popups
                        .push(crate::message_popup::MessagePopup::info(
                            "Export trajectories KML",
                            msg,
                        ));
                }
                Err(err) => {
                    self.push_log(PendingLog::with_target(
                        LogLevel::Error,
                        "kml-export",
                        err.clone(),
                    ));
                    self.message_popups
                        .push(crate::message_popup::MessagePopup::error(
                            "Export trajectories KML",
                            err,
                        ));
                }
            }
        }

        while let Ok(result) = self.exported_profiling.try_recv() {
            match result {
                Ok(path) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::info(
                        "profiling-export",
                        format!("exported profiling snapshot to {}", path.display()),
                    )),
                Err(err) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::error(
                        "profiling-export",
                        err,
                    )),
            }
        }

        while let Ok(result) = self.csv_export_rx.try_recv() {
            match result {
                Ok((path, rows)) => {
                    self.session
                        .push_diagnostic(delog_core::diagnostics::Diag::info(
                            "csv-export",
                            format!("exported {rows} rows to {}", path.display()),
                        ))
                }
                Err(err) => self
                    .session
                    .push_diagnostic(delog_core::diagnostics::Diag::error("csv-export", err)),
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

    fn clear_current_layout(&mut self) {
        Self::clear_current_layout_state(
            &mut self.workspace,
            &mut self.playback,
            &mut self.view,
            &mut self.view_fitted,
            &mut self.fit_view_all,
            &mut self.marker_us,
            &mut self.markers,
            &mut self.vehicles,
            &mut self.vehicle_dialog,
            &mut self.vehicle_revision,
            &mut self.traj_dirty,
        );
        self.pending_layout = None;
        self.deferred_layout_doc = None;
        self.traj_building = None;
        self.vehicle_trajectories.clear();
    }

    #[allow(clippy::too_many_arguments)]
    fn clear_current_layout_state(
        workspace: &mut Workspace,
        playback: &mut Playback,
        view: &mut Option<ViewX>,
        view_fitted: &mut bool,
        fit_view_all: &mut bool,
        marker_us: &mut Option<i64>,
        markers: &mut crate::markers::Markers,
        vehicles: &mut Vec<crate::vehicle::VehicleConfig>,
        vehicle_dialog: &mut crate::vehicle_dialog::VehicleDialog,
        vehicle_revision: &mut u64,
        traj_dirty: &mut bool,
    ) {
        *workspace = Workspace::new();
        playback.speed = 1.0;
        playback.follow_live = false;
        *view = None;
        *view_fitted = false;
        *fit_view_all = DEFAULT_FIT_VIEW_ALL;
        *marker_us = None;
        *markers = crate::markers::Markers::new();
        vehicles.clear();
        *vehicle_dialog = crate::vehicle_dialog::VehicleDialog::default();
        *vehicle_revision = vehicle_revision.wrapping_add(1);
        *traj_dirty = true;
    }

    fn refresh_performance_snapshot(
        &mut self,
        frame: &eframe::Frame,
        snapshot: &delog_core::snapshot::StoreSnapshot,
    ) {
        if !self.performance_dock.open {
            return;
        }
        let now = Instant::now();
        if self
            .performance_last_refresh
            .is_some_and(|last| now.duration_since(last) < PERFORMANCE_REFRESH_INTERVAL)
        {
            return;
        }

        self.performance_snapshot = self.build_performance_snapshot(frame, snapshot);
        self.performance_last_refresh = Some(now);
    }

    fn build_performance_snapshot(
        &self,
        frame: &eframe::Frame,
        snapshot: &delog_core::snapshot::StoreSnapshot,
    ) -> PerformanceSnapshot {
        let view = self.view;
        let traces = self
            .workspace
            .fields()
            .map(|field| {
                let visible_samples = view.and_then(|view| {
                    let (x0, x1) = view.seconds(self.origin_us);
                    self.caches.field_visible_samples(field, x0, x1)
                });
                TraceSummary {
                    label: crate::legend::trace_label(snapshot, field),
                    samples: self.caches.field_samples(field),
                    visible_samples,
                    cache_cpu_bytes: self.caches.field_mem(field).cache_cpu,
                    gpu_bytes: self.gpu.field_gpu_bytes(frame, field),
                }
            })
            .collect();
        let gpu = self.gpu.summary(frame);
        PerformanceSnapshot {
            metrics: self.session.metrics().snapshot(),
            resources: ResourceSummary {
                gpu_buffer_count: gpu.buffer_count,
                gpu_bytes: gpu.gpu_bytes,
                cache_ready_count: self.caches.ready_count(),
                cache_cpu_bytes: self.caches.total_cache_bytes(),
            },
            traces,
        }
    }

    fn push_log(&mut self, pending: PendingLog) {
        self.logs.push(LogRecord {
            seq: self.next_log_seq,
            elapsed_ms: self.log_started_at.elapsed().as_millis(),
            level: pending.level,
            target: pending.target,
            message: pending.message,
        });
        self.next_log_seq = self.next_log_seq.wrapping_add(1);
        let excess = self.logs.len().saturating_sub(LOG_RETENTION);
        if excess > 0 {
            self.logs.drain(0..excess);
        }
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
            speed: self.playback.speed as f64,
            follow_live: self.playback.follow_live,
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

    fn spawn_export_diagnostics_dialog(
        &self,
        ctx: &egui::Context,
        records: Vec<DiagRecord>,
        snapshot: &delog_core::snapshot::StoreSnapshot,
    ) {
        let doc = diagnostics_export_doc(records, snapshot);
        let tx = self.exported_diagnostics_tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("delog-diagnostics-export-dialog".into())
            .spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("DeLOG diagnostics", &["json"])
                    .add_filter("All files", &["*"])
                    .set_title("Export diagnostics JSON")
                    .set_file_name("diagnostics.json")
                    .save_file()
                {
                    let result = serde_json::to_vec_pretty(&doc)
                        .map_err(|err| err.to_string())
                        .and_then(|json| std::fs::write(&path, json).map_err(|err| err.to_string()))
                        .map(|_| path);
                    let _ = tx.send(result);
                    ctx.request_repaint();
                }
            })
            .expect("spawn diagnostics export dialog thread");
    }

    /// KML is built on the UI thread (needs snapshot + vehicle state); only the
    /// file dialog and write run on the worker. Results flow back through
    /// `exported_kml` and surface as a diagnostic plus a message popup.
    fn spawn_export_kml_dialog(
        &self,
        ctx: &egui::Context,
        snapshot: &delog_core::snapshot::StoreSnapshot,
    ) {
        let export =
            crate::kml_export::build_kml(snapshot, &self.vehicles, &self.vehicle_trajectories);
        if export.exported == 0 {
            let _ = self
                .exported_kml_tx
                .send(Err("no georeferenced trajectories to export".into()));
            ctx.request_repaint();
            return;
        }
        let noun = if export.exported == 1 {
            "trajectory"
        } else {
            "trajectories"
        };
        let summary = if export.skipped.is_empty() {
            format!("exported {} vehicle {noun}", export.exported)
        } else {
            format!(
                "exported {} vehicle {noun}, skipped {} (no geo reference): {}",
                export.exported,
                export.skipped.len(),
                export.skipped.join(", ")
            )
        };
        let xml = export.xml;
        let tx = self.exported_kml_tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("delog-kml-export-dialog".into())
            .spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("KML", &["kml"])
                    .add_filter("All files", &["*"])
                    .set_title("Export trajectories KML")
                    .set_file_name("trajectories.kml")
                    .save_file()
                {
                    let result = std::fs::write(&path, xml.as_bytes())
                        .map(|_| format!("{summary} to {}", path.display()))
                        .map_err(|err| format!("failed to write {}: {err}", path.display()));
                    let _ = tx.send(result);
                    ctx.request_repaint();
                }
            })
            .expect("spawn kml export dialog thread");
    }

    /// Export the current profiling snapshot (metric rings + resources + traces)
    /// to JSON off the UI thread. The doc is built on the UI
    /// thread (it needs the wgpu frame for GPU stats); only the file dialog and
    /// write run on the worker.
    fn spawn_export_profiling_dialog(
        &self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        snapshot: &delog_core::snapshot::StoreSnapshot,
    ) {
        let exported_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let doc = profiling_export_doc(
            &self.build_performance_snapshot(frame, snapshot),
            exported_at_unix_ms,
        );
        let tx = self.exported_profiling_tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("delog-profiling-export-dialog".into())
            .spawn(move || {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("DeLOG profiling", &["json"])
                    .add_filter("All files", &["*"])
                    .set_title("Export profiling snapshot JSON")
                    .set_file_name("profiling.json")
                    .save_file()
                {
                    let result = serde_json::to_vec_pretty(&doc)
                        .map_err(|err| err.to_string())
                        .and_then(|json| std::fs::write(&path, json).map_err(|err| err.to_string()))
                        .map(|_| path);
                    let _ = tx.send(result);
                    ctx.request_repaint();
                }
            })
            .expect("spawn profiling export dialog thread");
    }

    fn spawn_csv_export(
        &mut self,
        ctx: &egui::Context,
        snapshot: &std::sync::Arc<delog_core::snapshot::StoreSnapshot>,
        all_fields: &[crate::csv_export::CsvField],
        req: crate::csv_export::CsvExportRequest,
    ) {
        use std::sync::atomic::Ordering;
        let chosen: Vec<crate::csv_export::CsvField> = req
            .fields
            .iter()
            .filter_map(|id| all_fields.iter().find(|f| f.id == *id))
            .map(|f| crate::csv_export::CsvField {
                id: f.id,
                label: f.label.clone(),
                unit: f.unit.clone(),
            })
            .collect();
        let origin_us = snapshot.global_time_range().map(|r| r.min_us).unwrap_or(0);
        let snapshot = std::sync::Arc::clone(snapshot);
        let tx = self.csv_export_tx.clone();
        let ctx = ctx.clone();
        self.csv_cancel.store(false, Ordering::Relaxed);
        let cancel = std::sync::Arc::clone(&self.csv_cancel);
        let mode = req.mode;
        let window = req.window;
        std::thread::Builder::new()
            .name("delog-csv-export".into())
            .spawn(move || {
                let picked = rfd::FileDialog::new()
                    .add_filter("CSV", &["csv"])
                    .add_filter("All files", &["*"])
                    .set_title("Export CSV")
                    .set_file_name("export.csv")
                    .save_file();
                let Some(path) = picked else { return };
                let result = (|| {
                    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                    let mut w = std::io::BufWriter::new(file);
                    let rows = crate::csv_export::write_csv(
                        &mut w,
                        &snapshot,
                        &chosen,
                        window,
                        mode,
                        origin_us,
                        &cancel,
                        |_frac| {},
                    )
                    .map_err(|e| e.to_string())?;
                    use std::io::Write;
                    w.flush().map_err(|e| e.to_string())?;
                    Ok::<_, String>((path, rows))
                })();
                let _ = tx.send(result);
                ctx.request_repaint();
            })
            .expect("spawn csv export thread");
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
        self.view = None;
        self.view_fitted = false;
        self.fit_view_all = layout.fit_all;
        self.playback.set_speed(layout.speed as f32);
        self.playback.follow_live = layout.follow_live;
        // Legend/tooltip visibility is restored per-pane via the workspace.
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
                .collapsible(false)
                .default_pos(ctx.content_rect().center())
                .pivot(egui::Align2::CENTER_CENTER)
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
                .collapsible(false)
                .default_pos(ctx.content_rect().center())
                .pivot(egui::Align2::CENTER_CENTER)
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
                .collapsible(false)
                .default_pos(ctx.content_rect().center())
                .pivot(egui::Align2::CENTER_CENTER)
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
                .default_pos(ctx.content_rect().center())
                .pivot(egui::Align2::CENTER_CENTER)
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
        let _ = crate::layout::save_app_settings(&self.settings);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Whole-frame CPU time (`frame_total`); drops at function end.
        let _frame_timer = self.session.metrics().scope("frame_total");
        // Apply the global font override before any widget is laid out so a
        // changed size/family takes effect this frame.
        self.settings.font.apply(ui.ctx());
        self.handle_image_screenshot_events(ui.ctx());
        self.handle_image_export_writes();
        // Pre-UI bookkeeping: picked files, job pruning,
        // cache lifecycle + epoch handling, trajectory builds and autosave —
        // none of it inside a panel scope. `ui_prelude` captures this block so
        // `frame_total − Σ(ui_*)` no longer hides it as an unattributed gap.
        let ui_prelude_timer = self.session.metrics().scope("ui_prelude");
        self.handle_picked_files();
        self.handle_layout_io_results();
        self.session.prune_finished();
        self.poll_trajectory_builds();
        self.frame = self.frame.wrapping_add(1);

        // When event-driven and idle, the gap to the next frame is large and a
        // rate computed from it is meaningless, so the badge reads "idle".
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

        // `global_range` is O(total chunks across all topics) and called
        // several times per frame, so it is timed in isolation to quantify a
        // suspected cross-cutting cost as chunks accumulate during live.
        let global_range_timer = self.session.metrics().scope("global_range");
        let global_range = snapshot.global_time_range();
        drop(global_range_timer);
        if let Some(range) = global_range {
            self.origin_us = range.min_us;
            self.caches.set_origin(self.origin_us);
            // Fit the view to the data the first time real data appears,
            // replacing any empty-session placeholder; afterwards the user
            // owns the view (pan/zoom persists).
            if !self.view_fitted {
                self.view = Some(ViewX::from_range(range));
                self.view_fitted = true;
            }

            // Advance the playhead — the single time authority.
            let dt = ui.ctx().input(|i| i.stable_dt) as f64;
            self.playback.clamp_to(range);
            self.playback.advance(dt, range);
            if self.fit_view_all {
                self.view = Some(ViewX::from_range(range));
            } else if self.session.has_live_links() && self.playback.follow_live {
                self.pin_view_to_live(range);
            }

            // Idle-aware repaint: keep frames continuous only while playing or
            // a live link is connected; otherwise stay event-driven.
            if self.playback.playing || self.session.has_connected_live() {
                ui.ctx().request_repaint();
            }
        } else {
            // Empty session: a default 0..10 s window so empty plots can be
            // panned and zoomed before any log is loaded.
            self.origin_us = 0;
            self.caches.set_origin(0);
            self.view
                .get_or_insert(ViewX::from_range(EMPTY_SESSION_TIMELINE_RANGE));
        }
        if self.settings.render_mode == RenderMode::Continuous {
            ui.ctx().request_repaint();
        }
        self.caches.begin_frame(self.frame);
        for field in self.caches.poll_builds() {
            let label = snapshot
                .fields
                .get(field.index())
                .filter(|entry| entry.id == field)
                .map(|entry| {
                    snapshot
                        .topic(entry.topic)
                        .map(|topic| format!("{}.{}", topic.entry.name, entry.name))
                        .unwrap_or_else(|| entry.name.clone())
                })
                .unwrap_or_else(|| format!("field {}", field.0));
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::warning(
                    "cache-empty",
                    format!("could not build render cache for {label}"),
                ));
        }
        if snapshot.epoch != self.last_epoch {
            self.caches.on_epoch(&snapshot);
            self.try_apply_deferred_layout(&snapshot);
            for field in self.workspace.prune_removed_fields(&snapshot) {
                self.caches.unpin(field);
            }
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
        for field in self.workspace.fields().collect::<Vec<_>>() {
            self.caches.request(field, &snapshot);
        }
        self.caches.evict_over_budget();

        for message in self.gpu.drain_gpu_errors(frame) {
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::error("gpu", message));
        }

        drop(ui_prelude_timer);

        // Per-section UI-thread timers: `frame_total` minus the
        // sum of these scopes is egui's own tessellation/bookkeeping, so the
        // breakdown attributes the frame to the panel that actually costs it.
        let ui_menu_timer = self.session.metrics().scope("ui_menu");
        egui::Panel::top("main_menu").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open File").clicked() {
                        self.spawn_open_dialog(ui.ctx());
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Export Diagnostics JSON...").clicked() {
                        self.spawn_export_diagnostics_dialog(
                            ui.ctx(),
                            self.session.diagnostic_records(),
                            &snapshot,
                        );
                        ui.close();
                    }
                    if ui.button("Export Profiling JSON...").clicked() {
                        self.spawn_export_profiling_dialog(ui.ctx(), frame, &snapshot);
                        ui.close();
                    }
                    if ui.button("Export CSV...").clicked() {
                        self.csv_export.open = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Settings...").clicked() {
                        self.settings_dialog.open();
                        ui.close();
                    }
                });
                ui.menu_button("View", |ui| {
                    self.dock_open_checkbox(ui, AppDockTab::Diagnostics, "Diagnostic (F1)");
                    self.dock_open_checkbox(ui, AppDockTab::Performance, "Performance (F2)");
                    self.dock_open_checkbox(ui, AppDockTab::Markers, "Markers (F3)");
                    #[cfg(feature = "scripting")]
                    self.dock_open_checkbox(ui, AppDockTab::ScriptingConsole, "Scripting (F9)");
                    self.dock_open_checkbox(ui, AppDockTab::Logging, "Logging (F12)");
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
                    if ui.button("Clear current layout").clicked() {
                        self.clear_current_layout();
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
                #[cfg(feature = "scripting")]
                ui.menu_button("Scripts", |ui| {
                    if ui.button("Editor...").clicked() {
                        self.scripts.open = true;
                        ui.close();
                    }
                    if ui.button("Variables...").clicked() {
                        self.scripts.variables_open = true;
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button("Run", |ui| {
                        let names = self.scripts.script_names();
                        if names.is_empty() {
                            ui.add_enabled(false, egui::Button::new("No saved scripts"));
                        } else {
                            let run_enabled = self.scripts.ordinary_dispatch_enabled();
                            for name in names {
                                if ui
                                    .add_enabled(run_enabled, egui::Button::new(name.as_str()))
                                    .clicked()
                                {
                                    let _ = self.scripts.run_named(
                                        &name,
                                        self.session.store(),
                                        self.session.ingest_sender(),
                                        Arc::clone(self.session.metrics()),
                                    );
                                    ui.close();
                                }
                            }
                        }
                    });
                });
                #[cfg(feature = "scripting")]
                ui.menu_button("Parsers", |ui| {
                    if ui.button("Editor...").clicked() {
                        self.scripts.open_parser_editor();
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button("Parse File", |ui| match self.scripts.parser_names() {
                        Ok(names) if names.is_empty() => {
                            ui.add_enabled(false, egui::Button::new("No saved parsers"));
                        }
                        Ok(names) => {
                            let parser_open_enabled = self.scripts.parser_dispatch_enabled();
                            for name in names {
                                if ui
                                    .add_enabled(
                                        parser_open_enabled,
                                        egui::Button::new(name.as_str()),
                                    )
                                    .on_hover_text("Open file with parser")
                                    .clicked()
                                {
                                    let _ = self.scripts.request_open(ui.ctx(), &name);
                                    ui.close();
                                }
                            }
                        }
                        Err(_) => {
                            ui.add_enabled(false, egui::Button::new("Could not list parsers"));
                        }
                    });
                });
                if self.settings.show_fps {
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| match self.fps_ema {
                            Some(fps) => {
                                let color = if fps > 59.0 {
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
                        },
                    );
                }
            });
        });

        drop(ui_menu_timer);

        let ui_toolbar_timer = self.session.metrics().scope("ui_toolbar");
        egui::Panel::top("tool_icons").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let streaming = self.session.has_live_links();
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

                ui.separator();

                let active_tint = self.settings.theme.accent();
                let inactive_tint = ui.visuals().weak_text_color();
                hover_mode_menu_button(
                    ui,
                    "toolbar-hover-mode",
                    crate::icons::mouse_pointer(),
                    active_tint,
                    true,
                    |ui| {
                        use delog_core::field_view::SampleMode::{Linear, Next, Prev};
                        if ui
                            .radio_value(&mut self.hover_mode, Prev, "Previous")
                            .clicked()
                        {
                            ui.close();
                        }
                        if ui.radio_value(&mut self.hover_mode, Next, "Next").clicked() {
                            ui.close();
                        }
                        if ui
                            .radio_value(&mut self.hover_mode, Linear, "Linear")
                            .clicked()
                        {
                            ui.close();
                        }
                    },
                )
                .on_hover_text(format!("Select hover mode"));

                let snap_tint = if self.snap_playhead {
                    active_tint
                } else {
                    inactive_tint
                };
                if icon_button(
                    ui,
                    "toolbar-snap-playhead",
                    crate::icons::magnet(),
                    snap_tint,
                    self.snap_playhead,
                )
                .on_hover_text("Toggle playhead snap")
                .clicked()
                {
                    self.snap_playhead = !self.snap_playhead;
                }

                let has_marker = self.marker_us.is_some();
                let marker_tint = if has_marker {
                    active_tint
                } else {
                    inactive_tint
                };
                let marker_hover = if has_marker {
                    "Remove measuring marker"
                } else {
                    "Add measuring marker at playhead"
                };
                if ui
                    .add_enabled_ui(has_marker || global_range.is_some(), |ui| {
                        icon_button(
                            ui,
                            "toolbar-measuring-marker",
                            crate::icons::ruler_dimension_line(),
                            marker_tint,
                            has_marker,
                        )
                    })
                    .inner
                    .on_hover_text(marker_hover)
                    .clicked()
                {
                    self.marker_us = if has_marker {
                        None
                    } else {
                        Some(self.playback.t_us)
                    };
                }

                if icon_button(
                    ui,
                    "toolbar-equal-plot-heights",
                    crate::icons::grid_2x2_check(),
                    inactive_tint,
                    false,
                )
                .on_hover_text("Resize all plots")
                .clicked()
                {
                    self.workspace.equalize_plot_heights();
                }

                ui.separator();

                if icon_button(
                    ui,
                    "toolbar-legend-position",
                    legend_position_icon(self.settings.plot.legend_position),
                    active_tint,
                    true,
                )
                .on_hover_text("Cycle legend position")
                .clicked()
                {
                    self.settings.plot.legend_position =
                        next_legend_position(self.settings.plot.legend_position);
                }

                let legends_hidden = !self.workspace.all_plot_legends_visible();
                let legend_tint = if legends_hidden {
                    active_tint
                } else {
                    inactive_tint
                };
                if icon_button(
                    ui,
                    "toolbar-legends",
                    crate::icons::eye_off(),
                    legend_tint,
                    legends_hidden,
                )
                .on_hover_text("Toggle legends")
                .clicked()
                {
                    self.workspace.set_all_plot_legends(legends_hidden);
                }

                ui.separator();

                if icon_button(
                    ui,
                    "toolbar-export-workspace-png",
                    crate::icons::export(),
                    inactive_tint,
                    false,
                )
                .on_hover_text("Export workspace PNG")
                .clicked()
                {
                    self.queue_image_capture(
                        ui.ctx(),
                        crate::image_export::ImageCaptureIntent::workspace(
                            crate::image_export::ImageCaptureAction::Export,
                            self.frame,
                        ),
                    );
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
                        recording_status(&status)
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

                let native_load_active = self.session.has_active_loads();
                #[cfg(feature = "scripting")]
                let parser_label = self
                    .scripts
                    .is_parser_running()
                    .then(|| self.scripts.parser_active_label());
                #[cfg(not(feature = "scripting"))]
                let parser_label: Option<String> = None;
                let load_state = combined_load_state(
                    native_load_active,
                    self.session.active_labels(),
                    parser_label.as_deref(),
                );
                if load_state.active {
                    ui.separator();
                    if ui.button("Cancel").clicked() {
                        self.session.cancel_all();
                        #[cfg(feature = "scripting")]
                        if load_state.parser_active {
                            self.scripts.cancel_parsers();
                        }
                    }
                    if !load_state.native_labels.is_empty() {
                        ui.label(format!("loading {}", load_state.native_labels.join(", ")));
                    }
                    if let Some(label) = &load_state.parser_label {
                        ui.label(label);
                    }
                    if !load_state.parser_active
                        && let Some(frac) = self.session.overall_progress()
                    {
                        ui.add(egui::ProgressBar::new(frac).desired_width(120.0));
                    } else {
                        ui.spinner();
                    }
                }
            });
        });

        drop(ui_toolbar_timer);
        let range = timeline_range_for_ui(global_range);

        let (focus_diagnostics, focus_performance, focus_markers, focus_logging) =
            ui.ctx().input(|i| {
                (
                    i.key_pressed(egui::Key::F1),
                    i.key_pressed(egui::Key::F2),
                    i.key_pressed(egui::Key::F3),
                    i.key_pressed(egui::Key::F12),
                )
            });
        if focus_diagnostics {
            self.open_dock(AppDockTab::Diagnostics);
        }
        if focus_performance {
            self.open_dock(AppDockTab::Performance);
        }
        if focus_markers {
            self.open_dock(AppDockTab::Markers);
        }
        if focus_logging {
            self.open_dock(AppDockTab::Logging);
        }
        #[cfg(feature = "scripting")]
        if ui.ctx().input(|i| i.key_pressed(egui::Key::F9)) {
            self.open_dock(AppDockTab::ScriptingConsole);
        }

        // Transport keys — skipped while a widget owns the
        // keyboard (e.g. the browser filter box).
        if !ui.ctx().egui_wants_keyboard_input() {
            let (space, home, end, left, right, save_layout, load_layout, add_marker) =
                ui.ctx().input(|i| {
                    (
                        i.key_pressed(egui::Key::Space),
                        i.key_pressed(egui::Key::Home),
                        i.key_pressed(egui::Key::End),
                        i.key_pressed(egui::Key::ArrowLeft),
                        i.key_pressed(egui::Key::ArrowRight),
                        i.modifiers.command && i.key_pressed(egui::Key::S),
                        i.modifiers.command && i.key_pressed(egui::Key::L),
                        i.key_pressed(egui::Key::M),
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
                let target =
                    crate::timeline::step_target(&snapshot, reference, self.playback.t_us, right);
                self.playback.scrub(target, range);
            }
            if add_marker {
                self.markers.add_at(self.playback.t_us);
            }
        }

        let ui_diagnostics_timer = self.session.metrics().scope("ui_diagnostics");
        let diagnostics = self.session.diagnostic_records();
        // Auto-open the dock when a new (distinct) diagnostic arrives. The seq is
        // tracked even when the feature is off so re-enabling it never opens for
        // diagnostics that landed while disabled.
        if let Some(newest_seq) = diagnostics.iter().map(|record| record.seq).max() {
            if should_auto_open_diagnostics(
                self.settings.auto_open_diagnostics,
                self.last_diagnostic_seq,
                newest_seq,
            ) {
                self.open_dock(AppDockTab::Diagnostics);
            }
            self.last_diagnostic_seq = Some(newest_seq);
        }
        drop(ui_diagnostics_timer);
        let ui_performance_timer = self.session.metrics().scope("ui_performance");
        if self.docks.is_open(AppDockTab::Performance) {
            self.refresh_performance_snapshot(frame, &snapshot);
            ui.ctx().request_repaint_after(PERFORMANCE_REFRESH_INTERVAL);
        }
        drop(ui_performance_timer);
        self.sync_docks_from_legacy_flags();
        if self.docks.has_tabs() {
            let mut actions = PendingDockActions::default();
            #[cfg(feature = "scripting")]
            let store = self.session.store();
            #[cfg(feature = "scripting")]
            let ingest_sender = self.session.ingest_sender();
            #[cfg(feature = "scripting")]
            let metrics = self.session.metrics();
            let show_docks = |ui: &mut egui::Ui,
                              docks: &mut AppDockController,
                              viewer: &mut AppDockViewer<'_>| {
                docks.show_inside(ui, viewer);
            };
            let mut render_docks = |ui: &mut egui::Ui| {
                let mut viewer = AppDockViewer {
                    diagnostics_dock: &mut self.diagnostics_dock,
                    diagnostics: &diagnostics,
                    snapshot: &snapshot,
                    logging_dock: &mut self.logging_dock,
                    logs: &self.logs,
                    performance_dock: &mut self.performance_dock,
                    performance_snapshot: &self.performance_snapshot,
                    markers_dock: &mut self.markers_dock,
                    markers: &mut self.markers,
                    origin_us: self.origin_us,
                    #[cfg(feature = "scripting")]
                    scripts: &mut self.scripts,
                    #[cfg(feature = "scripting")]
                    store: &store,
                    #[cfg(feature = "scripting")]
                    ingest_sender: &ingest_sender,
                    #[cfg(feature = "scripting")]
                    metrics,
                    actions: &mut actions,
                };
                show_docks(ui, &mut self.docks, &mut viewer);
            };

            egui::Panel::bottom("app_docks")
                .resizable(true)
                .default_size(240.0)
                .show_inside(ui, |ui| {
                    render_docks(ui);
                });

            if !self.diagnostics_dock.open {
                self.sync_dock_from_legacy_flag(AppDockTab::Diagnostics, false);
            }
            if !self.logging_dock.open {
                self.sync_dock_from_legacy_flag(AppDockTab::Logging, false);
            }
            if !self.performance_dock.open {
                self.sync_dock_from_legacy_flag(AppDockTab::Performance, false);
            }
            if !self.markers_dock.open {
                self.sync_dock_from_legacy_flag(AppDockTab::Markers, false);
            }
            #[cfg(feature = "scripting")]
            if !self.scripts.console_open {
                self.sync_dock_from_legacy_flag(AppDockTab::ScriptingConsole, false);
            }

            self.sync_legacy_dock_flags_from_state();

            if actions.clear_diagnostics {
                self.session.clear_diagnostics();
            }
            if let Some(t_us) = actions.diagnostic_jump_us
                && let Some(range) = snapshot.global_time_range()
            {
                self.playback.scrub(t_us, range);
            }
            if actions.clear_logs {
                self.logs.clear();
            }
            if let Some(t_us) = actions.marker_jump_us
                && let Some(range) = snapshot.global_time_range()
            {
                self.playback.scrub(t_us, range);
            }
        }

        // The timeline's `utc_offset_us` arg stays None until a parser captures
        // a UTC reference (BIN GPS week / ULog time_ref_utc); `any_live` stays
        // false because the snapshot has no streaming flag yet. It is registered
        // after the resizable docks so those docks sit below the timeline.
        let ui_timeline_timer = self.session.metrics().scope("ui_timeline");
        egui::Panel::bottom("timeline").show_inside(ui, |ui| {
            let action = crate::timeline::ui(
                ui,
                &mut self.playback,
                &mut self.fit_view_all,
                &mut self.view,
                range,
                None,
                self.session.has_live_links(),
                self.settings.theme,
                &self.markers,
            );
            if action.lock_live {
                self.lock_to_live(range);
            }
            if action.view_changed {
                // Dragging the window slider is a manual view change: drop
                // out of fit-all and live-follow, like a pan/zoom.
                self.fit_view_all = false;
                self.playback.unlock_live();
                self.view_fitted = true;
            }
            if let Some(t_us) = action.marker_jump {
                self.playback.scrub(t_us, range);
            }
            if let Some((id, t_us)) = action.marker_move
                && let Some(m) = self.markers.get_mut(id)
            {
                m.t_us = t_us.clamp(range.min_us, range.max_us);
            }
            if let Some(id) = action.marker_delete {
                self.markers.remove(id);
            }
            if let Some((id, edit)) = action.marker_edit
                && let Some(m) = self.markers.get_mut(id)
            {
                if let Some(label) = edit.label {
                    m.label = label;
                }
                if let Some(color) = edit.color {
                    m.color = color;
                }
            }
        });
        drop(ui_timeline_timer);

        let ui_browser_timer = self.session.metrics().scope("ui_browser");
        if self.browser_collapsed {
            let button_size = browser::panel_toggle_button_size(ui);
            let collapsed_left_margin = ui.spacing().item_spacing.x;
            let collapsed_width = collapsed_left_margin + button_size.x;
            let collapsed_frame =
                egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::ZERO);
            egui::Panel::left("data_browser_collapsed")
                .resizable(false)
                .show_separator_line(false)
                .frame(collapsed_frame)
                .exact_size(collapsed_width)
                .show_inside(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_space(collapsed_left_margin);
                            let icon_size = button_size - ui.spacing().button_padding * 2.0;
                            let icon = egui::Image::new(crate::icons::panel_left_open())
                                .fit_to_exact_size(icon_size)
                                .tint(ui.visuals().text_color());
                            if ui
                                .add_sized(button_size, egui::Button::image(icon))
                                .on_hover_text("Show data browser")
                                .clicked()
                            {
                                self.browser_collapsed = false;
                            }
                        });
                    });
                });
        } else {
            // Reuse the cached tree while the epoch is unchanged. Take it out of
            // `self` so the render closure can mutably borrow other `self` fields
            // without aliasing the model, then put it back after the panel.
            let epoch = snapshot.epoch;
            let model = match self.browser_model.take() {
                Some((cached_epoch, model)) if cached_epoch == epoch => model,
                _ => {
                    self.browser_filter.reset();
                    BrowserModel::from_snapshot(&snapshot)
                }
            };
            let browser_panel = egui::Panel::left("data_browser_expanded")
                .resizable(true)
                .min_size(360.0);
            let browser_panel = if model.is_empty() {
                browser_panel.default_size(ui.spacing().text_edit_width)
            } else {
                browser_panel.default_size(360.0)
            };
            browser_panel.show_inside(ui, |ui| {
                // Offset edits go through the ingest thread (the single
                // registry writer) and come back as a new epoch.
                let browser_response = browser::ui(
                    ui,
                    epoch,
                    &model,
                    &mut self.browser_query,
                    &mut self.browser_filter,
                    &mut self.browser_selection,
                    &mut self.offset_dialog,
                );
                if browser_response.collapse_requested {
                    self.browser_collapsed = true;
                }
                if let Some((source, offset_us)) = browser_response.offset_change {
                    self.session.set_source_offset(source, offset_us);
                }
                if let Some(source) = browser_response.remove_source {
                    self.session.remove_source(source);
                }
                if let Some(source) = browser_response.inspect_source {
                    self.source_metadata_dialog = Some(source);
                }
                if let Some(field) = browser_response.inspect_field_metadata {
                    self.field_metadata_dialog = Some(field);
                }
                if let Some(field) = browser_response.inspect_field_stats {
                    self.field_stats.open(field);
                }
                if let Some(field) = browser_response.generate_markers {
                    let title = crate::legend::trace_label(&snapshot, field);
                    self.generate_markers_dialog =
                        Some(crate::generate_markers::GenerateMarkersDialog::open(
                            &snapshot, field, title,
                        ));
                }
            });
            self.browser_model = Some((epoch, model));
        }
        drop(ui_browser_timer);
        show_source_metadata_window(ui.ctx(), &snapshot, &mut self.source_metadata_dialog);
        show_field_metadata_window(ui.ctx(), &snapshot, &mut self.field_metadata_dialog);
        show_field_stats_window(
            ui.ctx(),
            &snapshot,
            self.view,
            &mut self.caches,
            &mut self.field_stats,
        );
        for (t_us, name, color) in crate::generate_markers::generate_markers_window(
            ui.ctx(),
            &mut self.generate_markers_dialog,
        ) {
            self.markers.push_loaded(t_us, name, color, String::new());
        }

        if self.csv_export.open {
            let model = self
                .browser_model
                .as_ref()
                .map(|(_, m)| m.clone())
                .unwrap_or_default();
            let fields = crate::csv_export::numeric_fields(&snapshot, &model);
            let full = snapshot
                .global_time_range()
                .map(|r| (r.min_us, r.max_us))
                .unwrap_or((0, 1));
            let visible = self.view.map(|v| (v.min_us, v.max_us)).unwrap_or(full);
            if let Some(req) =
                crate::csv_export::dialog_ui(ui.ctx(), &mut self.csv_export, &fields, visible, full)
            {
                self.spawn_csv_export(ui.ctx(), &snapshot, &fields, req);
                self.csv_export.open = false;
            }
        }

        let ui_workspace_timer = self.session.metrics().scope("ui_workspace");
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            // The workspace renders even before any log loads, so plots can be
            // arranged and the 3D view opened on an empty session.

            let workspace_rect = ui.available_rect_before_wrap();

            // The central panel is a fallback drop zone: dropping a field onto
            // empty workspace space plots it in the first pane.
            let frame_style = egui::Frame::default();
            let mut handled_workspace_drop = false;
            let (_, dropped) =
                ui.dnd_drop_zone::<Vec<delog_core::identity::FieldId>, ()>(frame_style, |ui| {
                    // Owned metrics handle: `behavior` borrows `self` mutably
                    // below, so we can't reach `self.session` while it lives.
                    let tree_metrics = self.session.metrics().clone();
                    let live_map_scopes = self.workspace.map_scopes();
                    self.gpu.retain_map_scopes(frame, &live_map_scopes);
                    if let Some(manager) = self.tile_manager.as_mut() {
                        manager.retain_scopes(&live_map_scopes);
                    }
                    self.gpu.begin_plot_frame(frame);
                    let services = PlotServices {
                        frame,
                        snapshot: &snapshot,
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
                        render_tuning: self.settings.render,
                        scene3d: self.settings.scene3d,
                        accent: self.settings.theme.accent(),
                        playhead_us: snapshot.global_time_range().map(|_| self.playback.t_us),
                        playing: self.playback.playing,
                        vehicles: &self.vehicles,
                        trajectories: &self.vehicle_trajectories,
                        traj_generation: self.traj_vehicle_revision,
                        shared_y_gutter: self.workspace.shared_y_gutter,
                        plot_display: self.settings.plot,
                        markers: self.markers.as_slice(),
                    };
                    let mut behavior = crate::workspace::Behavior::new(services);
                    // `workspace_tree`: the egui_tiles layout + pane rendering.
                    // Profiling (2026-06-28) showed egui_tiles' own machinery is
                    // negligible (~0.02 ms); the cost is the per-pane `pane_ui`
                    // render. `ui_workspace − workspace_tree` is begin/retain +
                    // action handling.
                    let tree_timer = tree_metrics.scope("workspace_tree");
                    self.workspace.tree.ui(&mut behavior, ui);
                    drop(tree_timer);
                    let actions = behavior.into_actions();
                    // Share the widest pane gutter so stacked plots align next
                    // frame. Converges in one frame; until then each
                    // pane never drops below its own gutter, so labels never
                    // clip.
                    self.workspace.shared_y_gutter = actions.max_y_gutter;
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
                        // Manual pan/zoom drops out of fit-all (like a scrub
                        // disengages live-follow).
                        self.fit_view_all = false;
                    }
                    if actions.open_vehicle_config {
                        self.vehicle_dialog.open = true;
                    }
                    if actions.export_kml {
                        self.spawn_export_kml_dialog(ui.ctx(), &snapshot);
                    }
                    if let Some(field) = actions.inspect_field_stats {
                        self.field_stats.open(field);
                    }
                    if let Some(action) = actions.image {
                        match action {
                            crate::workspace::WorkspaceImageAction::CopyPlot { rect } => {
                                self.queue_image_capture(
                                    ui.ctx(),
                                    crate::image_export::ImageCaptureIntent::plot(
                                        crate::image_export::ImageCaptureAction::Copy,
                                        rect,
                                        self.frame,
                                    ),
                                );
                            }
                            crate::workspace::WorkspaceImageAction::ExportPlot { rect } => {
                                self.queue_image_capture(
                                    ui.ctx(),
                                    crate::image_export::ImageCaptureIntent::plot(
                                        crate::image_export::ImageCaptureAction::Export,
                                        rect,
                                        self.frame,
                                    ),
                                );
                            }
                        }
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
            self.start_queued_image_capture(ui.ctx(), Some(workspace_rect));
        });
        drop(ui_workspace_timer);

        // Floating windows/dialogs + overlays; drops with the function (still
        // inside `frame_total`, after every other section).
        let _ui_windows_timer = self.session.metrics().scope("ui_windows");
        self.show_layout_windows(ui.ctx());
        crate::message_popup::show_all(&mut self.message_popups, ui.ctx());
        let settings_before = self.settings.clone();
        let tile_cache =
            self.tile_manager
                .as_ref()
                .map_or_else(TileCacheUiState::default, |manager| {
                    let status = manager.status();
                    TileCacheUiState {
                        available: true,
                        usage_bytes: status.cache_bytes,
                        clearing: matches!(
                            status.cache_action,
                            CacheActionStatus::Pending {
                                kind: CacheActionKind::Clear,
                                ..
                            }
                        ),
                    }
                });
        let settings_change = self
            .settings_dialog
            .show(ui.ctx(), &mut self.settings, tile_cache);
        if settings_change.theme_changed || self.theme_needs_apply {
            self.settings.theme.apply(ui.ctx());
            self.theme_needs_apply = false;
        }
        if let Some(manager) = self.tile_manager.as_mut() {
            if settings_change.tile_cache_limit_changed
                && let Err(error) = manager.set_limit(self.settings.scene3d.tile_cache_limit_bytes)
            {
                tracing::warn!(%error, "failed to queue map tile cache limit");
            }
            if settings_change.clear_tile_cache
                && let Err(error) = manager.clear_cache()
            {
                tracing::warn!(%error, "failed to queue map tile cache clear");
            }
        }
        if settings_change.map_provider_changed
            || tile_cache_needs_repaint(settings_change.clear_tile_cache, tile_cache.clearing)
        {
            ui.ctx().request_repaint();
        }
        if self.settings != settings_before
            && let Err(err) = crate::layout::save_app_settings(&self.settings)
        {
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::error(
                    "settings-save",
                    err.to_string(),
                ));
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
        for log in self.vehicle_dialog.take_logs() {
            self.push_log(log);
        }
        if let Some(endpoint) = self
            .connection_dialog
            .ui(ui.ctx(), &mut self.show_connection_dialog)
        {
            let recording = match self.connection_dialog.recording_path() {
                Ok(recording) => recording,
                Err(err) => {
                    self.session
                        .push_diagnostic(delog_core::diagnostics::Diag::error(
                            "live-recording",
                            err,
                        ));
                    None
                }
            };
            self.settings.live_connection = self.connection_dialog.to_settings();
            if let Err(err) = crate::layout::save_app_settings(&self.settings) {
                self.session
                    .push_diagnostic(delog_core::diagnostics::Diag::error(
                        "settings-save",
                        err.to_string(),
                    ));
            }
            if let Err(err) = self.session.start_live(endpoint, recording) {
                self.session
                    .push_diagnostic(delog_core::diagnostics::Diag::error("live-open", err));
            }
        }

        #[cfg(feature = "scripting")]
        {
            if let Some(sink) = self.scripts.live_batch_sender_if_running() {
                self.session.set_live_script_sink(Some(sink));
            }
            self.scripts.ui(
                ui.ctx(),
                self.session.store(),
                self.session.ingest_sender(),
                Arc::clone(self.session.metrics()),
                self.settings.scripting.auto_open_variables,
                self.settings.scripting.auto_open_console,
            );
            for message in self.scripts.take_parser_diagnostics() {
                self.push_log(PendingLog::with_target(
                    LogLevel::Error,
                    "python-parser",
                    message,
                ));
            }
            for log in self.scripts.take_logs() {
                self.push_log(log);
            }
        }
    }
}

fn recording_status(status: &delog_stream::LiveLinkStatus) -> String {
    if status.recording.is_none() {
        return String::new();
    }
    if status.ingest.recorder_errors == 0 {
        format!(" · rec {} frames", status.ingest.recorder_records)
    } else {
        format!(
            " · rec {} frames · {} errors",
            status.ingest.recorder_records, status.ingest.recorder_errors
        )
    }
}

fn diagnostics_export_doc(
    records: Vec<DiagRecord>,
    snapshot: &delog_core::snapshot::StoreSnapshot,
) -> DiagnosticsExportDoc {
    let exported_at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let records = records
        .into_iter()
        .map(|record| {
            let source_label = record
                .diag
                .source
                .and_then(|source| snapshot.source(source))
                .map(|source| source.entry.label.clone());
            DiagnosticsExportRecord {
                seq: record.seq,
                count: record.count,
                severity: export_severity(record.diag.severity),
                code: record.diag.code,
                source_id: record.diag.source.map(|source| source.0),
                source_label,
                time_us: record.diag.time_us,
                byte_offset: record.diag.byte_offset,
                message: record.diag.message,
            }
        })
        .collect();
    DiagnosticsExportDoc {
        delog_diagnostics: 1,
        exported_at_unix_ms,
        records,
    }
}

fn export_severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

/// Which tab of the source metadata window is active. Persisted per source in
/// egui temporary memory so the selection survives across frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum SourceMetaTab {
    #[default]
    Info,
    Parameters,
    LoggedMessages,
}

impl SourceMetaTab {
    const ALL: [Self; 3] = [Self::Info, Self::Parameters, Self::LoggedMessages];

    const fn label(self) -> &'static str {
        match self {
            Self::Info => "Info",
            Self::Parameters => "Parameters",
            Self::LoggedMessages => "Logged Messages",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct FieldMetadata {
    title: String,
    source_label: String,
    topic_name: String,
    field_name: String,
    dtype: &'static str,
    unit: Option<String>,
    description: Option<String>,
    multiplier: f64,
    rows: u64,
    source_offset_us: i64,
    range: Option<TimeRange>,
}

fn field_metadata(
    snapshot: &delog_core::snapshot::StoreSnapshot,
    field_id: delog_core::identity::FieldId,
) -> Option<FieldMetadata> {
    let field = snapshot
        .fields
        .get(field_id.index())
        .filter(|field| field.id == field_id && !field.removed)?;
    let topic = snapshot
        .topic(field.topic)
        .filter(|topic| !topic.entry.removed)?;
    let source = snapshot
        .source(topic.entry.source)
        .filter(|source| !source.entry.removed)?;
    let store = topic.store.as_ref()?;
    let schema = store.schema.field_by_name(&field.name)?;
    let range = store
        .time_range()
        .and_then(|range| range.offset(source.entry.offset_us));

    Some(FieldMetadata {
        title: format!(
            "{} / {}.{}",
            source.entry.label, topic.entry.name, field.name
        ),
        source_label: source.entry.label.clone(),
        topic_name: topic.entry.name.clone(),
        field_name: field.name.clone(),
        dtype: schema.dtype_label(),
        unit: schema.unit.clone(),
        description: schema.description.clone(),
        multiplier: schema.multiplier,
        rows: store.rows,
        source_offset_us: source.entry.offset_us,
        range,
    })
}

fn show_field_metadata_window(
    ctx: &egui::Context,
    snapshot: &delog_core::snapshot::StoreSnapshot,
    selected: &mut Option<delog_core::identity::FieldId>,
) {
    let Some(field_id) = *selected else {
        return;
    };
    let Some(meta) = field_metadata(snapshot, field_id) else {
        *selected = None;
        return;
    };

    let mut open = true;
    egui::Window::new(format!("Field Metadata - {}", meta.title))
        .id(egui::Id::new(("field_metadata", field_id.0)))
        .open(&mut open)
        .collapsible(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .default_width(440.0)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("field_metadata_summary")
                .num_columns(2)
                .striped(true)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    ui.strong("Source");
                    ui.label(meta.source_label.as_str());
                    ui.end_row();
                    ui.strong("Topic");
                    ui.label(meta.topic_name.as_str());
                    ui.end_row();
                    ui.strong("Field");
                    ui.label(meta.field_name.as_str());
                    ui.end_row();
                    ui.strong("Field ID");
                    ui.monospace(field_id.0.to_string());
                    ui.end_row();
                    ui.strong("Type");
                    ui.label(meta.dtype);
                    ui.end_row();
                    ui.strong("Unit");
                    ui.label(meta.unit.as_deref().unwrap_or("-"));
                    ui.end_row();
                    ui.strong("Multiplier");
                    ui.monospace(meta.multiplier.to_string());
                    ui.end_row();
                    ui.strong("Rows");
                    ui.label(meta.rows.to_string());
                    ui.end_row();
                    ui.strong("Offset");
                    ui.label(format!("{} us", meta.source_offset_us));
                    ui.end_row();
                    ui.strong("Range");
                    ui.label(meta.range.map(format_range).unwrap_or_else(|| "-".into()));
                    ui.end_row();
                });
            ui.separator();
            match meta.description.as_deref() {
                Some(description) if !description.is_empty() => {
                    ui.label(description);
                }
                _ => {
                    ui.weak("No field description.");
                }
            }
        });

    if !open {
        *selected = None;
    }
}

fn show_source_metadata_window(
    ctx: &egui::Context,
    snapshot: &delog_core::snapshot::StoreSnapshot,
    selected: &mut Option<delog_core::identity::SourceId>,
) {
    let Some(source_id) = *selected else {
        return;
    };
    let Some(source) = snapshot
        .source(source_id)
        .filter(|source| !source.entry.removed)
    else {
        *selected = None;
        return;
    };

    let mut open = true;
    egui::Window::new(format!("Source Metadata - {}", source.entry.label))
        .id(egui::Id::new(("source_metadata", source_id.0)))
        .open(&mut open)
        .collapsible(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .default_width(520.0)
        .default_height(420.0)
        .show(ctx, |ui| {
            let tab_id = egui::Id::new(("source_metadata_tab", source_id.0));
            let active_tab = ui
                .data(|d| d.get_temp::<SourceMetaTab>(tab_id))
                .unwrap_or_default();
            let mut dock_state = source_metadata_dock_state(active_tab);
            let mut viewer = SourceMetadataTabViewer {
                snapshot,
                source_id,
            };
            egui_dock::DockArea::new(&mut dock_state)
                .id(egui::Id::new(("source_metadata_dock_area", source_id.0)))
                .style(egui_dock::Style::from_egui(ui.style().as_ref()))
                .allowed_splits(egui_dock::AllowedSplits::None)
                .draggable_tabs(false)
                .tab_context_menus(false)
                .show_close_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut viewer);
            ui.data_mut(|d| d.insert_temp(tab_id, active_source_metadata_tab(&mut dock_state)));
        });

    if !open {
        *selected = None;
    }
}

fn source_metadata_dock_state(active_tab: SourceMetaTab) -> egui_dock::DockState<SourceMetaTab> {
    let mut dock_state = egui_dock::DockState::new(SourceMetaTab::ALL.to_vec());
    if let Some(path) = dock_state.find_tab(&active_tab) {
        let _ = dock_state.set_active_tab(path);
        dock_state.set_focused_node_and_surface(path.node_path());
    }
    dock_state
}

fn active_source_metadata_tab(
    dock_state: &mut egui_dock::DockState<SourceMetaTab>,
) -> SourceMetaTab {
    dock_state
        .find_active_focused()
        .map(|(_, tab)| *tab)
        .unwrap_or_default()
}

struct SourceMetadataTabViewer<'a> {
    snapshot: &'a delog_core::snapshot::StoreSnapshot,
    source_id: delog_core::identity::SourceId,
}

impl egui_dock::TabViewer for SourceMetadataTabViewer<'_> {
    type Tab = SourceMetaTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.label().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        show_source_metadata_tab(ui, self.snapshot, self.source_id, *tab);
    }

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        false
    }
}

fn show_source_metadata_tab(
    ui: &mut egui::Ui,
    snapshot: &delog_core::snapshot::StoreSnapshot,
    source_id: delog_core::identity::SourceId,
    tab: SourceMetaTab,
) {
    let Some(source) = snapshot
        .source(source_id)
        .filter(|source| !source.entry.removed)
    else {
        return;
    };

    match tab {
        SourceMetaTab::Info => {
            let (rows, range, topics) = source_summary(snapshot, source_id);
            source_metadata_summary_table(
                ui,
                &[
                    ("Label", source.entry.label.clone()),
                    (
                        "Kind",
                        source_kind_label(source.entry.label.as_str()).to_owned(),
                    ),
                    ("Source ID", source_id.0.to_string()),
                    ("Topics", topics.to_string()),
                    ("Rows", rows.to_string()),
                    ("Offset", format!("{} us", source.entry.offset_us)),
                    (
                        "Range",
                        range.map(format_range).unwrap_or_else(|| "-".into()),
                    ),
                ],
            );
        }
        SourceMetaTab::Parameters => {
            if source.entry.meta.params.is_empty() {
                ui.weak("No parameters captured.");
            } else {
                let query_id = egui::Id::new(("source_param_query", source_id.0));
                let mut query = ui
                    .data(|d| d.get_temp::<String>(query_id))
                    .unwrap_or_default();
                ui.add(
                    egui::TextEdit::singleline(&mut query)
                        .hint_text("Filter parameters...")
                        .desired_width(f32::INFINITY),
                );
                ui.data_mut(|d| d.insert_temp(query_id, query.clone()));

                let matches: Vec<_> = source
                    .entry
                    .meta
                    .params
                    .iter()
                    .filter(|param| crate::browser::matches_query(&query, &param.name))
                    .collect();
                if matches.is_empty() {
                    ui.weak("No parameters match the filter.");
                } else {
                    source_metadata_params_table(ui, source_id, &matches);
                }
            }
        }
        SourceMetaTab::LoggedMessages => {
            if source.entry.meta.auto_markers.is_empty() {
                ui.weak("No logged messages captured.");
            } else {
                source_metadata_markers_table(ui, source_id, &source.entry.meta.auto_markers);
            }
        }
    }
}

fn source_metadata_summary_table(ui: &mut egui::Ui, rows: &[(&str, String)]) {
    let row_height = table_row_height(ui);
    TableBuilder::new(ui)
        .id_salt("source_metadata_summary_table")
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .auto_shrink([false, false])
        .column(Column::auto().at_least(96.0))
        .column(Column::remainder().clip(true))
        .body(|mut body| {
            for (key, value) in rows {
                body.row(row_height, |mut row| {
                    row.col(|ui| {
                        ui.strong(*key);
                    });
                    row.col(|ui| {
                        ui.label(value);
                    });
                });
            }
        });
}

fn source_metadata_params_table(
    ui: &mut egui::Ui,
    source_id: delog_core::identity::SourceId,
    params: &[&delog_core::identity::SourceParam],
) {
    let row_height = table_row_height(ui);
    egui::ScrollArea::vertical()
        .id_salt(("source_params", source_id.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt("source_metadata_params_table")
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .auto_shrink([false, false])
                .column(Column::auto().at_least(120.0))
                .column(Column::auto().at_least(72.0))
                .column(Column::remainder().clip(true))
                .header(row_height, |mut header| {
                    header.col(|ui| {
                        ui.strong("Name");
                    });
                    header.col(|ui| {
                        ui.strong("Type");
                    });
                    header.col(|ui| {
                        ui.strong("Value");
                    });
                })
                .body(|body| {
                    body.rows(row_height, params.len(), |mut row| {
                        let param = params[row.index()];
                        row.col(|ui| {
                            ui.monospace(param.name.as_str());
                        });
                        row.col(|ui| {
                            ui.label(param.ty.as_str());
                        });
                        row.col(|ui| {
                            ui.label(param.value.as_str());
                        });
                    });
                });
        });
}

fn source_metadata_markers_table(
    ui: &mut egui::Ui,
    source_id: delog_core::identity::SourceId,
    markers: &[delog_core::identity::AutoMarker],
) {
    let row_height = table_row_height(ui);
    egui::ScrollArea::vertical()
        .id_salt(("source_markers", source_id.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt("source_metadata_markers_table")
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .auto_shrink([false, false])
                .column(Column::auto().at_least(72.0))
                .column(Column::auto().at_least(72.0))
                .column(Column::remainder().clip(true))
                .header(row_height, |mut header| {
                    header.col(|ui| {
                        ui.strong("Time");
                    });
                    header.col(|ui| {
                        ui.strong("Level");
                    });
                    header.col(|ui| {
                        ui.strong("Text");
                    });
                })
                .body(|body| {
                    body.rows(row_height, markers.len(), |mut row| {
                        let marker = &markers[row.index()];
                        row.col(|ui| {
                            ui.label(format!("{:.3}s", marker.time_us as f64 / 1e6));
                        });
                        row.col(|ui| {
                            ui.label(marker.level.to_string());
                        });
                        row.col(|ui| {
                            ui.label(marker.text.as_str());
                        });
                    });
                });
        });
}

fn show_field_stats_window(
    ctx: &egui::Context,
    snapshot: &Arc<delog_core::snapshot::StoreSnapshot>,
    view: Option<ViewX>,
    caches: &mut CacheManager,
    controller: &mut FieldStatsController,
) {
    let Some(field_id) = controller.selected() else {
        return;
    };
    let Some((title, unit)) = field_label_and_unit(snapshot, field_id) else {
        controller.close();
        return;
    };

    let now = Instant::now();
    if controller.tab() == StatsTab::Visible
        && let Some(view) = view
    {
        controller.request(
            StatsRequestKey::new(field_id, snapshot.epoch, view.min_us, view.max_us),
            Arc::clone(snapshot),
            now,
        );
    }
    controller.poll(now);

    let provisional = view.and_then(|view| {
        let cache = caches.get(field_id)?;
        let (x0, x1) = view.seconds(cache.origin_us);
        let (a, b) = cache.index_range(x0, x1);
        let mm = cache.pyramid.query(&cache.xy, a, b);
        mm.is_finite().then_some((
            f64::from(mm.min) + cache.y_origin(),
            f64::from(mm.max) + cache.y_origin(),
        ))
    });
    let tab = controller.tab();
    let current = controller.result().copied();
    let displayed = current.or_else(|| controller.stale_result().copied());
    let visible_range = view.map(|view| TimeRange {
        min_us: view.min_us,
        max_us: view.max_us,
    });
    let global_range = field_time_range(snapshot, field_id);
    let updating = controller.is_updating();
    if updating {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    let error = controller.error().map(str::to_owned);
    let suffix = unit
        .as_ref()
        .map(|unit| format!(" {unit}"))
        .unwrap_or_default();

    let mut open = true;
    egui::Window::new(field_stats_window_title(&title))
        .id(egui::Id::new(("field_stats", field_id.0)))
        .open(&mut open)
        .collapsible(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .default_width(320.0)
        .resizable(false)
        .show(ctx, |ui| {
            let mut dock_state = field_stats_dock_state(tab);
            let mut viewer = FieldStatsTabViewer {
                snapshot,
                field_id,
                visible_range,
                global_range,
                displayed,
                provisional,
                updating,
                error: error.as_deref(),
                suffix: &suffix,
            };
            egui_dock::DockArea::new(&mut dock_state)
                .id(egui::Id::new(("field_stats_dock_area", field_id.0)))
                .style(egui_dock::Style::from_egui(ui.style().as_ref()))
                .allowed_splits(egui_dock::AllowedSplits::None)
                .draggable_tabs(false)
                .tab_context_menus(false)
                .show_close_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut viewer);
            controller.set_tab(active_field_stats_tab(&mut dock_state));
        });

    if !open {
        controller.close();
    }
}

fn field_stats_dock_state(active_tab: StatsTab) -> egui_dock::DockState<StatsTab> {
    let mut dock_state = egui_dock::DockState::new(StatsTab::ALL.to_vec());
    if let Some(path) = dock_state.find_tab(&active_tab) {
        let _ = dock_state.set_active_tab(path);
        dock_state.set_focused_node_and_surface(path.node_path());
    }
    dock_state
}

fn active_field_stats_tab(dock_state: &mut egui_dock::DockState<StatsTab>) -> StatsTab {
    dock_state
        .find_active_focused()
        .map(|(_, tab)| *tab)
        .unwrap_or_default()
}

struct FieldStatsTabViewer<'a> {
    snapshot: &'a delog_core::snapshot::StoreSnapshot,
    field_id: delog_core::identity::FieldId,
    visible_range: Option<TimeRange>,
    global_range: Option<TimeRange>,
    displayed: Option<delog_core::analysis::FieldStats>,
    provisional: Option<(f64, f64)>,
    updating: bool,
    error: Option<&'a str>,
    suffix: &'a str,
}

impl egui_dock::TabViewer for FieldStatsTabViewer<'_> {
    type Tab = StatsTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.label().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            StatsTab::Visible => show_visible_field_stats_tab(
                ui,
                self.visible_range,
                self.displayed,
                self.provisional,
                self.updating,
                self.error,
                self.suffix,
            ),
            StatsTab::Global => show_global_field_stats_tab(
                ui,
                self.snapshot,
                self.field_id,
                self.global_range,
                self.suffix,
            ),
        }
    }

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        false
    }
}

fn show_visible_field_stats_tab(
    ui: &mut egui::Ui,
    range: Option<TimeRange>,
    displayed: Option<delog_core::analysis::FieldStats>,
    provisional: Option<(f64, f64)>,
    updating: bool,
    error: Option<&str>,
    suffix: &str,
) {
    stats_range_header(ui, range, updating);
    if let Some(error) = error {
        if error == "This field is not numeric." {
            ui.weak(error);
        } else {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    } else {
        ui.add_enabled_ui(!updating || displayed.is_none(), |ui| {
            stats_grid(
                ui,
                "visible_field_stats_grid",
                displayed,
                provisional,
                suffix,
            );
        });
    }
}

fn show_global_field_stats_tab(
    ui: &mut egui::Ui,
    snapshot: &delog_core::snapshot::StoreSnapshot,
    field_id: delog_core::identity::FieldId,
    range: Option<TimeRange>,
    suffix: &str,
) {
    stats_range_header(ui, range, false);
    match delog_core::analysis::global_field_stats(snapshot, field_id) {
        Ok(Some(stats)) => stats_grid(ui, "global_field_stats_grid", Some(stats), None, suffix),
        Ok(None) => {
            ui.weak("This field is not numeric.");
        }
        Err(err) => {
            ui.colored_label(ui.visuals().error_fg_color, err.to_string());
        }
    }
}

fn stats_range_header(ui: &mut egui::Ui, range: Option<TimeRange>, updating: bool) {
    ui.horizontal(|ui| {
        ui.strong("Range");
        match range {
            Some(range) => {
                ui.monospace(format_time_us(range.min_us));
                ui.weak("to");
                ui.monospace(format_time_us(range.max_us));
            }
            None => {
                ui.weak("unavailable");
            }
        }
        if updating {
            ui.separator();
            ui.label(egui::RichText::new("Updating...").color(ui.visuals().hyperlink_color));
        }
    });
    ui.add_space(6.0);
}

fn stats_grid(
    ui: &mut egui::Ui,
    id: &'static str,
    stats: Option<delog_core::analysis::FieldStats>,
    provisional: Option<(f64, f64)>,
    suffix: &str,
) {
    let min = stats.map(|s| s.min).or(provisional.map(|p| p.0));
    let max = stats.map(|s| s.max).or(provisional.map(|p| p.1));
    let rows = [
        ("Min", stat_with_unit(min, suffix)),
        ("Max", stat_with_unit(max, suffix)),
        ("Mean", stat_with_unit(stats.map(|s| s.mean), suffix)),
        ("Std dev", stat_with_unit(stats.map(|s| s.stddev), suffix)),
        (
            "Samples",
            stats.map_or_else(|| "-".into(), |s| s.count.to_string()),
        ),
        (
            "Missing",
            stats.map_or_else(|| "-".into(), |s| s.missing_count.to_string()),
        ),
        (
            "Rate",
            stats
                .and_then(|s| s.rate_hz)
                .map(|rate| format!("{} Hz", format_stat(rate)))
                .unwrap_or_else(|| "-".into()),
        ),
    ];
    let row_height = table_row_height(ui);
    TableBuilder::new(ui)
        .id_salt(id)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .auto_shrink([false, false])
        .column(Column::auto().at_least(72.0))
        .column(Column::remainder().clip(true))
        .body(|mut body| {
            for (key, value) in rows {
                body.row(row_height, |mut row| {
                    row.col(|ui| {
                        ui.strong(key);
                    });
                    row.col(|ui| {
                        ui.label(value);
                    });
                });
            }
        });
}

fn field_stats_window_title(field_label: &str) -> String {
    field_label.to_owned()
}

fn field_time_range(
    snapshot: &delog_core::snapshot::StoreSnapshot,
    field_id: delog_core::identity::FieldId,
) -> Option<TimeRange> {
    let field = snapshot
        .fields
        .get(field_id.index())
        .filter(|field| field.id == field_id && !field.removed)?;
    let topic = snapshot
        .topic(field.topic)
        .filter(|topic| !topic.entry.removed)?;
    let source = snapshot
        .source(topic.entry.source)
        .filter(|source| !source.entry.removed)?;
    topic
        .store
        .as_ref()?
        .time_range()
        .and_then(|range| range.offset(source.entry.offset_us))
}

fn table_row_height(ui: &egui::Ui) -> f32 {
    egui::TextStyle::Body
        .resolve(ui.style())
        .size
        .max(ui.spacing().interact_size.y)
}

fn stat_with_unit(value: Option<f64>, suffix: &str) -> String {
    value
        .map(|value| format!("{}{suffix}", format_stat(value)))
        .unwrap_or_else(|| "-".into())
}

fn format_time_us(value: i64) -> String {
    format!("{:.3} s", value as f64 / 1e6)
}

fn field_label_and_unit(
    snapshot: &delog_core::snapshot::StoreSnapshot,
    field_id: delog_core::identity::FieldId,
) -> Option<(String, Option<String>)> {
    let field = snapshot
        .fields
        .get(field_id.index())
        .filter(|field| field.id == field_id && !field.removed)?;
    let topic = snapshot.topic(field.topic)?;
    let source = snapshot.source(topic.entry.source)?;
    let unit = topic
        .store
        .as_ref()
        .and_then(|store| store.schema.field_by_name(&field.name))
        .and_then(|schema| schema.unit.clone());
    Some((
        format!(
            "{} / {}.{}",
            source.entry.label, topic.entry.name, field.name
        ),
        unit,
    ))
}

fn format_stat(value: f64) -> String {
    if value.is_nan() {
        "-".into()
    } else if value.abs() >= 100.0 {
        format!("{value:.0}")
    } else if value.abs() >= 10.0 {
        format!("{value:.2}")
    } else {
        format!("{value:.4}")
    }
}

fn source_summary(
    snapshot: &delog_core::snapshot::StoreSnapshot,
    source_id: delog_core::identity::SourceId,
) -> (u64, Option<TimeRange>, usize) {
    let Some(source) = snapshot.source(source_id) else {
        return (0, None, 0);
    };
    let mut rows = 0;
    let mut range: Option<TimeRange> = None;
    let mut topics = 0;
    for &topic_id in source.topics.iter() {
        let Some(topic) = snapshot
            .topic(topic_id)
            .filter(|topic| !topic.entry.removed)
        else {
            continue;
        };
        let Some(store) = topic.store.as_ref() else {
            continue;
        };
        topics += 1;
        rows += store.rows;
        if let Some(raw_range) = store.time_range()
            && let Some(effective) = raw_range.offset(source.entry.offset_us)
        {
            range = Some(match range {
                Some(current) => current.union(effective),
                None => effective,
            });
        }
    }
    (rows, range, topics)
}

fn format_range(range: TimeRange) -> String {
    format!(
        "{:.3}s - {:.3}s",
        range.min_us as f64 / 1e6,
        range.max_us as f64 / 1e6
    )
}

fn source_kind_label(label: &str) -> &'static str {
    if label.starts_with("mavlink:") {
        "Live MAVLink"
    } else if label.starts_with("script:") {
        "Derived"
    } else {
        "File"
    }
}

#[derive(Default)]
struct PendingDockActions {
    clear_diagnostics: bool,
    diagnostic_jump_us: Option<i64>,
    clear_logs: bool,
    marker_jump_us: Option<i64>,
}

struct AppDockViewer<'a> {
    diagnostics_dock: &'a mut DiagnosticsDock,
    diagnostics: &'a [DiagRecord],
    snapshot: &'a delog_core::snapshot::StoreSnapshot,
    logging_dock: &'a mut LoggingDock,
    logs: &'a [LogRecord],
    performance_dock: &'a mut PerformanceDock,
    performance_snapshot: &'a PerformanceSnapshot,
    markers_dock: &'a mut crate::markers::MarkersDock,
    markers: &'a mut crate::markers::Markers,
    origin_us: i64,
    #[cfg(feature = "scripting")]
    scripts: &'a mut scripts::ScriptsPanel,
    #[cfg(feature = "scripting")]
    store: &'a Arc<delog_core::snapshot::DataStore>,
    #[cfg(feature = "scripting")]
    ingest_sender: &'a delog_core::ingest::IngestSender,
    #[cfg(feature = "scripting")]
    metrics: &'a Arc<delog_core::metrics::MetricsRegistry>,
    actions: &'a mut PendingDockActions,
}

impl egui_dock::TabViewer for AppDockViewer<'_> {
    type Tab = AppDockTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        match tab {
            AppDockTab::Diagnostics => "Diagnostics".into(),
            AppDockTab::Performance => "Performance".into(),
            AppDockTab::Markers => "Markers".into(),
            #[cfg(feature = "scripting")]
            AppDockTab::ScriptingConsole => "Scripting".into(),
            AppDockTab::Logging => "Logging".into(),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            AppDockTab::Diagnostics => {
                let action = self
                    .diagnostics_dock
                    .ui(ui, self.diagnostics, self.snapshot);
                if action.clear {
                    self.actions.clear_diagnostics = true;
                }
                if action.jump_to_time_us.is_some() {
                    self.actions.diagnostic_jump_us = action.jump_to_time_us;
                }
            }
            AppDockTab::Performance => {
                self.performance_dock.ui(ui, self.performance_snapshot);
            }
            AppDockTab::Markers => {
                if let Some(t_us) = self.markers_dock.ui(ui, self.markers, self.origin_us) {
                    self.actions.marker_jump_us = Some(t_us);
                }
            }
            #[cfg(feature = "scripting")]
            AppDockTab::ScriptingConsole => {
                self.scripts
                    .console_dock_ui(ui, self.store, self.ingest_sender, self.metrics);
            }
            AppDockTab::Logging => {
                let action = self.logging_dock.ui(ui, self.logs);
                if action.clear {
                    self.actions.clear_logs = true;
                }
            }
        }
    }

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        false
    }
}

/// Fixed footprint of a toolbar icon button. Allocating an explicit size
/// keeps the button's rect independent of the SVG's load state, so the
/// toolbar's height can't change between egui's layout passes (which would
/// otherwise shift every panel below and spam "changed id between passes").
const ICON_BUTTON_SIZE: egui::Vec2 = egui::vec2(28.0, 24.0);

fn next_legend_position(
    position: crate::settings::LegendPosition,
) -> crate::settings::LegendPosition {
    match position {
        crate::settings::LegendPosition::TopLeft => crate::settings::LegendPosition::TopRight,
        crate::settings::LegendPosition::TopRight => crate::settings::LegendPosition::BottomLeft,
        crate::settings::LegendPosition::BottomLeft => crate::settings::LegendPosition::BottomRight,
        crate::settings::LegendPosition::BottomRight => crate::settings::LegendPosition::TopLeft,
    }
}

fn legend_position_icon(position: crate::settings::LegendPosition) -> egui::ImageSource<'static> {
    match position {
        crate::settings::LegendPosition::TopLeft => crate::icons::dice_top_left(),
        crate::settings::LegendPosition::TopRight => crate::icons::dice_top_right(),
        crate::settings::LegendPosition::BottomLeft => crate::icons::dice_bottom_left(),
        crate::settings::LegendPosition::BottomRight => crate::icons::dice_bottom_right(),
    }
}

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
    let image = toolbar_icon_image(icon, tint);
    ui.push_id(salt, |ui| {
        ui.add_sized(
            ICON_BUTTON_SIZE,
            egui::Button::image(image).selected(active),
        )
    })
    .inner
}

fn hover_mode_menu_button(
    ui: &mut egui::Ui,
    salt: &str,
    icon: egui::ImageSource<'_>,
    tint: egui::Color32,
    active: bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let image = toolbar_icon_image(icon, tint);
    ui.push_id(salt, |ui| {
        egui::containers::menu::MenuButton::from_button(
            egui::Button::image(image)
                .selected(active)
                .min_size(ICON_BUTTON_SIZE),
        )
        .ui(ui, add_contents)
        .0
    })
    .inner
}

fn toolbar_icon_image(icon: egui::ImageSource<'_>, tint: egui::Color32) -> egui::Image<'_> {
    let image = egui::Image::new(icon)
        .fit_to_exact_size(egui::vec2(18.0, 18.0))
        .tint(tint);
    image
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int32Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::diagnostics::{Diag, DiagRecord};
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;

    use super::*;

    #[test]
    fn auto_open_diagnostics_only_fires_for_newer_seqs_when_enabled() {
        // First diagnostic ever seen opens the dock.
        assert!(should_auto_open_diagnostics(true, None, 0));
        // A strictly newer seq opens it again.
        assert!(should_auto_open_diagnostics(true, Some(3), 4));
        // The same (or older) seq does not — avoids reopening after the user closes.
        assert!(!should_auto_open_diagnostics(true, Some(4), 4));
        assert!(!should_auto_open_diagnostics(true, Some(5), 4));
        // Disabled never opens, even for a brand-new diagnostic.
        assert!(!should_auto_open_diagnostics(false, None, 0));
        assert!(!should_auto_open_diagnostics(false, Some(3), 9));
    }

    #[test]
    fn combined_load_state_keeps_parser_label_separate_without_duplicates() {
        let state = combined_load_state(
            true,
            vec![
                "flight.bin".to_owned(),
                "running raw.py on flight.bin".to_owned(),
            ],
            Some("running raw.py on flight.bin"),
        );

        assert_eq!(state.native_labels, vec!["flight.bin"]);
        assert_eq!(
            state.parser_label.as_deref(),
            Some("running raw.py on flight.bin")
        );
        assert!(state.parser_active);
    }

    #[test]
    fn combined_load_state_is_active_for_parser_only_work() {
        let state = combined_load_state(false, Vec::new(), Some("running raw.py on sample.dat"));

        assert!(state.active);
        assert!(state.native_labels.is_empty());
        assert_eq!(
            state.parser_label.as_deref(),
            Some("running raw.py on sample.dat")
        );
        assert!(state.parser_active);
    }

    #[test]
    fn timeline_range_uses_empty_session_placeholder_without_data() {
        assert_eq!(
            timeline_range_for_ui(None),
            TimeRange::new(0, 10_000_000).unwrap()
        );
    }

    #[test]
    fn fit_to_view_defaults_on_for_new_sessions() {
        assert!(DEFAULT_FIT_VIEW_ALL);
    }

    #[test]
    fn clear_current_layout_resets_layout_and_vehicle_state() {
        let mut workspace = Workspace::new();
        let mut playback = Playback {
            speed: 2.0,
            follow_live: true,
            ..Playback::default()
        };
        let mut view = Some(ViewX::new(10, 20));
        let mut view_fitted = true;
        let mut fit_view_all = false;
        let mut marker_us = Some(42);
        let mut markers = crate::markers::Markers::new();
        markers.add_at(42);
        let mut vehicles = vec![crate::vehicle::VehicleConfig {
            source: delog_core::identity::SourceId(0),
            label: "Vehicle".into(),
            show: true,
            pos: crate::vehicle::PosMapping::Ned {
                north: delog_core::identity::FieldId(0),
                east: delog_core::identity::FieldId(1),
                down: delog_core::identity::FieldId(2),
                reference: None,
            },
            ori: crate::vehicle::OriMapping::Static,
            model: crate::vehicle::ModelKind::Cone,
            color: egui::Color32::WHITE,
            path_color: egui::Color32::WHITE,
            scale: 1.0,
        }];
        let mut vehicle_dialog = crate::vehicle_dialog::VehicleDialog::default();
        vehicle_dialog.open = true;
        let mut vehicle_revision = 7;
        let mut traj_dirty = false;

        DelogApp::clear_current_layout_state(
            &mut workspace,
            &mut playback,
            &mut view,
            &mut view_fitted,
            &mut fit_view_all,
            &mut marker_us,
            &mut markers,
            &mut vehicles,
            &mut vehicle_dialog,
            &mut vehicle_revision,
            &mut traj_dirty,
        );

        assert!(workspace.focused_first_field().is_none());
        assert_eq!(playback.speed, 1.0);
        assert!(!playback.follow_live);
        assert_eq!(view, None);
        assert!(!view_fitted);
        assert_eq!(fit_view_all, DEFAULT_FIT_VIEW_ALL);
        assert_eq!(marker_us, None);
        assert!(markers.as_slice().is_empty());
        assert!(vehicles.is_empty());
        assert!(!vehicle_dialog.open);
        assert_eq!(vehicle_revision, 8);
        assert!(traj_dirty);
    }

    #[test]
    fn empty_stat_formats_as_a_dash() {
        assert_eq!(format_stat(f64::NAN), "-");
    }

    #[test]
    fn stats_window_title_is_only_the_field_label() {
        assert_eq!(
            field_stats_window_title("flight / ATT.Roll"),
            "flight / ATT.Roll"
        );
    }

    #[test]
    fn field_stats_tabs_use_egui_dock() {
        let source = include_str!("app.rs");
        let field_stats_window = source
            .split("fn show_field_stats_window")
            .nth(1)
            .expect("field stats window should exist")
            .split("fn stats_grid")
            .next()
            .expect("field stats window should precede stats grid");

        assert!(field_stats_window.contains("egui_dock::DockArea::new"));
        assert!(source.contains("impl egui_dock::TabViewer for FieldStatsTabViewer"));
        assert!(!field_stats_window.contains("selectable_label"));
    }

    #[test]
    fn field_stats_window_is_compact_and_uses_resizable_tables() {
        let source = include_str!("app.rs");
        let field_stats = source
            .split("fn show_field_stats_window")
            .nth(1)
            .expect("field stats window should exist")
            .split("fn field_stats_window_title")
            .next()
            .expect("field stats helpers should precede title helper");

        assert!(field_stats.contains(".default_width(320.0)"));
        assert!(field_stats.contains("fn stats_range_header"));
        assert_eq!(field_stats.matches("stats_range_header(ui,").count(), 2);
        assert!(field_stats.contains("TableBuilder::new(ui)"));
        assert!(field_stats.contains(".resizable(true)"));
        assert!(field_stats.contains("Column::remainder()"));
        assert!(!field_stats.contains("egui::Grid::new(id)"));
    }

    #[test]
    fn diagnostics_export_doc_includes_source_labels_and_counts() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let snapshot = StoreSnapshot::from_registry(&identity, [], 7).unwrap();
        let doc = diagnostics_export_doc(
            vec![DiagRecord {
                seq: 42,
                diag: Diag::warning("ulog-dropout", "dropout")
                    .with_source(source)
                    .at_time(1_000_000)
                    .at_byte(99),
                count: 3,
            }],
            &snapshot,
        );

        let json = serde_json::to_value(&doc).unwrap();
        let record = &json["records"][0];
        assert_eq!(json["delog_diagnostics"], 1);
        assert_eq!(record["seq"], 42);
        assert_eq!(record["count"], 3);
        assert_eq!(record["severity"], "warning");
        assert_eq!(record["code"], "ulog-dropout");
        assert_eq!(record["source_id"], source.0);
        assert_eq!(record["source_label"], "flight");
        assert_eq!(record["time_us"], 1_000_000);
        assert_eq!(record["byte_offset"], 99);
        assert_eq!(record["message"], "dropout");
    }

    #[test]
    fn profiling_export_doc_carries_metrics_resources_and_traces() {
        let metrics = delog_core::metrics::MetricsRegistry::new();
        metrics.record("upload_bytes", 4096.0);
        metrics.add("gpu_full_uploads", 2);
        let snapshot = PerformanceSnapshot {
            metrics: metrics.snapshot(),
            resources: ResourceSummary {
                gpu_buffer_count: 3,
                gpu_bytes: 1024,
                cache_ready_count: 1,
                cache_cpu_bytes: 2048,
            },
            traces: vec![TraceSummary {
                label: "GPS.alt".into(),
                samples: Some(1000),
                visible_samples: Some(500),
                cache_cpu_bytes: 8000,
                gpu_bytes: 8000,
            }],
        };

        let doc = profiling_export_doc(&snapshot, 123);
        let json = serde_json::to_value(&doc).unwrap();

        assert_eq!(json["delog_profiling"], 1);
        assert_eq!(json["exported_at_unix_ms"], 123);
        assert_eq!(json["resources"]["gpu_buffer_count"], 3);
        assert_eq!(json["resources"]["cache_cpu_bytes"], 2048);

        // Metrics come through sorted by name (snapshot() guarantees it).
        let names: Vec<&str> = json["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["gpu_full_uploads", "upload_bytes"]);
        let upload = &json["metrics"][1];
        assert_eq!(upload["name"], "upload_bytes");
        assert_eq!(upload["last"], 4096.0);
        let full = &json["metrics"][0];
        assert_eq!(full["counter"], 2);

        assert_eq!(json["traces"][0]["label"], "GPS.alt");
        assert_eq!(json["traces"][0]["visible_samples"], 500);
    }

    #[test]
    fn field_metadata_includes_schema_rows_and_effective_range() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        identity.set_source_offset_us(source, 250);
        let topic = identity.add_topic(source, "GPS").unwrap();
        let lat = identity.add_field(topic, "Lat").unwrap();
        identity.add_field(topic, "Alt").unwrap();

        let schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [
                    FieldSchema::new("Lat", DataType::Int32, Some("deg"), 1e-7)
                        .unwrap()
                        .with_description("latitude"),
                    FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![1_000, 2_000, 3_000]),
                vec![
                    Arc::new(Int32Array::from(vec![1, 2, 3])) as ArrayRef,
                    Arc::new(Float64Array::from(vec![10.0, 11.0, 12.0])) as ArrayRef,
                ],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 9).unwrap();

        let meta = field_metadata(&snapshot, lat).unwrap();

        assert_eq!(meta.title, "flight / GPS.Lat");
        assert_eq!(meta.source_label, "flight");
        assert_eq!(meta.topic_name, "GPS");
        assert_eq!(meta.field_name, "Lat");
        assert_eq!(meta.dtype, "i32");
        assert_eq!(meta.unit.as_deref(), Some("deg"));
        assert_eq!(meta.description.as_deref(), Some("latitude"));
        assert_eq!(meta.multiplier, 1e-7);
        assert_eq!(meta.rows, 3);
        assert_eq!(meta.source_offset_us, 250);
        assert_eq!(meta.range, TimeRange::new(1_250, 3_250));
    }

    #[test]
    fn source_metadata_tabs_use_egui_dock() {
        let source = include_str!("app.rs");
        let source_metadata = source
            .split("fn show_source_metadata_window")
            .nth(1)
            .expect("source metadata window should exist")
            .split("fn show_source_metadata_tab")
            .next()
            .expect("source metadata window should precede tab renderer");

        assert!(source_metadata.contains("egui_dock::DockArea::new"));
        assert!(source.contains("impl egui_dock::TabViewer for SourceMetadataTabViewer"));
        assert!(!source_metadata.contains("selectable_value"));
    }

    #[test]
    fn source_metadata_tables_use_resizable_table_builders() {
        let source = include_str!("app.rs");
        let source_metadata = source
            .split("fn show_source_metadata_tab")
            .nth(1)
            .expect("source metadata tab renderer should exist")
            .split("fn show_field_stats_window")
            .next()
            .expect("source metadata should precede field stats");

        assert!(source_metadata.contains("source_metadata_summary_table"));
        assert!(source_metadata.contains("source_metadata_params_table"));
        assert!(source_metadata.contains("source_metadata_markers_table"));
        assert_eq!(source_metadata.matches("TableBuilder::new(ui)").count(), 3);
        assert_eq!(source_metadata.matches(".resizable(true)").count(), 3);
        assert!(source_metadata.matches("Column::remainder()").count() >= 3);
        assert!(!source_metadata.contains("egui::Grid::new"));
    }

    #[test]
    fn tile_cache_repaints_on_clear_submission_and_while_action_is_pending() {
        assert!(tile_cache_needs_repaint(true, false));
        assert!(tile_cache_needs_repaint(false, true));
        assert!(!tile_cache_needs_repaint(false, false));
    }
}
