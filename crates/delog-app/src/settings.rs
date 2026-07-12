//! Settings dialog state and tab rendering.

use crate::map::provider::{MapProviderId, provider};
use crate::theme::ThemeChoice;

const DEFAULT_TILE_CACHE_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;
const MIN_TILE_CACHE_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TILE_CACHE_LIMIT_BYTES: u64 = 64 * 1024 * 1024 * 1024;

fn default_tile_cache_limit_bytes() -> u64 {
    DEFAULT_TILE_CACHE_LIMIT_BYTES
}

fn default_decimate_threshold() -> f32 {
    8.0
}
fn default_line_aa_px() -> f32 {
    1.0
}
fn default_gap_factor() -> f32 {
    5.0
}
fn default_true() -> bool {
    true
}
fn default_opacity() -> f32 {
    0.85
}
fn default_marker_line_width() -> f32 {
    1.5
}
fn default_marker_shade_opacity() -> f32 {
    0.12
}
fn default_text_label_cap() -> usize {
    512
}
fn default_text_label_spacing() -> f32 {
    4.0
}
fn default_text_line_width() -> f32 {
    1.0
}
fn default_text_line_opacity() -> f32 {
    0.3
}
fn default_font_size() -> f32 {
    14.0
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub theme: ThemeChoice,
    #[serde(default)]
    pub render: RenderTuning,
    #[serde(default)]
    pub show_fps: bool,
    #[serde(default)]
    pub render_mode: RenderMode,
    #[serde(default = "default_true")]
    pub vsync: bool,
    #[serde(default)]
    pub live_connection: LiveConnectionSettings,
    #[serde(default)]
    pub scene3d: Scene3dSettings,
    #[serde(default)]
    pub plot: PlotDisplay,
    #[serde(default)]
    pub font: FontOverride,
    #[serde(default)]
    pub auto_open_diagnostics: bool,
    #[serde(default)]
    pub scripting: ScriptingSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
            render: RenderTuning::default(),
            show_fps: false,
            render_mode: RenderMode::default(),
            vsync: true,
            live_connection: LiveConnectionSettings::default(),
            scene3d: Scene3dSettings::default(),
            plot: PlotDisplay::default(),
            font: FontOverride::default(),
            auto_open_diagnostics: false,
            scripting: ScriptingSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FontOverride {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_font_size")]
    pub size: f32,
    #[serde(default)]
    pub monospace: bool,
}

impl Default for FontOverride {
    fn default() -> Self {
        Self {
            enabled: false,
            size: default_font_size(),
            monospace: false,
        }
    }
}

impl FontOverride {
    pub fn apply(self, ctx: &egui::Context) {
        let font_id = self.enabled.then(|| {
            let family = if self.monospace {
                egui::FontFamily::Monospace
            } else {
                egui::FontFamily::Proportional
            };
            egui::FontId::new(self.size.clamp(4.0, 40.0), family)
        });
        ctx.all_styles_mut(|style| style.override_font_id = font_id.clone());
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegendPosition {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl LegendPosition {
    pub const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::TopLeft => "Top left",
            Self::TopRight => "Top right",
            Self::BottomLeft => "Bottom left",
            Self::BottomRight => "Bottom right",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerDeltaReadout {
    #[default]
    Legend,
    Hover,
}

impl MarkerDeltaReadout {
    pub const ALL: [Self; 2] = [Self::Legend, Self::Hover];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Legend => "Legend",
            Self::Hover => "Hover readout",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlotDisplay {
    #[serde(default)]
    pub legend_position: LegendPosition,
    #[serde(default = "default_opacity")]
    pub legend_opacity: f32,
    #[serde(default)]
    pub hover_show_field_name: bool,
    #[serde(default)]
    pub hover_show_time: bool,
    #[serde(default = "default_opacity")]
    pub hover_opacity: f32,
    #[serde(default)]
    pub marker_delta_readout: MarkerDeltaReadout,
    #[serde(default = "default_opacity")]
    pub marker_line_opacity: f32,
    #[serde(default = "default_marker_line_width")]
    pub marker_line_width: f32,
    #[serde(default = "default_true")]
    pub marker_show_label: bool,
    #[serde(default)]
    pub marker_shade_regions: bool,
    #[serde(default = "default_marker_shade_opacity")]
    pub marker_shade_opacity: f32,
    #[serde(default = "default_text_label_cap")]
    pub text_label_cap: usize,
    #[serde(default = "default_true")]
    pub text_labels_bottom_up: bool,
    #[serde(default = "default_text_label_spacing")]
    pub text_label_spacing: f32,
    #[serde(default = "default_text_line_width")]
    pub text_line_width: f32,
    #[serde(default = "default_text_line_opacity")]
    pub text_line_opacity: f32,
}

impl Default for PlotDisplay {
    fn default() -> Self {
        Self {
            legend_position: LegendPosition::default(),
            legend_opacity: 1.0,
            hover_show_field_name: false,
            hover_show_time: false,
            hover_opacity: 1.0,
            marker_delta_readout: MarkerDeltaReadout::default(),
            marker_line_opacity: default_opacity(),
            marker_line_width: default_marker_line_width(),
            marker_show_label: true,
            marker_shade_regions: true,
            marker_shade_opacity: default_marker_shade_opacity(),
            text_label_cap: default_text_label_cap(),
            text_labels_bottom_up: true,
            text_label_spacing: default_text_label_spacing(),
            text_line_width: default_text_line_width(),
            text_line_opacity: default_text_line_opacity(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GapMode {
    /// Always a single line: bridge decimated columns, drop NaNs in the raw path.
    #[default]
    Connect,
    /// Break the line at gaps; draw nothing across them.
    Cut,
    /// Break the line at gaps; bridge each gap with a dotted segment.
    Dotted,
}

impl GapMode {
    pub const ALL: [GapMode; 3] = [GapMode::Connect, GapMode::Cut, GapMode::Dotted];

    pub fn label(self) -> &'static str {
        match self {
            GapMode::Connect => "Connect (single line)",
            GapMode::Cut => "Cut",
            GapMode::Dotted => "Dotted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(from = "RenderTuningDe")]
pub struct RenderTuning {
    pub decimate_threshold: f32,
    /// pixels (0 = hard edges)
    pub line_aa_px: f32,
    pub gap_mode: GapMode,
    /// A time jump larger than this many median sample intervals is a gap.
    pub gap_factor: f32,
}

impl Default for RenderTuning {
    fn default() -> Self {
        Self {
            decimate_threshold: default_decimate_threshold(),
            line_aa_px: default_line_aa_px(),
            gap_mode: GapMode::default(),
            gap_factor: default_gap_factor(),
        }
    }
}

/// Deserialization shim: carries the field defaults and migrates the removed
/// `bridge_columns` flag (`false` meant "leave gaps" — now `Cut`).
#[derive(serde::Deserialize)]
struct RenderTuningDe {
    #[serde(default = "default_decimate_threshold")]
    decimate_threshold: f32,
    #[serde(default = "default_line_aa_px")]
    line_aa_px: f32,
    #[serde(default)]
    bridge_columns: Option<bool>,
    #[serde(default)]
    gap_mode: Option<GapMode>,
    #[serde(default = "default_gap_factor")]
    gap_factor: f32,
}

impl From<RenderTuningDe> for RenderTuning {
    fn from(de: RenderTuningDe) -> Self {
        let gap_mode = de.gap_mode.unwrap_or(match de.bridge_columns {
            Some(false) => GapMode::Cut,
            _ => GapMode::Connect,
        });
        Self {
            decimate_threshold: de.decimate_threshold,
            line_aa_px: de.line_aa_px,
            gap_mode,
            gap_factor: de.gap_factor,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RenderMode {
    Reactive,
    #[default]
    Continuous,
}

impl RenderMode {
    pub const ALL: [Self; 2] = [Self::Reactive, Self::Continuous];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Reactive => "Reactive",
            Self::Continuous => "Continuous",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoOpenVariables {
    NewlyAdded,
    EveryRun,
    Never,
}

impl AutoOpenVariables {
    pub const ALL: [Self; 3] = [Self::NewlyAdded, Self::EveryRun, Self::Never];

    pub const fn label(self) -> &'static str {
        match self {
            Self::NewlyAdded => "When a new variable is added",
            Self::EveryRun => "On every run",
            Self::Never => "Never",
        }
    }
}

impl Default for AutoOpenVariables {
    fn default() -> Self {
        Self::NewlyAdded
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoOpenScriptingConsole {
    OnOutput,
    OnErrors,
    Never,
}

impl AutoOpenScriptingConsole {
    pub const ALL: [Self; 3] = [Self::OnOutput, Self::OnErrors, Self::Never];

    pub const fn label(self) -> &'static str {
        match self {
            Self::OnOutput => "On output",
            Self::OnErrors => "On errors",
            Self::Never => "Never",
        }
    }
}

impl Default for AutoOpenScriptingConsole {
    fn default() -> Self {
        Self::OnErrors
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveConnectionMode {
    #[default]
    UdpServer,
    TcpClient,
    Serial,
}

fn default_live_host() -> String {
    "0.0.0.0".to_owned()
}
fn default_live_port() -> u16 {
    14550
}
fn default_live_serial_path() -> String {
    #[cfg(windows)]
    {
        "COM3".to_owned()
    }
    #[cfg(not(windows))]
    {
        "/dev/ttyACM0".to_owned()
    }
}
fn default_live_baud() -> u32 {
    115_200
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LiveConnectionSettings {
    #[serde(default)]
    pub mode: LiveConnectionMode,
    #[serde(default = "default_live_host")]
    pub host: String,
    #[serde(default = "default_live_port")]
    pub port: u16,
    #[serde(default = "default_live_serial_path")]
    pub serial_path: String,
    #[serde(default = "default_live_baud")]
    pub baud: u32,
    #[serde(default)]
    pub recording_enabled: bool,
    #[serde(default, alias = "recording_path")]
    pub recording_dir: String,
}

impl Default for LiveConnectionSettings {
    fn default() -> Self {
        Self {
            mode: LiveConnectionMode::UdpServer,
            host: default_live_host(),
            port: default_live_port(),
            serial_path: default_live_serial_path(),
            baud: default_live_baud(),
            recording_enabled: false,
            recording_dir: String::new(),
        }
    }
}

fn default_scene_far_clip_m() -> f32 {
    20_000.0
}
fn default_scene_max_camera_distance_m() -> f32 {
    12_000.0
}
fn default_scene_grid_cell_m() -> f32 {
    1.0
}
fn default_scene_fog_start_m() -> f32 {
    1_000.0
}
fn default_scene_fog_end_m() -> f32 {
    20_000.0
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

/// Distances are render-space metres.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Scene3dSettings {
    #[serde(default)]
    pub map_provider: MapProviderId,
    #[serde(default = "default_tile_cache_limit_bytes")]
    pub tile_cache_limit_bytes: u64,
    #[serde(default = "default_scene_far_clip_m")]
    pub far_clip_m: f32,
    #[serde(default = "default_scene_max_camera_distance_m")]
    pub max_camera_distance_m: f32,
    #[serde(default = "default_true")]
    pub show_grid: bool,
    #[serde(default = "default_true")]
    pub show_axes: bool,
    /// When set, `grid_cell_m` is ignored.
    #[serde(default = "default_true")]
    pub grid_cell_auto: bool,
    #[serde(default = "default_scene_grid_cell_m")]
    pub grid_cell_m: f32,
    #[serde(default = "default_true")]
    pub fog_enabled: bool,
    #[serde(default = "default_scene_fog_start_m")]
    pub fog_start_m: f32,
    #[serde(default = "default_scene_fog_end_m")]
    pub fog_end_m: f32,
}

impl Default for Scene3dSettings {
    fn default() -> Self {
        Self {
            map_provider: MapProviderId::None,
            tile_cache_limit_bytes: default_tile_cache_limit_bytes(),
            far_clip_m: default_scene_far_clip_m(),
            max_camera_distance_m: default_scene_max_camera_distance_m(),
            show_grid: true,
            show_axes: true,
            grid_cell_auto: true,
            grid_cell_m: default_scene_grid_cell_m(),
            fog_enabled: true,
            fog_start_m: default_scene_fog_start_m(),
            fog_end_m: default_scene_fog_end_m(),
        }
    }
}

impl Scene3dSettings {
    pub fn resolved_far_clip_m(self) -> f32 {
        finite_or(self.far_clip_m, default_scene_far_clip_m()).clamp(10.0, 1_000_000.0)
    }

    pub fn resolved_max_camera_distance_m(self) -> f32 {
        finite_or(
            self.max_camera_distance_m,
            default_scene_max_camera_distance_m(),
        )
        .clamp(0.5, self.resolved_far_clip_m().max(0.5))
    }

    pub fn resolved_grid_cell_m(self) -> f32 {
        finite_or(self.grid_cell_m, default_scene_grid_cell_m()).clamp(0.01, 100_000.0)
    }

    /// Returns (cell size, whether the shader should cross-fade LOD levels). In
    /// auto mode the cell is a continuous function of camera height so it never
    /// snaps between sizes or shimmers when orbiting tightly around an airborne
    /// vehicle; the `true` flag drives the shader's LOD cross-fade.
    pub fn resolved_grid(self, eye_height_m: f32) -> (f32, bool) {
        if self.grid_cell_auto {
            let height = finite_or(eye_height_m, 100.0).abs().max(1e-3);
            (height / 10.0, true)
        } else {
            (self.resolved_grid_cell_m(), false)
        }
    }

    pub fn resolved_fog_m(self) -> (f32, f32) {
        let start = finite_or(self.fog_start_m, default_scene_fog_start_m())
            .clamp(0.0, self.resolved_far_clip_m());
        let end = finite_or(self.fog_end_m, default_scene_fog_end_m())
            .clamp(start + 1.0, self.resolved_far_clip_m().max(start + 1.0));
        (start, end)
    }
}

#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScriptingSettings {
    #[serde(default)]
    pub auto_open_variables: AutoOpenVariables,
    #[serde(default)]
    pub auto_open_console: AutoOpenScriptingConsole,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
enum SettingsTab {
    #[default]
    General,
    Plots,
    Rendering,
    Scene3d,
    Scripting,
}

impl SettingsTab {
    const ALL: [Self; 5] = [
        Self::General,
        Self::Plots,
        Self::Rendering,
        Self::Scene3d,
        Self::Scripting,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Plots => "Plots",
            Self::Rendering => "Rendering",
            Self::Scene3d => "3D View",
            Self::Scripting => "Scripting",
        }
    }
}

#[derive(Debug)]
pub struct SettingsDialog {
    open: bool,
    dock_state: egui_dock::DockState<SettingsTab>,
}

impl Default for SettingsDialog {
    fn default() -> Self {
        Self {
            open: false,
            dock_state: egui_dock::DockState::new(SettingsTab::ALL.to_vec()),
        }
    }
}

impl SettingsDialog {
    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        settings: &mut AppSettings,
        tile_cache: TileCacheUiState,
    ) -> SettingsChange {
        if !self.open {
            return SettingsChange::default();
        }

        let mut open = self.open;
        let mut change = SettingsChange::default();
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .default_width(520.0)
            .default_height(340.0)
            .resizable(true)
            .show(ctx, |ui| {
                let mut viewer = SettingsTabViewer {
                    settings,
                    change: &mut change,
                    tile_cache,
                };
                egui_dock::DockArea::new(&mut self.dock_state)
                    .id(egui::Id::new("settings_dock_area"))
                    .style(egui_dock::Style::from_egui(ui.style().as_ref()))
                    .allowed_splits(egui_dock::AllowedSplits::None)
                    .draggable_tabs(false)
                    .tab_context_menus(false)
                    .show_close_buttons(false)
                    .show_leaf_close_all_buttons(false)
                    .show_leaf_collapse_buttons(false)
                    .show_inside(ui, &mut viewer);
            });
        self.open &= open;
        change
    }
}

struct SettingsTabViewer<'a> {
    settings: &'a mut AppSettings,
    change: &'a mut SettingsChange,
    tile_cache: TileCacheUiState,
}

impl egui_dock::TabViewer for SettingsTabViewer<'_> {
    type Tab = SettingsTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.label().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        ui.set_min_width(340.0);
        match tab {
            SettingsTab::General => {
                *self.change |= general_tab(ui, self.settings);
            }
            SettingsTab::Plots => {
                plots_tab(ui, self.settings);
            }
            SettingsTab::Rendering => {
                rendering_tab(ui, self.settings);
            }
            SettingsTab::Scene3d => {
                *self.change |= scene3d_tab(ui, self.settings, self.tile_cache);
            }
            SettingsTab::Scripting => {
                scripting_tab(ui, self.settings);
            }
        }
    }

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        false
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SettingsChange {
    pub theme_changed: bool,
    pub map_provider_changed: bool,
    pub tile_cache_limit_changed: bool,
    pub clear_tile_cache: bool,
}

impl std::ops::BitOrAssign for SettingsChange {
    fn bitor_assign(&mut self, rhs: Self) {
        self.theme_changed |= rhs.theme_changed;
        self.map_provider_changed |= rhs.map_provider_changed;
        self.tile_cache_limit_changed |= rhs.tile_cache_limit_changed;
        self.clear_tile_cache |= rhs.clear_tile_cache;
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TileCacheUiState {
    pub available: bool,
    pub usage_bytes: u64,
    pub clearing: bool,
}

impl TileCacheUiState {
    fn usage_label(self) -> String {
        if self.available {
            format_bytes(self.usage_bytes)
        } else {
            "Cache unavailable".to_owned()
        }
    }

    fn clear_enabled(self) -> bool {
        self.available && !self.clearing
    }
}

fn general_tab(ui: &mut egui::Ui, settings: &mut AppSettings) -> SettingsChange {
    let before = settings.theme;
    egui::Grid::new("settings-general-grid")
        .num_columns(2)
        .spacing(egui::vec2(16.0, 10.0))
        .show(ui, |ui| {
            ui.label("Theme");
            egui::ComboBox::from_id_salt("settings-theme-choice")
                .selected_text(settings.theme.label())
                .show_ui(ui, |ui| {
                    for theme in ThemeChoice::ALL {
                        ui.selectable_value(&mut settings.theme, theme, theme.label());
                    }
                });
            ui.end_row();

            ui.label("Show FPS counter")
                .on_hover_text("Show a frame-rate badge in the top-right corner.");
            ui.checkbox(&mut settings.show_fps, "");
            ui.end_row();

            ui.label("Auto-open diagnostics")
                .on_hover_text("Open the Diagnostics dock automatically when a new diagnostic is reported.");
            ui.checkbox(&mut settings.auto_open_diagnostics, "");
            ui.end_row();

            ui.label("Render mode").on_hover_text(
                "Reactive: event-driven, idles at 0% GPU when nothing changes. \
                     Continuous: repaints every frame (smoother for debugging, higher GPU).",
            );
            egui::ComboBox::from_id_salt("settings-render-mode")
                .selected_text(settings.render_mode.label())
                .show_ui(ui, |ui| {
                    for mode in RenderMode::ALL {
                        ui.selectable_value(&mut settings.render_mode, mode, mode.label());
                    }
                });
            ui.end_row();

            ui.label("VSync").on_hover_text(
                "Synchronize frames to the monitor's refresh rate to avoid tearing. \
                     Disable to uncap the frame rate. Takes effect after restarting DeLOG.",
            );
            ui.checkbox(&mut settings.vsync, "");
            ui.end_row();

            let f = &mut settings.font;
            ui.label("Override font")
                .on_hover_text("Force one font (size + family) for all UI text, like the egui demo. Off uses egui's per-style fonts.");
            ui.checkbox(&mut f.enabled, "");
            ui.end_row();

            ui.label("Font size");
            ui.add_enabled(
                f.enabled,
                egui::DragValue::new(&mut f.size).range(4.0..=40.0).speed(0.25),
            );
            ui.end_row();

            ui.label("Font family");
            ui.add_enabled_ui(f.enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut f.monospace, false, "Proportional");
                    ui.radio_value(&mut f.monospace, true, "Monospace");
                });
            });
            ui.end_row();
        });

    SettingsChange {
        theme_changed: settings.theme != before,
        ..Default::default()
    }
}

fn plots_tab(ui: &mut egui::Ui, settings: &mut AppSettings) {
    let p = &mut settings.plot;
    egui::Grid::new("settings-plots-grid")
        .num_columns(2)
        .spacing(egui::vec2(16.0, 10.0))
        .show(ui, |ui| {
            ui.label("Legend position")
                .on_hover_text("Corner the legend overlay anchors to inside each plot.");
            egui::ComboBox::from_id_salt("settings-legend-position")
                .selected_text(p.legend_position.label())
                .show_ui(ui, |ui| {
                    for pos in LegendPosition::ALL {
                        ui.selectable_value(&mut p.legend_position, pos, pos.label());
                    }
                });
            ui.end_row();

            ui.label("Legend background")
                .on_hover_text("Opacity of the legend's background panel. 1 = solid, 0 = fully transparent.");
            ui.add(egui::Slider::new(&mut p.legend_opacity, 0.0..=1.0));
            ui.end_row();

            ui.label("Hover: field name")
                .on_hover_text("Show the topic.field name on each hover/playhead readout row.");
            ui.checkbox(&mut p.hover_show_field_name, "");
            ui.end_row();

            ui.label("Hover: time")
                .on_hover_text("Show the time header on the hover/playhead readout.");
            ui.checkbox(&mut p.hover_show_time, "");
            ui.end_row();

            ui.label("Hover background")
                .on_hover_text("Opacity of the hover/playhead readout's background panel. 1 = solid, 0 = fully transparent.");
            ui.add(egui::Slider::new(&mut p.hover_opacity, 0.0..=1.0));
            ui.end_row();

            ui.label("Measuring marker readout")
                .on_hover_text("Where the measuring marker's per-trace value delta is shown: in the legend next to each trace, or on the hover/playhead value readout.");
            egui::ComboBox::from_id_salt("settings-marker-delta-readout")
                .selected_text(p.marker_delta_readout.label())
                .show_ui(ui, |ui| {
                    for r in MarkerDeltaReadout::ALL {
                        ui.selectable_value(&mut p.marker_delta_readout, r, r.label());
                    }
                });
            ui.end_row();

            ui.label("Marker line opacity")
                .on_hover_text("Opacity of the manual session-marker vertical lines on plots. 1 = solid, 0 = fully transparent.");
            ui.add(egui::Slider::new(&mut p.marker_line_opacity, 0.0..=1.0));
            ui.end_row();

            ui.label("Marker line width")
                .on_hover_text("Width of the manual session-marker vertical lines on plots.");
            ui.add(egui::Slider::new(&mut p.marker_line_width, 0.5..=6.0).suffix(" px"));
            ui.end_row();

            ui.label("Marker labels")
                .on_hover_text("Draw each manual session marker's label at the top of its line on plots.");
            ui.checkbox(&mut p.marker_show_label, "");
            ui.end_row();

            ui.label("Shade between markers")
                .on_hover_text("Fill each plot region from one marker to the next (or the end) with that marker's colour.");
            ui.checkbox(&mut p.marker_shade_regions, "");
            ui.end_row();

            ui.label("Marker shade opacity")
                .on_hover_text("Opacity of the inter-marker region shading. 1 = solid, 0 = fully transparent.");
            ui.add(egui::Slider::new(&mut p.marker_shade_opacity, 0.0..=1.0));
            ui.end_row();

            ui.label("Text label cap")
                .on_hover_text("Max text-annotation labels drawn per string trace in the visible window. Higher shows more at once but costs more per frame on high-rate fields.");
            ui.add(egui::Slider::new(&mut p.text_label_cap, 16..=8192).logarithmic(true));
            ui.end_row();

            ui.label("Text stacking")
                .on_hover_text("Stack text-annotation labels from the bottom of the plot upward, or from the top down.");
            egui::ComboBox::from_id_salt("settings-text-stacking")
                .selected_text(if p.text_labels_bottom_up {
                    "Bottom to top"
                } else {
                    "Top to bottom"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut p.text_labels_bottom_up, true, "Bottom to top");
                    ui.selectable_value(&mut p.text_labels_bottom_up, false, "Top to bottom");
                });
            ui.end_row();

            ui.label("Text spacing")
                .on_hover_text("Default vertical spacing between stacked text-annotation rows.");
            ui.add(egui::Slider::new(&mut p.text_label_spacing, 0.0..=40.0).suffix(" px"));
            ui.end_row();

            ui.label("Text line width")
                .on_hover_text("Width of the timestamp connector line under each text-annotation label.");
            ui.add(egui::Slider::new(&mut p.text_line_width, 0.0..=6.0).suffix(" px"));
            ui.end_row();

            ui.label("Text line opacity")
                .on_hover_text("Opacity of the timestamp connector line. 1 = solid, 0 = fully transparent.");
            ui.add(egui::Slider::new(&mut p.text_line_opacity, 0.0..=1.0));
            ui.end_row();
        });

    if reset_to_defaults_button(ui) {
        settings.plot = PlotDisplay::default();
    }
}

fn rendering_tab(ui: &mut egui::Ui, settings: &mut AppSettings) {
    let r = &mut settings.render;
    egui::Grid::new("settings-rendering-grid")
        .num_columns(2)
        .spacing(egui::vec2(16.0, 10.0))
        .show(ui, |ui| {
            ui.label("Decimate threshold")
                .on_hover_text("Switch to the min/max draw path above this many samples per pixel. Lower = decimate sooner (faster); higher = keep the true line longer.");
            ui.add(
                egui::Slider::new(&mut r.decimate_threshold, 1.0..=64.0)
                    .logarithmic(true)
                    .suffix(" smp/px"),
            );
            ui.end_row();

            ui.label("Edge anti-aliasing")
                .on_hover_text("Width of the edge feather, in pixels. 0 = hard edges, higher = smoother but softer lines.");
            ui.add(egui::Slider::new(&mut r.line_aa_px, 0.0..=3.0).suffix(" px"));
            ui.end_row();

            ui.label("Missing data")
                .on_hover_text("How gaps (null samples, or time jumps larger than the gap threshold) are drawn. Connect interpolates a single line across them; Cut breaks the line; Dotted bridges them with a dotted segment.");
            egui::ComboBox::from_id_salt("settings-gap-mode")
                .selected_text(r.gap_mode.label())
                .show_ui(ui, |ui| {
                    for mode in GapMode::ALL {
                        ui.selectable_value(&mut r.gap_mode, mode, mode.label());
                    }
                });
            ui.end_row();

            if r.gap_mode != GapMode::Connect {
                ui.label("Gap threshold")
                    .on_hover_text("A time jump larger than this many typical sample intervals counts as missing data.");
                ui.add(
                    egui::DragValue::new(&mut r.gap_factor)
                        .range(1.5..=1000.0)
                        .speed(0.5)
                        .suffix("x typical interval"),
                );
                ui.end_row();
            }
        });

    if reset_to_defaults_button(ui) {
        settings.render = RenderTuning::default();
    }
}

fn scene3d_tab(
    ui: &mut egui::Ui,
    settings: &mut AppSettings,
    tile_cache: TileCacheUiState,
) -> SettingsChange {
    let before_provider = settings.scene3d.map_provider;
    let before_limit = settings.scene3d.tile_cache_limit_bytes;
    let mut clear_tile_cache = false;
    let s = &mut settings.scene3d;
    egui::Grid::new("settings-scene3d-grid")
        .num_columns(2)
        .spacing(egui::vec2(16.0, 10.0))
        .show(ui, |ui| {
            ui.label("Map provider");
            egui::ComboBox::from_id_salt("settings-map-provider")
                .selected_text(map_provider_label(s.map_provider))
                .show_ui(ui, |ui| {
                    for id in [MapProviderId::None, MapProviderId::BingSatellite] {
                        ui.selectable_value(&mut s.map_provider, id, map_provider_label(id));
                    }
                });
            ui.end_row();

            ui.label("Tile cache limit");
            ui.add(
                egui::Slider::new(
                    &mut s.tile_cache_limit_bytes,
                    MIN_TILE_CACHE_LIMIT_BYTES..=MAX_TILE_CACHE_LIMIT_BYTES,
                )
                .logarithmic(true)
                .custom_formatter(|value, _| format_bytes(value as u64)),
            );
            ui.end_row();

            ui.label("Cache usage");
            ui.horizontal(|ui| {
                ui.label(tile_cache.usage_label());
                if ui
                    .add_enabled(
                        tile_cache.clear_enabled(),
                        egui::Button::new("Clear tile cache"),
                    )
                    .clicked()
                {
                    clear_tile_cache = true;
                }
                if tile_cache.clearing {
                    ui.spinner();
                }
            });
            ui.end_row();

            ui.label("Render distance")
                .on_hover_text("Far clipping plane for vehicles, paths, and grid rays.");
            ui.add(
                egui::Slider::new(&mut s.far_clip_m, 10.0..=1_000_000.0)
                    .logarithmic(true)
                    .suffix(" m"),
            );
            ui.end_row();

            ui.label("Max zoom-out")
                .on_hover_text("Maximum orbit-camera distance from its target.");
            ui.add(
                egui::Slider::new(&mut s.max_camera_distance_m, 0.5..=1_000_000.0)
                    .logarithmic(true)
                    .suffix(" m"),
            );
            ui.end_row();

            ui.label("Grid");
            ui.checkbox(&mut s.show_grid, "");
            ui.end_row();

            ui.label("Axes");
            ui.checkbox(&mut s.show_axes, "");
            ui.end_row();

            ui.label("Auto grid cell")
                .on_hover_text("Size the grid cell from the camera height above the ground, like the plot grids. Disable to set a fixed cell size.");
            ui.checkbox(&mut s.grid_cell_auto, "");
            ui.end_row();

            ui.label("Grid cell")
                .on_hover_text("Fixed grid cell size. Ignored while 'Auto grid cell' is on.");
            ui.add_enabled(
                !s.grid_cell_auto,
                egui::Slider::new(&mut s.grid_cell_m, 0.01..=100_000.0)
                    .logarithmic(true)
                    .suffix(" m"),
            );
            ui.end_row();

            ui.label("Fog")
                .on_hover_text("Fade the grid out with distance. Disable to draw the grid crisp all the way to the render distance.");
            ui.checkbox(&mut s.fog_enabled, "");
            ui.end_row();

            ui.label("Fog start");
            ui.add_enabled(
                s.fog_enabled,
                egui::Slider::new(&mut s.fog_start_m, 0.0..=1_000_000.0).suffix(" m"),
            );
            ui.end_row();

            ui.label("Fog end");
            ui.add_enabled(
                s.fog_enabled,
                egui::Slider::new(&mut s.fog_end_m, 1.0..=1_000_000.0)
                    .logarithmic(true)
                    .suffix(" m"),
            );
            ui.end_row();
        });

    if reset_to_defaults_button(ui) {
        settings.scene3d = Scene3dSettings::default();
    }
    SettingsChange {
        map_provider_changed: settings.scene3d.map_provider != before_provider,
        tile_cache_limit_changed: settings.scene3d.tile_cache_limit_bytes != before_limit,
        clear_tile_cache,
        ..Default::default()
    }
}

fn map_provider_label(id: MapProviderId) -> &'static str {
    provider(id).map_or("None", |provider| provider.label())
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.0} MiB", bytes as f64 / MIB)
    }
}

fn scripting_tab(ui: &mut egui::Ui, settings: &mut AppSettings) {
    let s = &mut settings.scripting;
    egui::Grid::new("settings-scripting-grid")
        .num_columns(2)
        .spacing(egui::vec2(16.0, 10.0))
        .show(ui, |ui| {
            ui.label("Open Variables window")
                .on_hover_text("Automatically show the Script Variables window after running a named script that declares a tunable variable.");
            egui::ComboBox::from_id_salt("settings-auto-open-variables")
                .selected_text(s.auto_open_variables.label())
                .show_ui(ui, |ui| {
                    for mode in AutoOpenVariables::ALL {
                        ui.selectable_value(&mut s.auto_open_variables, mode, mode.label());
                    }
                });
            ui.end_row();

            ui.label("Open Scripting Console")
                .on_hover_text("Automatically show the Scripting Console dock when Python output or errors are reported.");
            egui::ComboBox::from_id_salt("settings-auto-open-scripting-console")
                .selected_text(s.auto_open_console.label())
                .show_ui(ui, |ui| {
                    for mode in AutoOpenScriptingConsole::ALL {
                        ui.selectable_value(&mut s.auto_open_console, mode, mode.label());
                    }
                });
            ui.end_row();
        });

    if reset_to_defaults_button(ui) {
        settings.scripting = ScriptingSettings::default();
    }
}

fn reset_to_defaults_button(ui: &mut egui::Ui) -> bool {
    ui.add_space(10.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.button("Reset to defaults").clicked()
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::provider::MapProviderId;

    #[test]
    fn legacy_empty_settings_default_map_and_cache() {
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.scene3d.map_provider, MapProviderId::None);
        assert_eq!(settings.scene3d.tile_cache_limit_bytes, 1024 * 1024 * 1024);
    }

    #[test]
    fn map_and_cache_settings_round_trip_with_stable_provider_name() {
        let mut settings = AppSettings::default();
        settings.scene3d.map_provider = MapProviderId::BingSatellite;
        settings.scene3d.tile_cache_limit_bytes = 4 * 1024 * 1024 * 1024;
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"map_provider\":\"bing_satellite\""));
        assert_eq!(
            serde_json::from_str::<AppSettings>(&json).unwrap(),
            settings
        );
    }

    #[test]
    fn scene_defaults_reset_map_and_cache() {
        let defaults = Scene3dSettings::default();
        assert_eq!(defaults.map_provider, MapProviderId::None);
        assert_eq!(defaults.tile_cache_limit_bytes, 1024 * 1024 * 1024);
    }

    #[test]
    fn unavailable_tile_cache_explains_status_and_disables_clear() {
        let state = TileCacheUiState::default();
        assert_eq!(state.usage_label(), "Cache unavailable");
        assert!(!state.clear_enabled());

        let available = TileCacheUiState {
            available: true,
            usage_bytes: 1024 * 1024,
            clearing: false,
        };
        assert_eq!(available.usage_label(), "1 MiB");
        assert!(available.clear_enabled());
    }

    #[test]
    fn app_wires_cache_controls_without_blocking_the_ui() {
        let source = include_str!("app.rs");
        assert!(source.contains("TileManager::new"));
        assert!(source.contains("manager.set_limit"));
        assert!(source.contains("manager.clear_cache"));
        assert!(source.contains("CacheActionStatus::Pending"));
        assert!(!source.contains("block_on"));
    }

    #[test]
    fn settings_tabs_are_named_for_stable_navigation() {
        let labels: Vec<_> = SettingsTab::ALL
            .into_iter()
            .map(SettingsTab::label)
            .collect();
        assert_eq!(
            labels,
            ["General", "Plots", "Rendering", "3D View", "Scripting"]
        );
    }

    #[test]
    fn settings_tabs_use_egui_dock() {
        let source = include_str!("settings.rs");

        assert!(source.contains("egui_dock::DockArea::new"));
        assert!(source.contains("impl egui_dock::TabViewer for SettingsTabViewer"));
        assert!(!source.contains(concat!("fn ", "tab_list")));
    }

    #[test]
    fn settings_tab_bodies_do_not_repeat_tab_titles_as_headings() {
        let source = include_str!("settings.rs");

        for title in SettingsTab::ALL.map(SettingsTab::label) {
            assert!(!source.contains(&format!("ui.heading(\"{title}\")")));
        }
    }

    #[test]
    fn plot_settings_do_not_expose_a_separate_legend_default() {
        let source = include_str!("settings.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production settings code should precede tests");
        assert!(!production.contains("show_legend_default"));
        assert!(!production.contains("Show legend by default"));
    }

    #[test]
    fn settings_reset_buttons_are_right_aligned() {
        let source = include_str!("settings.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production settings code should precede tests");

        assert!(source.contains("fn reset_to_defaults_button"));
        assert!(source.contains("egui::Layout::right_to_left"));
        assert_eq!(
            production.matches("reset_to_defaults_button(ui)").count(),
            4
        );
    }

    #[test]
    fn plot_display_defaults_legend_top_left_with_minimal_hover() {
        let p = PlotDisplay::default();
        assert_eq!(p.legend_position, LegendPosition::TopLeft);
        assert!(!p.hover_show_field_name);
        assert!(!p.hover_show_time);
        assert_eq!(p.legend_opacity, 1.0);
        assert_eq!(p.hover_opacity, 1.0);
        assert!(p.marker_shade_regions);
    }

    #[test]
    fn app_settings_persist_plot_display() {
        let settings = AppSettings {
            plot: PlotDisplay {
                legend_position: LegendPosition::BottomRight,
                legend_opacity: 0.5,
                hover_show_field_name: false,
                hover_show_time: false,
                hover_opacity: 0.25,
                marker_delta_readout: MarkerDeltaReadout::Hover,
                marker_line_opacity: 0.5,
                marker_line_width: 2.0,
                marker_show_label: false,
                marker_shade_regions: true,
                marker_shade_opacity: 0.2,
                text_label_cap: 1024,
                text_labels_bottom_up: false,
                text_label_spacing: 8.0,
                text_line_width: 2.0,
                text_line_opacity: 0.5,
            },
            ..AppSettings::default()
        };

        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("legend_position"));
        let decoded: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.plot, settings.plot);
    }

    #[test]
    fn font_override_defaults_off_proportional_14() {
        let f = FontOverride::default();
        assert!(!f.enabled);
        assert!(!f.monospace);
        assert_eq!(f.size, 14.0);
    }

    #[test]
    fn app_settings_persist_font_override() {
        let settings = AppSettings {
            font: FontOverride {
                enabled: true,
                size: 20.0,
                monospace: true,
            },
            ..AppSettings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"font\""));
        let decoded: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.font, settings.font);
    }

    #[test]
    fn old_config_without_plot_display_defaults_it() {
        let json = r#"{"theme":"catppuccin_mocha"}"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.plot.legend_position, LegendPosition::TopLeft);
        assert!(!s.plot.hover_show_time);
    }

    #[test]
    fn default_settings_hide_fps_render_continuously_and_enable_vsync() {
        let s = AppSettings::default();
        assert!(!s.show_fps);
        assert_eq!(s.render_mode, RenderMode::Continuous);
        assert!(s.vsync);
        assert!(!s.auto_open_diagnostics);
    }

    #[test]
    fn old_config_without_new_fields_uses_defaults() {
        let json = r#"{"theme":"catppuccin_mocha"}"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert!(!s.show_fps);
        assert_eq!(s.render_mode, RenderMode::Continuous);
        assert!(s.vsync);
        assert!(!s.auto_open_diagnostics);
        assert!(!s.live_connection.recording_enabled);
        assert!(s.live_connection.recording_dir.is_empty());
    }

    #[test]
    fn serialized_app_settings_do_not_include_live_seal_policy() {
        let json = serde_json::to_string(&AppSettings::default()).unwrap();
        assert!(!json.contains("live_seal"));
    }

    #[test]
    fn app_settings_persist_last_live_connection() {
        let settings = AppSettings {
            live_connection: LiveConnectionSettings {
                mode: LiveConnectionMode::TcpClient,
                host: "192.168.1.20".to_owned(),
                port: 5760,
                serial_path: "/dev/ttyUSB0".to_owned(),
                baud: 921_600,
                recording_enabled: true,
                recording_dir: "/tmp/logs".to_owned(),
            },
            ..AppSettings::default()
        };

        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("live_connection"));
        let decoded: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.live_connection, settings.live_connection);
    }

    #[test]
    fn app_settings_persist_scene3d_settings() {
        let settings = AppSettings {
            scene3d: Scene3dSettings {
                map_provider: MapProviderId::BingSatellite,
                tile_cache_limit_bytes: 2 * 1024 * 1024 * 1024,
                far_clip_m: 25_000.0,
                max_camera_distance_m: 12_000.0,
                show_grid: false,
                show_axes: false,
                grid_cell_auto: false,
                grid_cell_m: 5.0,
                fog_enabled: false,
                fog_start_m: 1500.0,
                fog_end_m: 20_000.0,
            },
            ..AppSettings::default()
        };

        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("scene3d"));
        let decoded: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.scene3d, settings.scene3d);
    }

    #[test]
    fn scene3d_defaults_enable_fog_and_auto_grid_cell() {
        let s = Scene3dSettings::default();
        assert!(s.fog_enabled);
        assert!(s.grid_cell_auto);
    }

    #[test]
    fn auto_grid_uses_continuous_cell_and_lod_blend() {
        let s = Scene3dSettings {
            grid_cell_auto: true,
            ..Scene3dSettings::default()
        };
        let (cell, lod) = s.resolved_grid(100.0);
        assert!(lod);
        assert!((cell - 10.0).abs() < 1e-3);
        assert!(s.resolved_grid(50.0).0 < s.resolved_grid(5_000.0).0);
    }

    #[test]
    fn auto_grid_cell_follows_height_not_orbit_radius() {
        let s = Scene3dSettings::default();
        let (cell, lod) = s.resolved_grid(101.5);
        assert!(lod);
        assert!((cell - 10.15).abs() < 1e-2);
    }

    #[test]
    fn fixed_grid_cell_is_a_single_level_independent_of_height() {
        let s = Scene3dSettings {
            grid_cell_auto: false,
            grid_cell_m: 5.0,
            ..Scene3dSettings::default()
        };
        assert_eq!(s.resolved_grid(50.0), (5.0, false));
        assert_eq!(s.resolved_grid(50_000.0), (5.0, false));
    }

    #[test]
    fn old_scene3d_config_without_new_toggles_defaults_them_on() {
        let json = r#"{"theme":"catppuccin_mocha","scene3d":{"far_clip_m":25000.0}}"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert!(s.scene3d.fog_enabled);
        assert!(s.scene3d.grid_cell_auto);
        assert_eq!(s.scene3d.far_clip_m, 25_000.0);
    }

    #[test]
    fn render_mode_labels_are_stable() {
        let labels: Vec<_> = RenderMode::ALL.into_iter().map(RenderMode::label).collect();
        assert_eq!(labels, ["Reactive", "Continuous"]);
    }

    #[test]
    fn old_bridge_columns_false_migrates_to_cut() {
        let r: RenderTuning = serde_json::from_str(
            r#"{"decimate_threshold":8.0,"line_aa_px":1.0,"bridge_columns":false}"#,
        )
        .unwrap();
        assert_eq!(r.gap_mode, GapMode::Cut);
    }

    #[test]
    fn old_bridge_columns_true_and_absent_migrate_to_connect() {
        let with_true: RenderTuning = serde_json::from_str(r#"{"bridge_columns":true}"#).unwrap();
        assert_eq!(with_true.gap_mode, GapMode::Connect);
        let absent: RenderTuning = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.gap_mode, GapMode::Connect);
        assert_eq!(absent.gap_factor, 5.0);
        assert_eq!(absent, RenderTuning::default());
    }

    #[test]
    fn new_gap_fields_round_trip() {
        let r = RenderTuning {
            gap_mode: GapMode::Dotted,
            gap_factor: 12.5,
            ..RenderTuning::default()
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: RenderTuning = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }
}

