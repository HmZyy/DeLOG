use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

pub mod commands;
pub mod command_palette;
pub mod context_header;
pub mod global_plot_toolbar;
pub mod inspector;
pub mod empty_state;

use delog_cache::CacheManager;
use delog_core::diagnostics::{DiagRecord, Severity};
use delog_core::time::TimeRange;
use egui_extras::{Column, TableBuilder};
use serde::Serialize;

use crate::plotting::browser::{self, BrowserFilterCache, BrowserModel};
use crate::ui::diagnostics::DiagnosticsDock;
use crate::ui::docks::{AppDockController, AppDockTab};
use crate::plotting::field_stats::{FieldStatsController, StatsTab};
use crate::plotting::gpu::GpuBridge;
use crate::config::layout::doc::{LayoutDoc, LayoutError};
use crate::shell::layout_apply::{LayoutApply, LoadOutcome, PendingLayout};
use crate::ingest::live::ConnectionDialog;
use crate::ui::logging::{LogLevel, LogRecord, LoggingDock, PendingLog};
use crate::map::worker::{CacheActionKind, CacheActionStatus, TileManager};
use crate::ui::performance::{PerformanceDock, PerformanceSnapshot, ResourceSummary, TraceSummary};
use crate::plotting::plot::ViewX;
#[cfg(feature = "scripting")]
use crate::scripting::scripts;
use crate::session::session::Session;
use crate::config::settings::{AppSettings, RenderMode, SettingsDialog, TileCacheUiState};
use crate::sync::sync_window::SyncWindow;

fn tile_cache_needs_repaint(clear_submitted: bool, cache_action_pending: bool) -> bool {
    clear_submitted || cache_action_pending
}