#[cfg(test)]
mod scripting_settings_tests {
    use super::*;

    #[test]
    fn auto_open_defaults_to_newly_added() {
        assert_eq!(AutoOpenVariables::default(), AutoOpenVariables::NewlyAdded);
        assert_eq!(
            AppSettings::default().scripting.auto_open_variables,
            AutoOpenVariables::NewlyAdded
        );
    }

    #[test]
    fn scripting_defaults_when_absent_from_json() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            s.scripting.auto_open_variables,
            AutoOpenVariables::NewlyAdded
        );
        assert_eq!(
            s.scripting.auto_open_console,
            AutoOpenScriptingConsole::OnErrors
        );
    }

    #[test]
    fn auto_open_round_trips_as_snake_case() {
        let json = serde_json::to_string(&AutoOpenVariables::EveryRun).unwrap();
        assert_eq!(json, "\"every_run\"");
        let back: AutoOpenVariables = serde_json::from_str(&json).unwrap();
        assert_eq!(back, AutoOpenVariables::EveryRun);
    }

    #[test]
    fn scripting_console_auto_open_round_trips_as_snake_case() {
        let json = serde_json::to_string(&AutoOpenScriptingConsole::OnOutput).unwrap();
        assert_eq!(json, "\"on_output\"");
        let back: AutoOpenScriptingConsole = serde_json::from_str(&json).unwrap();
        assert_eq!(back, AutoOpenScriptingConsole::OnOutput);
    }
}