fn keep_active_loads_repainting(ctx: &egui::Context, has_active_loads: bool) {
    if has_active_loads {
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}
use crate::plotting::timeline::Playback;
use crate::shell::workspace::{PlotServices, Workspace};

struct TrajectoryBuildResult {
    epoch: u64,
    vehicle_revision: u64,
    trajectories: Vec<crate::scene3d::vehicle::VehicleTrajectory>,
}

type LayoutImportResult = Result<LayoutDoc, LayoutError>;
type LayoutExportResult = Result<std::path::PathBuf, LayoutError>;
type DiagnosticsExportResult = Result<std::path::PathBuf, String>;
type ProfilingExportResult = Result<std::path::PathBuf, String>;

struct DataExportSuccess {
    path: std::path::PathBuf,
    format: crate::export::data_export::ExportFormat,
    rows: u64,
}

enum DataExportEvent {
    Started(crate::export::data_export::ActiveExport),
    Written { id: u64, success: DataExportSuccess },
    Cancelled { id: u64, path: std::path::PathBuf },
    Failed { id: u64, error: String },
}
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
    parquet_import: crate::ingest::parquet_import::ParquetImportUi,
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
    markers: crate::plotting::markers::Markers,
    snap_playhead: bool,
    frame: u64,
    last_epoch: u64,
    origin_us: i64,
    /// `None` when idle/event-driven, so the badge doesn't show a misleading
    /// rate built from a single stale frame.
    fps_ema: Option<f32>,
    last_frame_at: Option<Instant>,
    /// Picked on a worker thread - the dialog must never block the UI thread.
    picked_files: mpsc::Receiver<PickedFiles>,
    picked_files_tx: mpsc::Sender<PickedFiles>,
    imported_layouts: mpsc::Receiver<LayoutImportResult>,
    imported_layouts_tx: mpsc::Sender<LayoutImportResult>,
    exported_layouts: mpsc::Receiver<LayoutExportResult>,
    exported_layouts_tx: mpsc::Sender<LayoutExportResult>,
    exported_diagnostics: mpsc::Receiver<DiagnosticsExportResult>,
    exported_diagnostics_tx: mpsc::Sender<DiagnosticsExportResult>,
    exported_kml: mpsc::Receiver<Result<String, String>>,
    exported_kml_tx: mpsc::Sender<Result<String, String>>,
    message_popups: Vec<crate::ui::message_popup::MessagePopup>,
    exported_profiling: mpsc::Receiver<ProfilingExportResult>,
    exported_profiling_tx: mpsc::Sender<ProfilingExportResult>,
    data_export: crate::export::data_export::DataExportState,
    data_export_tx: mpsc::Sender<DataExportEvent>,
    data_export_rx: mpsc::Receiver<DataExportEvent>,
    data_exports: Vec<crate::export::data_export::ActiveExport>,
    next_data_export_id: u64,
    image_export_writes: mpsc::Receiver<crate::export::image_export::PngWriteRequest>,
    image_export_writes_tx: mpsc::Sender<crate::export::image_export::PngWriteRequest>,
    pending_image_capture: Option<crate::export::image_export::PendingImageCapture>,
    queued_image_capture: Option<crate::export::image_export::ImageCaptureIntent>,
    next_image_capture_id: u64,
    image_clipboard: Option<arboard::Clipboard>,
    browser_collapsed: bool,
    inspector: inspector::InspectorState,
    shell_emphasis: context_header::ShellEmphasis,
    command_palette: command_palette::CommandPaletteState,
    docks: AppDockController,
    diagnostics_dock: DiagnosticsDock,
    last_diagnostic_seq: Option<u64>,
    logging_dock: LoggingDock,
    logs: Vec<LogRecord>,
    next_log_seq: u64,
    log_started_at: Instant,
    performance_dock: PerformanceDock,
    markers_dock: crate::plotting::markers::MarkersDock,
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
    sync_window: Option<SyncWindow>,
    dataflow: crate::dataflow::window::DataFlowUi,
    generate_markers_dialog: Option<crate::shell::generate_markers::GenerateMarkersDialog>,
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
    vehicles: Vec<crate::scene3d::vehicle::VehicleConfig>,
    vehicle_dialog: crate::session::vehicle_dialog::VehicleDialog,
    /// Parallel to `vehicles`, rebuilt on a worker when the data epoch or
    /// vehicle set changes.
    vehicle_trajectories: Vec<crate::scene3d::vehicle::VehicleTrajectory>,
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
        let settings = crate::config::layout::doc::load_app_settings();
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
        let (data_export_tx, data_export_rx) = mpsc::channel();
        let (image_export_writes_tx, image_export_writes) = mpsc::channel();
        let (parquet_import, parquet_selection) = crate::ingest::parquet_import::ParquetImportUi::new();
        let session = Session::new(cc.egui_ctx.clone(), parquet_selection);
        // Shared metrics registry so cache metrics land in the same dock.
        let caches = CacheManager::new().with_metrics(std::sync::Arc::clone(session.metrics()));
        Self {
            session,
            parquet_import,
            #[cfg(feature = "scripting")]
            scripts: {
                let config_dir =
                    crate::config::layout::doc::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
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
            markers: crate::plotting::markers::Markers::new(),
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
            data_export: crate::export::data_export::DataExportState::default(),
            data_export_tx,
            data_export_rx,
            data_exports: Vec::new(),
            next_data_export_id: 1,
            image_export_writes,
            image_export_writes_tx,
            pending_image_capture: None,
            queued_image_capture: None,
            next_image_capture_id: 1,
            image_clipboard: None,
            browser_collapsed: false,
            inspector: inspector::InspectorState::default(),
            shell_emphasis: context_header::ShellEmphasis::default(),
            command_palette: command_palette::CommandPaletteState::default(),
            docks: AppDockController::new_empty(),
            diagnostics_dock: DiagnosticsDock::default(),
            last_diagnostic_seq: None,
            logging_dock: LoggingDock::default(),
            logs: Vec::new(),
            next_log_seq: 0,
            log_started_at: Instant::now(),
            performance_dock: PerformanceDock::default(),
            markers_dock: crate::plotting::markers::MarkersDock::default(),
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
            sync_window: None,
            dataflow: crate::dataflow::window::DataFlowUi::new(),
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
            vehicle_dialog: crate::session::vehicle_dialog::VehicleDialog::default(),
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

    fn open_dock(&mut self, tab: AppDockTab) {
        self.set_legacy_dock_open(tab, true);
        self.docks.open_or_focus(tab);
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
    fn spawn_open_dialog(&self, ctx: &egui::Context, parser: Option<&str>) {
        let tx = self.picked_files_tx.clone();
        let ctx = ctx.clone();
        let parser = parser.map(str::to_owned);
        std::thread::Builder::new()
            .name("delog-open-dialog".into())
            .spawn(move || {
                let dialog = match parser.as_deref() {
                    Some(name) => rfd::FileDialog::new()
                        .add_filter("All files", &["*"])
                        .set_title(format!("Open with {}", parser_label(name))),
                    None => rfd::FileDialog::new()
                        .add_filter(
                            "Flight logs",
                            &["bin", "BIN", "ulg", "ulog", "tlog", "parquet"],
                        )
                        .add_filter("All files", &["*"])
                        .set_title("Open flight logs"),
                };
                if let Some(paths) = dialog.pick_files() {
                    let _ = tx.send(PickedFiles {
                        paths,
                        parser,
                    });
                    ctx.request_repaint();
                }
            })
            .expect("spawn file dialog thread");
    }

    fn handle_picked_files(&mut self) {
        while let Ok(picked) = self.picked_files.try_recv() {
            for path in picked.paths {
                self.session.open_path(path, picked.parser.clone());
            }
        }
    }

    fn spawn_png_export_dialog(
        &self,
        ctx: &egui::Context,
        kind: crate::export::image_export::ImageCaptureKind,
        png_bytes: Vec<u8>,
    ) {
        let tx = self.image_export_writes_tx.clone();
        let ctx = ctx.clone();
        let file_name = match kind {
            crate::export::image_export::ImageCaptureKind::Workspace => "workspace.png",
            crate::export::image_export::ImageCaptureKind::Plot => "plot.png",
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
                    let _ = tx.send(crate::export::image_export::PngWriteRequest::new(path, png_bytes));
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
        kind: crate::export::image_export::ImageCaptureKind,
        image: &egui::ColorImage,
    ) {
        let what = match kind {
            crate::export::image_export::ImageCaptureKind::Workspace => "workspace",
            crate::export::image_export::ImageCaptureKind::Plot => "plot",
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
        match crate::export::image_export::copy_image_to_clipboard(clipboard, image) {
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
        intent: crate::export::image_export::ImageCaptureIntent,
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
        action: crate::export::image_export::ImageCaptureAction,
        kind: crate::export::image_export::ImageCaptureKind,
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
        self.pending_image_capture = Some(crate::export::image_export::PendingImageCapture {
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
            let Some(id) = crate::export::image_export::screenshot_request_id(&user_data) else {
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
            let Some(cropped) = crate::export::image_export::crop_color_image(
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
                crate::export::image_export::ImageCaptureAction::Copy => {
                    self.copy_captured_image_to_clipboard(pending.kind, &cropped);
                }
                crate::export::image_export::ImageCaptureAction::Export => {
                    match crate::export::image_export::encode_png(&cropped) {
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
                        .push(crate::ui::message_popup::MessagePopup::info(
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
                        .push(crate::ui::message_popup::MessagePopup::error(
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

        while let Ok(event) = self.data_export_rx.try_recv() {
            let finished = match event {
                DataExportEvent::Started(active) => {
                    self.data_exports.push(active);
                    continue;
                }
                DataExportEvent::Written { id, success } => {
                    self.session
                        .push_diagnostic(delog_core::diagnostics::Diag::info(
                            "data-export",
                            format!(
                                "exported {} rows as {} to {}",
                                success.rows,
                                success.format.label(),
                                success.path.display()
                            ),
                        ));
                    id
                }
                DataExportEvent::Cancelled { id, path } => {
                    self.session
                        .push_diagnostic(delog_core::diagnostics::Diag::info(
                            "data-export",
                            format!("cancelled export to {}", path.display()),
                        ));
                    id
                }
                DataExportEvent::Failed { id, error } => {
                    self.session
                        .push_diagnostic(delog_core::diagnostics::Diag::error(
                            "data-export",
                            error,
                        ));
                    id
                }
            };
            self.data_exports.retain(|active| active.id != finished);
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
        match crate::shell::layout_apply::load_doc(doc.clone(), snapshot) {
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
        let json = crate::config::layout::doc::doc_json(&doc)?;
        if !force && self.last_session_autosave_json.as_deref() == Some(json.as_str()) {
            self.last_session_autosave = Instant::now();
            return Ok(false);
        }

        crate::config::layout::doc::save_session_json(&json)?;
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
        markers: &mut crate::plotting::markers::Markers,
        vehicles: &mut Vec<crate::scene3d::vehicle::VehicleConfig>,
        vehicle_dialog: &mut crate::session::vehicle_dialog::VehicleDialog,
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
        *markers = crate::plotting::markers::Markers::new();
        vehicles.clear();
        *vehicle_dialog = crate::session::vehicle_dialog::VehicleDialog::default();
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
                    label: crate::plotting::legend::trace_label(snapshot, field),
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
                    .map(|v| crate::scene3d::vehicle::build_trajectory(&snapshot, v))
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
        crate::shell::layout_apply::current_doc(crate::shell::layout_apply::CurrentLayout {
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
        match crate::config::layout::doc::save_named(name, &doc) {
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
                    let result = crate::config::layout::doc::export_doc(&path, &doc).map(|_| path);
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
                    let result = crate::config::layout::doc::import_doc(&path);
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
            crate::export::kml_export::build_kml(snapshot, &self.vehicles, &self.vehicle_trajectories);
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

    fn spawn_data_export(
        &mut self,
        ctx: &egui::Context,
        snapshot: &std::sync::Arc<delog_core::snapshot::StoreSnapshot>,
        all_fields: &[crate::export::data_export::ExportField],
        request: crate::export::data_export::DataExportRequest,
    ) {
        let id = self.next_data_export_id;
        self.next_data_export_id += 1;
        let chosen = match crate::export::data_export::resolve_export_fields(&request.fields, all_fields) {
            Ok(chosen) => chosen,
            Err(error) => {
                let _ = self.data_export_tx.send(DataExportEvent::Failed {
                    id,
                    error: error.to_string(),
                });
                ctx.request_repaint();
                return;
            }
        };
        let origin_us = snapshot
            .global_time_range()
            .map(|range| range.min_us)
            .unwrap_or(0);
        let snapshot = std::sync::Arc::clone(snapshot);
        let tx = self.data_export_tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("delog-data-export".into())
            .spawn(move || {
                let format = request.format;
                let picked = rfd::FileDialog::new()
                    .add_filter(format.filter_name(), &[format.extension()])
                    .add_filter("All files", &["*"])
                    .set_title(format.dialog_title())
                    .set_file_name(format.default_file_name())
                    .save_file();
                let Some(path) = picked else { return };
                let progress = crate::export::data_export::ExportProgress::default();
                let cancel = delog_core::parse_ctl::CancelToken::new();
                let _ = tx.send(DataExportEvent::Started(
                    crate::export::data_export::ActiveExport::new(
                        id,
                        &path,
                        progress.clone(),
                        cancel.clone(),
                    ),
                ));
                ctx.request_repaint();

                let ctl = crate::export::data_export::ExportCtl::new(cancel, move |fraction| {
                    progress.set(fraction);
                });
                let event = match crate::export::data_export::write_export_file(
                    &path,
                    format,
                    &snapshot,
                    &chosen,
                    request.window,
                    request.mode,
                    origin_us,
                    &ctl,
                ) {
                    Ok(rows) => DataExportEvent::Written {
                        id,
                        success: DataExportSuccess { path, format, rows },
                    },
                    Err(crate::export::data_export::DataExportError::Cancelled) => {
                        DataExportEvent::Cancelled { id, path }
                    }
                    Err(error) => DataExportEvent::Failed {
                        id,
                        error: error.to_string(),
                    },
                };
                let _ = tx.send(event);
                ctx.request_repaint();
            })
            .expect("spawn data export thread");
    }

    fn load_layout(&mut self, name: &str, snapshot: &delog_core::snapshot::StoreSnapshot) {
        match crate::config::layout::doc::load_named_doc(name) {
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

    fn command_context(
        &self,
        snapshot: &delog_core::snapshot::StoreSnapshot,
    ) -> commands::CommandContext {
        let offline_source_count = snapshot
            .sources
            .iter()
            .filter(|source| {
                !source.entry.removed
                    && source.entry.kind == delog_core::identity::SourceKind::File
            })
            .count();
        commands::CommandContext {
            has_data: snapshot.global_time_range().is_some(),
            offline_source_count,
            live_link_count: self.session.live_statuses().len(),
            has_active_tasks: self.session.has_active_loads(),
            scripting_enabled: cfg!(feature = "scripting"),
        }
    }

    fn command_presentations(
        &mut self,
        snapshot: &delog_core::snapshot::StoreSnapshot,
    ) -> Vec<commands::CommandPresentation> {
        use commands::{
            AppCommand, CommandAvailability, CommandPresentation,
        };
        let context = self.command_context(snapshot);
        debug_assert_eq!(commands::dynamic_command_families().len(), 4);
        let mut dynamic = Vec::new();
        for name in self.session.parser_names() {
            dynamic.push(CommandPresentation {
                command: AppCommand::OpenWithParser(name.to_owned()),
                label: format!("Open with {}…", parser_label(name)),
                shortcut: None,
                availability: CommandAvailability::Enabled,
            });
        }
        for name in crate::config::layout::doc::list_layouts() {
            dynamic.push(CommandPresentation {
                command: AppCommand::LoadNamedLayout(name.clone()),
                label: format!("Load layout: {name}"),
                shortcut: None,
                availability: CommandAvailability::Enabled,
            });
        }
        for (index, status) in self.session.live_statuses().into_iter().enumerate() {
            dynamic.push(CommandPresentation {
                command: AppCommand::DisconnectLink(index),
                label: format!("Disconnect {}", status.endpoint),
                shortcut: None,
                availability: CommandAvailability::Enabled,
            });
        }
        #[cfg(feature = "scripting")]
        {
            let script_availability = if self.scripts.ordinary_dispatch_enabled() {
                CommandAvailability::Enabled
            } else {
                CommandAvailability::Disabled("Another script is already running")
            };
            for name in self.scripts.script_names() {
                dynamic.push(CommandPresentation {
                    command: AppCommand::RunScript(name.clone()),
                    label: format!("Run script: {name}"),
                    shortcut: None,
                    availability: script_availability.clone(),
                });
            }
            if let Ok(names) = self.scripts.parser_names() {
                let parser_availability = if self.scripts.parser_dispatch_enabled() {
                    CommandAvailability::Enabled
                } else {
                    CommandAvailability::Disabled("Another parser is already running")
                };
                for name in names {
                    dynamic.push(CommandPresentation {
                        command: AppCommand::OpenWithParser(name.clone()),
                        label: format!("Parse file with {name}…"),
                        shortcut: None,
                        availability: parser_availability.clone(),
                    });
                }
            }
        }
        commands::present_commands(&context, dynamic)
    }

    fn command_palette_entries(
        presentations: Vec<commands::CommandPresentation>,
    ) -> Vec<command_palette::PaletteEntry> {
        use commands::AppCommand;
        let mut entries = command_palette::CommandPaletteState::entries(presentations);
        for entry in &mut entries {
            let terms = match entry.command {
                AppCommand::Static(id) => {
                    let spec = id.spec();
                    entry
                        .search_text
                        .push_str(&format!(" {:?} {:?}", spec.group, spec.routes));
                    spec.search_terms
                }
                _ => "dynamic recent named",
            };
            entry.search_text.push(' ');
            entry.search_text.push_str(terms);
        }
        entries
    }

    fn dispatch_command(
        &mut self,
        command: commands::AppCommand,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        snapshot: &delog_core::snapshot::StoreSnapshot,
        range: TimeRange,
    ) {
        use commands::{AppCommand, CommandId};
        match command {
            AppCommand::ToggleShellEmphasis => {
                self.shell_emphasis = self.shell_emphasis.toggle();
            }
            AppCommand::OpenWithParser(name) => {
                let built_in = self.session.parser_names().iter().any(|known| *known == name);
                if built_in {
                    self.spawn_open_dialog(ctx, Some(&name));
                } else {
                    #[cfg(feature = "scripting")]
                    let _ = self.scripts.request_open(ctx, &name);
                }
            }
            AppCommand::RunScript(name) => {
                #[cfg(feature = "scripting")]
                let _ = self.scripts.run_named(
                    &name,
                    self.session.store(),
                    self.session.ingest_sender(),
                    Arc::clone(self.session.metrics()),
                );
                #[cfg(not(feature = "scripting"))]
                let _ = name;
            }
            AppCommand::LoadNamedLayout(name) => self.load_layout(&name, snapshot),
            AppCommand::DisconnectLink(index) => self.session.stop_live(index),
            AppCommand::Static(id) => match id {
                CommandId::Open => self.spawn_open_dialog(ctx, None),
                CommandId::ConnectLive => self.show_connection_dialog = true,
                CommandId::SyncSources => self.sync_window = SyncWindow::open(snapshot),
                CommandId::DisconnectLive => {
                    if !self.session.live_statuses().is_empty() {
                        self.session.stop_live(0);
                    }
                }
                CommandId::CancelTasks => {
                    self.session.cancel_all();
                    #[cfg(feature = "scripting")]
                    self.scripts.cancel_parsers();
                }
                CommandId::ExportData => self.data_export.open(),
                CommandId::ExportDiagnostics => self.spawn_export_diagnostics_dialog(
                    ctx,
                    self.session.diagnostic_records(),
                    snapshot,
                ),
                CommandId::ExportProfiling => {
                    self.spawn_export_profiling_dialog(ctx, frame, snapshot)
                }
                CommandId::ExportWorkspacePng => self.queue_image_capture(
                    ctx,
                    crate::export::image_export::ImageCaptureIntent::workspace(
                        crate::export::image_export::ImageCaptureAction::Export,
                        self.frame,
                    ),
                ),
                CommandId::ToggleDataBrowser => self.browser_collapsed = !self.browser_collapsed,
                CommandId::ToggleInspector => self.inspector.open = !self.inspector.open,
                CommandId::ToggleScene3d => self.workspace.toggle_scene_pane(),
                CommandId::OpenDiagnostics => self.open_dock(AppDockTab::Diagnostics),
                CommandId::OpenPerformance => self.open_dock(AppDockTab::Performance),
                CommandId::OpenMarkers => self.open_dock(AppDockTab::Markers),
                CommandId::OpenScripting => {
                    #[cfg(feature = "scripting")]
                    self.open_dock(AppDockTab::ScriptingConsole);
                }
                CommandId::OpenLogging => self.open_dock(AppDockTab::Logging),
                CommandId::SaveLayout => self.save_layout_dialog.open = true,
                CommandId::LoadLayout => {
                    self.load_layout_dialog.layouts =
                        crate::config::layout::doc::list_layouts();
                    self.load_layout_dialog.selected = None;
                    self.load_layout_dialog.open = true;
                }
                CommandId::ManageLayouts => self.open_layout_manager(),
                CommandId::ClearLayout => self.clear_current_layout(),
                CommandId::ImportLayout => self.spawn_import_layout_dialog(ctx),
                CommandId::ExportLayout => self.spawn_export_layout_dialog(ctx, snapshot),
                CommandId::EqualizePlots => self.workspace.equalize_plot_heights(),
                CommandId::OpenDataFlow => self.dataflow.open = true,
                CommandId::OpenScriptEditor => {
                    #[cfg(feature = "scripting")]
                    {
                        self.scripts.open = true;
                    }
                }
                CommandId::OpenScriptVariables => {
                    #[cfg(feature = "scripting")]
                    {
                        self.scripts.variables_open = true;
                    }
                }
                CommandId::OpenParserEditor => {
                    #[cfg(feature = "scripting")]
                    self.scripts.open_parser_editor();
                }
                CommandId::TogglePlayheadSnap => self.snap_playhead = !self.snap_playhead,
                CommandId::AddMeasuringMarker => {
                    self.marker_us = self.marker_us.is_none().then_some(self.playback.t_us)
                }
                CommandId::CycleLegendPosition => {
                    self.settings.plot.legend_position =
                        next_legend_position(self.settings.plot.legend_position)
                }
                CommandId::ToggleLegends => {
                    let visible = !self.workspace.all_plot_legends_visible();
                    self.workspace.set_all_plot_legends(visible);
                }
                CommandId::OpenSettings => self.settings_dialog.open(),
                CommandId::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                CommandId::TogglePlayback => self.playback.toggle(),
                CommandId::JumpStart => self.playback.jump_start(range),
                CommandId::JumpEnd => {
                    if self.session.has_live_links() {
                        self.lock_to_live(range);
                    } else {
                        self.playback.jump_end(range);
                    }
                }
                CommandId::StepLeft | CommandId::StepRight => {
                    let reference = self.workspace.focused_first_field();
                    let right = id == CommandId::StepRight;
                    let target = crate::plotting::timeline::step_target(
                        snapshot,
                        reference,
                        self.playback.t_us,
                        right,
                    );
                    self.playback.scrub(target, range);
                }
                CommandId::AddMarker => {
                    self.markers.add_at(self.playback.t_us);
                }
            },
        }
    }

    fn refresh_layout_manager(&mut self, preferred: Option<String>) {
        self.layout_manager_dialog.layouts = crate::config::layout::doc::list_layouts();
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
                match crate::config::layout::doc::rename_named(&from, &display) {
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
                match crate::config::layout::doc::duplicate_named(&from, &display) {
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
            LayoutManagerAction::Delete(name) => match crate::config::layout::doc::delete_named(&name) {
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
        let _ = crate::config::layout::doc::save_app_settings(&self.settings);
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
        // cache lifecycle + epoch handling, trajectory builds and autosave -
        // none of it inside a panel scope. `ui_prelude` captures this block so
        // `frame_total − Σ(ui_*)` no longer hides it as an unattributed gap.
        let ui_prelude_timer = self.session.metrics().scope("ui_prelude");
        self.handle_picked_files();
        self.handle_layout_io_results();
        self.parquet_import.poll_requests();
        keep_active_loads_repainting(ui.ctx(), self.session.has_active_loads());
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

            // Advance the playhead - the single time authority.
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
        let range = timeline_range_for_ui(global_range);
        let command_presentations = self.command_presentations(&snapshot);
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
        let load = if load_state.active {
            let mut labels = load_state.native_labels;
            if let Some(label) = load_state.parser_label {
                labels.push(label);
            }
            context_header::LoadStatusView::Active {
                label: if labels.is_empty() {
                    "Working…".to_owned()
                } else {
                    labels.join(" · ")
                },
                progress: (!load_state.parser_active)
                    .then(|| self.session.overall_progress())
                    .flatten(),
            }
        } else {
            context_header::LoadStatusView::Idle
        };
        let active_source_label = snapshot
            .sources
            .iter()
            .find(|source| !source.entry.removed)
            .map(|source| source.entry.label.clone());
        let file_sources = snapshot
            .sources
            .iter()
            .filter(|source| {
                !source.entry.removed
                    && source.entry.kind == delog_core::identity::SourceKind::File
            })
            .count();
        let row_count = snapshot
            .topics
            .iter()
            .filter(|topic| !topic.entry.removed)
            .filter_map(|topic| topic.store.as_ref().map(|store| store.rows as usize))
            .fold(0_usize, usize::saturating_add);
        let live_statuses = self
            .session
            .live_statuses()
            .into_iter()
            .enumerate()
            .map(|(index, status)| {
                let recording = recording_status(&status);
                context_header::LiveSummary {
                    index,
                    endpoint: status.endpoint.to_string(),
                    state: status.state.label().to_owned(),
                    rx_frames: status.link.rx_frames,
                    rows: status.ingest.rows,
                    recording: (!recording.is_empty()).then_some(recording),
                }
            })
            .collect();
        let adaptive_shell = empty_state::shell_model(
            empty_state::ShellModelInput {
                file_sources,
                live_links: self.session.live_statuses().len(),
                rows: row_count,
            },
            self.shell_emphasis,
        );
        debug_assert!(adaptive_shell.browser_available && adaptive_shell.workspace_visible);
        let header_model = context_header::HeaderModel {
            emphasis: self.shell_emphasis,
            active_source_label,
            live_statuses,
            load,
            fps: self.settings.show_fps.then_some(self.fps_ema).flatten(),
            theme: self.settings.theme,
        };
        let header_commands = egui::Panel::top("context_header")
            .show_inside(ui, |ui| {
                context_header::show(ui, &header_model, &command_presentations)
            })
            .inner;
        drop(ui_menu_timer);
        for command in header_commands {
            self.dispatch_command(command, ui.ctx(), frame, &snapshot, range);
        }

        let wants_keyboard = ui.ctx().egui_wants_keyboard_input();
        let ctrl_k = ui.ctx().input(|input| {
            input.modifiers.command && input.key_pressed(egui::Key::K)
        });
        if command_palette::should_toggle_palette(ctrl_k, wants_keyboard) {
            if self.command_palette.open {
                self.command_palette.open = false;
            } else {
                self.command_palette.open();
            }
        }

        // Existing shortcuts and palette selections share dispatch. Commands
        // stay dormant while an editor or the palette owns keyboard input.
        if !wants_keyboard && !self.command_palette.open {
            use commands::AppCommand;
            let shortcuts = ui.ctx().input(|input| {
                SHORTCUT_KEYS
                    .iter()
                    .copied()
                    .filter(|key| input.key_pressed(*key))
                    .filter_map(|key| command_for_shortcut(key, input.modifiers.command))
                    .collect::<Vec<_>>()
            });
            for command in shortcuts {
                self.dispatch_command(AppCommand::Static(command), ui.ctx(), frame, &snapshot, range);
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
            let action = crate::plotting::timeline::ui(
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
                            let icon = egui::Image::new(crate::ui::icons::panel_left_open())
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
                    self.inspector.focus_statistics(vec![field]);
                }
                if let Some(field) = browser_response.generate_markers {
                    let title = crate::plotting::legend::trace_label(&snapshot, field);
                    self.generate_markers_dialog =
                        Some(crate::shell::generate_markers::GenerateMarkersDialog::open(
                            &snapshot, field, title,
                        ));
                }
            });
            self.browser_model = Some((epoch, model));
        }
        drop(ui_browser_timer);
        if let Some(t_us) =
            show_source_metadata_window(ui.ctx(), &snapshot, &mut self.source_metadata_dialog)
            && let Some(range) = snapshot.global_time_range()
        {
            self.playback.scrub(t_us, range);
        }
        show_field_metadata_window(ui.ctx(), &snapshot, &mut self.field_metadata_dialog);
        show_field_stats_window(
            ui.ctx(),
            &snapshot,
            self.view,
            &mut self.caches,
            &mut self.field_stats,
        );
        if self.inspector.open {
            let focused_fields = self.workspace.focused_fields();
            let inspected_trace = match self.inspector.selection {
                inspector::InspectorSelection::Trace { tile_id, field } => {
                    self.workspace.trace_ref(tile_id, field).cloned()
                }
                _ => None,
            };
            let inspector_events = egui::Panel::right("analysis_inspector")
                .resizable(true)
                .default_size(320.0)
                .min_size(260.0)
                .max_size(520.0)
                .show_inside(ui, |ui| {
                    inspector::show(
                        ui,
                        &mut self.inspector,
                        &snapshot,
                        self.playback.t_us,
                        self.hover_mode,
                        &focused_fields,
                        diagnostics.len(),
                        &mut self.field_stats,
                        inspected_trace.as_ref(),
                        &header_model.live_statuses,
                    )
                })
                .inner;
            for event in inspector_events {
                match event {
                    inspector::InspectorEvent::SetTraceColor {
                        tile_id,
                        field,
                        color,
                    } => {
                        self.workspace.set_trace_color(tile_id, field, color);
                    }
                    inspector::InspectorEvent::SetTraceMode {
                        tile_id,
                        field,
                        mode,
                    } => {
                        self.workspace.set_trace_mode(tile_id, field, mode);
                    }
                    inspector::InspectorEvent::SetTraceWidth {
                        tile_id,
                        field,
                        width_px,
                    } => {
                        self.workspace.set_trace_width(tile_id, field, width_px);
                    }
                    inspector::InspectorEvent::SetTraceLabel {
                        tile_id,
                        field,
                        label,
                    } => {
                        self.workspace.set_trace_label(tile_id, field, label);
                    }
                }
            }
        }
        for (t_us, name, color) in crate::shell::generate_markers::generate_markers_window(
            ui.ctx(),
            &mut self.generate_markers_dialog,
        ) {
            self.markers.push_loaded(t_us, name, color, String::new());
        }

        if self.data_export.open {
            let model = self
                .browser_model
                .as_ref()
                .map(|(_, m)| m.clone())
                .unwrap_or_default();
            let fields = crate::export::data_export::available_fields(&snapshot, &model);
            let full = snapshot
                .global_time_range()
                .map(|r| (r.min_us, r.max_us))
                .unwrap_or((0, 1));
            let visible = self.view.map(|v| (v.min_us, v.max_us)).unwrap_or(full);
            if let Some(req) = crate::export::data_export::dialog_ui(
                ui.ctx(),
                &mut self.data_export,
                &fields,
                visible,
                full,
            ) {
                self.spawn_data_export(ui.ctx(), &snapshot, &fields, req);
                self.data_export.open = false;
            }
        }

        let ui_workspace_timer = self.session.metrics().scope("ui_workspace");
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            // The workspace renders even before any log loads, so plots can be
            // arranged and the 3D view opened on an empty session.

            let toolbar_model = global_plot_toolbar::GlobalPlotToolbarModel {
                cursor_sampling: self.hover_mode,
                playhead_snap: self.snap_playhead,
                all_legends_visible: self.workspace.all_plot_legends_visible(),
                legend_position: self.settings.plot.legend_position,
            };
            for action in global_plot_toolbar::show(ui, &toolbar_model) {
                match action {
                    global_plot_toolbar::GlobalPlotToolbarAction::FitAll => {
                        if let Some(range) = snapshot.global_time_range() {
                            self.view = Some(ViewX::from_range(range));
                            self.fit_view_all = true;
                        }
                    }
                    global_plot_toolbar::GlobalPlotToolbarAction::SetCursorSampling(mode) => {
                        self.hover_mode = mode;
                    }
                    global_plot_toolbar::GlobalPlotToolbarAction::TogglePlayheadSnap => {
                        self.snap_playhead = !self.snap_playhead;
                    }
                    global_plot_toolbar::GlobalPlotToolbarAction::ToggleAllLegends => {
                        let visible = !self.workspace.all_plot_legends_visible();
                        self.workspace.set_all_plot_legends(visible);
                    }
                    global_plot_toolbar::GlobalPlotToolbarAction::CycleLegendPosition => {
                        self.settings.plot.legend_position =
                            next_legend_position(self.settings.plot.legend_position);
                    }
                    global_plot_toolbar::GlobalPlotToolbarAction::EqualizePlotHeights => {
                        self.workspace.equalize_plot_heights();
                    }
                }
            }

            let workspace_rect = ui.available_rect_before_wrap();

            if adaptive_shell.show_empty_state {
                let parsers: Vec<_> = self
                    .session
                    .parser_names()
                    .iter()
                    .map(|name| (*name, crate::shell::app::parser_label(name)))
                    .collect();
                let actions = egui::Area::new(egui::Id::new("workspace-empty-state"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(workspace_rect.center())
                    .pivot(empty_state::PIVOT)
                    .show(ui.ctx(), |ui| {
                        empty_state::show(ui, adaptive_shell.emphasis, &parsers)
                    })
                    .inner;
                for action in actions {
                    let command = match action {
                        empty_state::EmptyStateAction::Open => {
                            Some(commands::AppCommand::Static(commands::CommandId::Open))
                        }
                        empty_state::EmptyStateAction::OpenWith(parser) => {
                            Some(commands::AppCommand::OpenWithParser(parser))
                        }
                        empty_state::EmptyStateAction::ConnectLive => Some(
                            commands::AppCommand::Static(commands::CommandId::ConnectLive),
                        ),
                    };
                    if let Some(command) = command {
                        self.dispatch_command(command, ui.ctx(), frame, &snapshot, range);
                    }
                }
            }

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
                        playhead_us: snapshot.global_time_range().map(|_| self.playback.t_us),
                        playing: self.playback.playing,
                        vehicles: &self.vehicles,
                        trajectories: &self.vehicle_trajectories,
                        traj_generation: self.traj_vehicle_revision,
                        shared_y_gutter: self.workspace.shared_y_gutter,
                        plot_display: self.settings.plot,
                        markers: self.markers.as_slice(),
                    };
                    let mut behavior = crate::shell::workspace::Behavior::new(services);
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
                    if let Some(mv) = actions.legend_move {
                        let field = self.workspace.apply_legend_move(mv);
                        self.caches.request(field, &snapshot);
                        handled_workspace_drop = true;
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
                    if let Some(fields) = actions.inspect_field_stats {
                        self.field_stats.open_fields(fields.clone());
                        self.inspector.focus_statistics(fields);
                    }
                    if let Some((tile_id, field)) = actions.inspect_trace {
                        self.inspector.focus_trace(tile_id, field);
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

        // Render synchronization previews after the workspace has reset the
        // per-frame uniform allocator and retained its own GPU buffers. The
        // synchronization callback can then safely append private uniforms and
        // upload fields that are not plotted in the workspace.
        if let Some(mut sync_window) = self.sync_window.take() {
            sync_window.reconcile(&snapshot);
            if sync_window.pending_is_authoritative(&snapshot) {
                sync_window.mark_applied(&snapshot);
            }
            let response =
                sync_window.show(ui.ctx(), &snapshot, &self.gpu, frame, &mut self.caches);
            if let Some(offsets) = response.apply {
                if self.session.set_source_offsets(offsets).is_err() {
                    sync_window.apply_dispatch_failed();
                }
            }
            if sync_window.open {
                self.sync_window = Some(sync_window);
            }
        }

        {
            #[cfg(feature = "scripting")]
            if self.dataflow.open && self.dataflow.has_script_node() {
                let host = self.scripts.engine_flow_host(
                    self.session.store(),
                    self.session.ingest_sender(),
                    Arc::clone(self.session.metrics()),
                );
                self.dataflow.set_script_host(Some(host));
            }
            let sender = self.session.ingest_sender();
            let live_connected = self.session.has_connected_live();
            let dataflow_settings = self.settings.dataflow;
            let mut logs = Vec::new();
            if self.dataflow.open {
                logs.extend(
                    self.dataflow
                        .show(ui.ctx(), &snapshot, &sender, live_connected),
                );
            }
            logs.extend(self.dataflow.drive(
                ui.ctx(),
                &snapshot,
                &sender,
                live_connected,
                dataflow_settings,
            ));
            for (level, message) in logs {
                self.push_log(crate::ui::logging::log(level, message));
            }
        }

        // Floating windows/dialogs + overlays; drops with the function (still
        // inside `frame_total`, after every other section).
        let _ui_windows_timer = self.session.metrics().scope("ui_windows");
        self.parquet_import.show(ui.ctx());
        crate::export::data_export::progress_ui(ui.ctx(), &self.data_exports);
        self.show_layout_windows(ui.ctx());
        crate::ui::message_popup::show_all(&mut self.message_popups, ui.ctx());
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
            && let Err(err) = crate::config::layout::doc::save_app_settings(&self.settings)
        {
            self.session
                .push_diagnostic(delog_core::diagnostics::Diag::error(
                    "settings-save",
                    err.to_string(),
                ));
        }
        if crate::session::vehicle_dialog::show(
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
            if let Err(err) = crate::config::layout::doc::save_app_settings(&self.settings) {
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
                self.settings.scripting.use_original_timestamps,
            );
            for command in self.scripts.take_marker_commands() {
                self.markers.apply_script_command(command);
            }
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

        let palette_entries = Self::command_palette_entries(command_presentations);
        if let Some(command) = self.command_palette.show(ui.ctx(), &palette_entries) {
            self.dispatch_command(command, ui.ctx(), frame, &snapshot, range);
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
    original_source: Option<String>,
    original_topic: Option<String>,
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
        original_source: store
            .schema
            .provenance()
            .map(|provenance| provenance.original_source().to_owned()),
        original_topic: store
            .schema
            .provenance()
            .map(|provenance| provenance.original_topic().to_owned()),
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
                    if let Some(original_source) = meta.original_source.as_deref() {
                        ui.strong("Original source");
                        ui.label(original_source);
                        ui.end_row();
                    }
                    if let Some(original_topic) = meta.original_topic.as_deref() {
                        ui.strong("Original topic");
                        ui.label(original_topic);
                        ui.end_row();
                    }
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
) -> Option<i64> {
    let source_id = (*selected)?;
    let Some(source) = snapshot
        .source(source_id)
        .filter(|source| !source.entry.removed)
    else {
        *selected = None;
        return None;
    };

    let mut jump_to_time_us = None;
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
                jump_to_time_us: None,
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
            jump_to_time_us = viewer.jump_to_time_us;
            ui.data_mut(|d| d.insert_temp(tab_id, active_source_metadata_tab(&mut dock_state)));
        });

    if !open {
        *selected = None;
    }
    jump_to_time_us
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
    jump_to_time_us: Option<i64>,
}

impl egui_dock::TabViewer for SourceMetadataTabViewer<'_> {
    type Tab = SourceMetaTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.label().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        if let Some(t_us) = show_source_metadata_tab(ui, self.snapshot, self.source_id, *tab) {
            self.jump_to_time_us = Some(t_us);
        }
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
) -> Option<i64> {
    let source = snapshot
        .source(source_id)
        .filter(|source| !source.entry.removed)?;

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
                    .filter(|param| crate::plotting::browser::matches_query(&query, &param.name))
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
                return source_metadata_markers_table(
                    ui,
                    source_id,
                    &source.entry.meta.auto_markers,
                    source.entry.offset_us,
                );
            }
        }
    }
    None
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
                    header.col(|ui| {
                        ui.strong("Default");
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
                        row.col(|ui| match param.default.as_deref() {
                            Some(default) => {
                                ui.label(default);
                            }
                            None => {
                                ui.weak("-");
                            }
                        });
                    });
                });
        });
}

fn source_metadata_markers_table(
    ui: &mut egui::Ui,
    source_id: delog_core::identity::SourceId,
    markers: &[delog_core::identity::AutoMarker],
    offset_us: i64,
) -> Option<i64> {
    let mut jump_to_time_us = None;
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
                            match delog_core::time::effective_time_us(marker.time_us, offset_us) {
                                Some(t_us) => {
                                    if ui
                                        .button(format!("{:.3}s", t_us as f64 / 1e6))
                                        .on_hover_text("Jump playhead to this message")
                                        .clicked()
                                    {
                                        jump_to_time_us = Some(t_us);
                                    }
                                }
                                None => {
                                    ui.weak("-");
                                }
                            }
                        });
                        row.col(|ui| match marker.level {
                            Some(level) => {
                                ui.label(level.to_string());
                            }
                            None => {
                                ui.weak("-");
                            }
                        });
                        row.col(|ui| {
                            ui.label(marker.text.as_str());
                        });
                    });
                });
        });
    jump_to_time_us
}

fn show_field_stats_window(
    ctx: &egui::Context,
    snapshot: &Arc<delog_core::snapshot::StoreSnapshot>,
    view: Option<ViewX>,
    caches: &mut CacheManager,
    controller: &mut FieldStatsController,
) {
    if controller.fields().is_empty() {
        return;
    }

    let now = Instant::now();
    if controller.tab() == StatsTab::Visible
        && let Some(view) = view
    {
        controller.request_all(
            snapshot.epoch,
            view.min_us,
            view.max_us,
            Arc::clone(snapshot),
            now,
        );
    }
    controller.poll(now);

    let tab = controller.tab();
    let rows = field_stats_rows(snapshot, caches, view, controller);
    let visible_range = view.map(|view| TimeRange {
        min_us: view.min_us,
        max_us: view.max_us,
    });
    let global_range = snapshot.global_time_range();
    let updating = controller.is_any_updating();
    if updating {
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    let mut open = true;
    egui::Window::new("Field stats")
        .id(egui::Id::new("field_stats"))
        .open(&mut open)
        .collapsible(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .default_width(900.0)
        .resizable(true)
        .show(ctx, |ui| {
            let mut dock_state = field_stats_dock_state(tab);
            let mut viewer = FieldStatsTabViewer {
                snapshot,
                rows: &rows,
                visible_range,
                global_range,
                updating,
            };
            egui_dock::DockArea::new(&mut dock_state)
                .id(egui::Id::new("field_stats_dock_area"))
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

#[derive(Clone)]
struct FieldStatsRow {
    field: delog_core::identity::FieldId,
    name: String,
    suffix: String,
    stats: Option<delog_core::analysis::FieldStats>,
    provisional: Option<(f64, f64)>,
    updating: bool,
    state: Option<String>,
}

fn field_stats_rows(
    snapshot: &delog_core::snapshot::StoreSnapshot,
    caches: &mut CacheManager,
    view: Option<ViewX>,
    controller: &FieldStatsController,
) -> Vec<FieldStatsRow> {
    controller
        .fields()
        .iter()
        .copied()
        .map(|field| {
            let name = crate::plotting::legend::trace_label(snapshot, field);
            let (suffix, unavailable) = match field_unit(snapshot, field) {
                Some(Some(unit)) => (format!(" {unit}"), false),
                Some(None) => (String::new(), false),
                None => (String::new(), true),
            };
            let stats = controller
                .result_for(field)
                .copied()
                .or_else(|| controller.stale_result_for(field).copied());
            let provisional = view.and_then(|view| {
                let cache = caches.get(field)?;
                provisional_visible_stats(cache, view)
            });
            let state = unavailable
                .then(|| "Unavailable".to_owned())
                .or_else(|| controller.error_for(field).map(str::to_owned));
            FieldStatsRow {
                field,
                name,
                suffix,
                stats,
                provisional,
                updating: controller.is_updating_for(field),
                state,
            }
        })
        .collect()
}

fn provisional_visible_stats(cache: &delog_cache::TraceCache, view: ViewX) -> Option<(f64, f64)> {
    let (x0, x1) = view.seconds(cache.origin_us);
    let (a, b) = cache.index_range(x0, x1);
    let mm = cache.pyramid.query(&cache.xy, a, b);
    mm.is_finite().then_some((
        f64::from(mm.min) + cache.y_origin(),
        f64::from(mm.max) + cache.y_origin(),
    ))
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
    rows: &'a [FieldStatsRow],
    visible_range: Option<TimeRange>,
    global_range: Option<TimeRange>,
    updating: bool,
}

impl egui_dock::TabViewer for FieldStatsTabViewer<'_> {
    type Tab = StatsTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.label().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            StatsTab::Visible => {
                show_visible_field_stats_tab(ui, self.visible_range, self.rows, self.updating)
            }
            StatsTab::Global => {
                show_global_field_stats_tab(ui, self.snapshot, self.global_range, self.rows)
            }
        }
    }

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        false
    }
}

fn show_visible_field_stats_tab(
    ui: &mut egui::Ui,
    range: Option<TimeRange>,
    rows: &[FieldStatsRow],
    updating: bool,
) {
    stats_range_header(ui, range, updating);
    stats_table(ui, "visible_field_stats_table", rows);
}

fn show_global_field_stats_tab(
    ui: &mut egui::Ui,
    snapshot: &delog_core::snapshot::StoreSnapshot,
    range: Option<TimeRange>,
    visible_rows: &[FieldStatsRow],
) {
    stats_range_header(ui, range, false);
    let rows: Vec<_> = visible_rows
        .iter()
        .cloned()
        .map(|mut row| {
            row.provisional = None;
            row.updating = false;
            if row.state.as_deref() != Some("Unavailable") {
                match delog_core::analysis::global_field_stats(snapshot, row.field) {
                    Ok(Some(stats)) => {
                        row.stats = Some(stats);
                        row.state = None;
                    }
                    Ok(None) => {
                        row.stats = None;
                        row.state = Some("Not numeric".into());
                    }
                    Err(err) => {
                        row.stats = None;
                        row.state = Some(err.to_string());
                    }
                }
            }
            row
        })
        .collect();
    stats_table(ui, "global_field_stats_table", &rows);
}

fn stats_table(ui: &mut egui::Ui, id: &'static str, rows: &[FieldStatsRow]) {
    let row_height = table_row_height(ui);
    egui::ScrollArea::horizontal()
        .id_salt((id, "horizontal"))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(880.0);
            TableBuilder::new(ui)
                .id_salt(id)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .auto_shrink([false, false])
                .column(Column::initial(180.0).at_least(120.0).clip(true))
                .column(Column::initial(80.0).at_least(64.0))
                .column(Column::initial(96.0).at_least(72.0))
                .column(Column::initial(96.0).at_least(72.0))
                .column(Column::initial(96.0).at_least(72.0))
                .column(Column::initial(96.0).at_least(72.0))
                .column(Column::initial(80.0).at_least(64.0))
                .column(Column::remainder().at_least(80.0))
                .header(row_height, |mut header| {
                    header.col(|ui| {
                        ui.strong("Name");
                    });
                    header.col(|ui| {
                        ui.strong("Samples");
                    });
                    header.col(|ui| {
                        ui.strong("Min");
                    });
                    header.col(|ui| {
                        ui.strong("Max");
                    });
                    header.col(|ui| {
                        ui.strong("Mean");
                    });
                    header.col(|ui| {
                        ui.strong("Std dev");
                    });
                    header.col(|ui| {
                        ui.strong("Missing");
                    });
                    header.col(|ui| {
                        ui.strong("Rate");
                    });
                })
                .body(|body| {
                    body.rows(row_height, rows.len(), |mut table_row| {
                        let row = &rows[table_row.index()];
                        let values = stats_row_values(row);
                        table_row.col(|ui| {
                            ui.label(&row.name);
                            if row.updating {
                                ui.weak("updating...");
                            }
                        });
                        table_row.col(|ui| {
                            ui.label(&values[0]);
                        });
                        for value in &values[1..] {
                            table_row.col(|ui| {
                                ui.label(value);
                            });
                        }
                    });
                });
        });
}

fn stats_row_values(row: &FieldStatsRow) -> [String; 7] {
    if let Some(state) = &row.state {
        return [
            state.clone(),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
        ];
    }

    let min = row
        .stats
        .map(|stats| stats.min)
        .or(row.provisional.map(|p| p.0));
    let max = row
        .stats
        .map(|stats| stats.max)
        .or(row.provisional.map(|p| p.1));
    [
        row.stats
            .map(|stats| stats.count.to_string())
            .unwrap_or_else(|| "-".into()),
        stat_with_unit(min, &row.suffix),
        stat_with_unit(max, &row.suffix),
        stat_with_unit(row.stats.map(|stats| stats.mean), &row.suffix),
        stat_with_unit(row.stats.map(|stats| stats.stddev), &row.suffix),
        row.stats
            .map(|stats| stats.missing_count.to_string())
            .unwrap_or_else(|| "-".into()),
        row.stats
            .and_then(|stats| stats.rate_hz)
            .map(|rate| format!("{} Hz", format_stat(rate)))
            .unwrap_or_else(|| "-".into()),
    ]
}

fn field_unit(
    snapshot: &delog_core::snapshot::StoreSnapshot,
    field_id: delog_core::identity::FieldId,
) -> Option<Option<String>> {
    let field = snapshot
        .fields
        .get(field_id.index())
        .filter(|field| field.id == field_id && !field.removed)?;
    let topic = snapshot
        .topic(field.topic)
        .filter(|topic| !topic.entry.removed)?;
    Some(
        topic
            .store
            .as_ref()
            .and_then(|store| store.schema.field_by_name(&field.name))
            .and_then(|schema| schema.unit.clone()),
    )
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

struct PickedFiles {
    paths: Vec<std::path::PathBuf>,
    parser: Option<String>,
}

fn parser_label(name: &str) -> &str {
    match name {
        "ardupilot-bin" => "ArduPilot DataFlash",
        "ulog" => "PX4 ULog",
        "tlog" => "MAVLink Telemetry",
        "parquet" => "Parquet",
        other => other,
    }
}

const SHORTCUT_KEYS: &[egui::Key] = &[
    egui::Key::F1,
    egui::Key::F2,
    egui::Key::F3,
    egui::Key::F9,
    egui::Key::F12,
    egui::Key::Space,
    egui::Key::Home,
    egui::Key::End,
    egui::Key::ArrowLeft,
    egui::Key::ArrowRight,
    egui::Key::S,
    egui::Key::L,
    egui::Key::M,
];

fn command_for_shortcut(
    key: egui::Key,
    command_modifier: bool,
) -> Option<commands::CommandId> {
    use commands::CommandId;
    match (key, command_modifier) {
        (egui::Key::S, true) => Some(CommandId::SaveLayout),
        (egui::Key::L, true) => Some(CommandId::LoadLayout),
        (egui::Key::F1, _) => Some(CommandId::OpenDiagnostics),
        (egui::Key::F2, _) => Some(CommandId::OpenPerformance),
        (egui::Key::F3, _) => Some(CommandId::OpenMarkers),
        (egui::Key::F9, _) => Some(CommandId::OpenScripting),
        (egui::Key::F12, _) => Some(CommandId::OpenLogging),
        (egui::Key::Space, _) => Some(CommandId::TogglePlayback),
        (egui::Key::Home, _) => Some(CommandId::JumpStart),
        (egui::Key::End, _) => Some(CommandId::JumpEnd),
        (egui::Key::ArrowLeft, _) => Some(CommandId::StepLeft),
        (egui::Key::ArrowRight, _) => Some(CommandId::StepRight),
        (egui::Key::M, _) => Some(CommandId::AddMarker),
        _ => None,
    }
}

fn format_time_us(value: i64) -> String {
    format!("{:.3} s", value as f64 / 1e6)
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
    markers_dock: &'a mut crate::plotting::markers::MarkersDock,
    markers: &'a mut crate::plotting::markers::Markers,
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

fn next_legend_position(
    position: crate::config::settings::LegendPosition,
) -> crate::config::settings::LegendPosition {
    match position {
        crate::config::settings::LegendPosition::TopLeft => crate::config::settings::LegendPosition::TopRight,
        crate::config::settings::LegendPosition::TopRight => crate::config::settings::LegendPosition::BottomLeft,
        crate::config::settings::LegendPosition::BottomLeft => crate::config::settings::LegendPosition::BottomRight,
        crate::config::settings::LegendPosition::BottomRight => crate::config::settings::LegendPosition::TopLeft,
    }
}

#[cfg(test)]
mod tests;
