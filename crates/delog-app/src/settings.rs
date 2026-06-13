//! Settings dialog state and tab rendering.

use crate::theme::ThemeChoice;

fn default_decimate_threshold() -> f32 {
    8.0
}
fn default_line_aa_px() -> f32 {
    1.0
}
fn default_true() -> bool {
    true
}
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub theme: ThemeChoice,
    /// Plot rendering tuning (live-adjustable, persisted in the config).
    #[serde(default)]
    pub render: RenderTuning,
    /// Show the corner FPS badge (PRF-08). Default off.
    #[serde(default)]
    pub show_fps: bool,
    /// Frame-pacing policy (PRF-09). Default `Reactive`.
    #[serde(default)]
    pub render_mode: RenderMode,
    /// Last valid live connection entered in the MAVLink connection dialog.
    #[serde(default)]
    pub live_connection: LiveConnectionSettings,
    /// 3D scene render and camera tuning.
    #[serde(default)]
    pub scene3d: Scene3dSettings,
}

/// Knobs for the plot draw path (§9.5 decimation + line/edge AA). Lives in the
/// config so the values can be tuned live and persist across sessions.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RenderTuning {
    /// Switch to the decimated min/max path above this many samples per pixel.
    #[serde(default = "default_decimate_threshold")]
    pub decimate_threshold: f32,
    /// Edge anti-alias feather, in pixels (0 = hard edges).
    #[serde(default = "default_line_aa_px")]
    pub line_aa_px: f32,
    /// Bridge adjacent decimated columns so smooth slopes read as a connected
    /// line instead of disjoint bars (§9.5).
    #[serde(default = "default_true")]
    pub bridge_columns: bool,
}

impl Default for RenderTuning {
    fn default() -> Self {
        Self {
            decimate_threshold: default_decimate_threshold(),
            line_aa_px: default_line_aa_px(),
            bridge_columns: true,
        }
    }
}

/// Frame-pacing policy (PRF-09). `Reactive` is event-driven and idles at 0% GPU
/// (§11 / TLN-06); `Continuous` repaints every frame regardless of activity.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RenderMode {
    #[default]
    Reactive,
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

/// Transport persisted for the live MAVLink connection dialog.
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

/// Last-used values for the live connection dialog. Network modes use
/// `host`/`port`; serial uses `serial_path`/`baud`.
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
}

impl Default for LiveConnectionSettings {
    fn default() -> Self {
        Self {
            mode: LiveConnectionMode::UdpServer,
            host: default_live_host(),
            port: default_live_port(),
            serial_path: default_live_serial_path(),
            baud: default_live_baud(),
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

/// Persisted 3D scene tuning. Distances are render-space metres. The camera
/// always tracks the selected vehicle (falling back to the world origin when no
/// pose is available).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Scene3dSettings {
    #[serde(default = "default_scene_far_clip_m")]
    pub far_clip_m: f32,
    #[serde(default = "default_scene_max_camera_distance_m")]
    pub max_camera_distance_m: f32,
    #[serde(default = "default_true")]
    pub show_grid: bool,
    #[serde(default = "default_true")]
    pub show_axes: bool,
    /// Auto-size the grid cell from the camera distance (like the plot grids).
    /// When set, `grid_cell_m` is ignored.
    #[serde(default = "default_true")]
    pub grid_cell_auto: bool,
    #[serde(default = "default_scene_grid_cell_m")]
    pub grid_cell_m: f32,
    /// Whether the distance fog/fade is applied to the grid at all.
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

    /// The grid cell to render and whether the shader should cross-fade LOD
    /// levels around it.
    ///
    /// In auto mode the cell is a *continuous* function of the camera's height
    /// above the ground plane (where the grid lives) — so it does not collapse
    /// to a shimmering fine grid when you orbit tightly around an airborne
    /// vehicle, and it never *snaps* between sizes as the height changes. The
    /// `true` flag tells the grid shader to draw two bracketing power-of-ten
    /// grids and fade the finer one in/out, keeping lines anchored to world
    /// coordinates with no popping. In fixed mode the exact `grid_cell_m` is
    /// drawn as a single level (`false`).
    pub fn resolved_grid(self, eye_height_m: f32) -> (f32, bool) {
        if self.grid_cell_auto {
            let height = finite_or(eye_height_m, 100.0).abs().max(1e-3);
            // ~10 cells across the view; the shader handles LOD continuity.
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    #[default]
    General,
    Rendering,
    Scene3d,
}

impl SettingsTab {
    const ALL: [Self; 3] = [Self::General, Self::Rendering, Self::Scene3d];

    const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Rendering => "Rendering",
            Self::Scene3d => "3D View",
        }
    }
}

#[derive(Debug, Default)]
pub struct SettingsDialog {
    open: bool,
    selected_tab: SettingsTab,
}

impl SettingsDialog {
    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn show(&mut self, ctx: &egui::Context, settings: &mut AppSettings) -> SettingsChange {
        if !self.open {
            return SettingsChange::default();
        }

        let mut open = self.open;
        let mut change = SettingsChange::default();
        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(520.0)
            .default_height(340.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    self.tab_list(ui);
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(340.0);
                        match self.selected_tab {
                            SettingsTab::General => {
                                change |= general_tab(ui, settings);
                            }
                            SettingsTab::Rendering => {
                                rendering_tab(ui, settings);
                            }
                            SettingsTab::Scene3d => {
                                scene3d_tab(ui, settings);
                            }
                        }
                    });
                });
            });
        self.open &= open;
        change
    }

    fn tab_list(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.set_min_width(132.0);
            for tab in SettingsTab::ALL {
                ui.selectable_value(&mut self.selected_tab, tab, tab.label());
            }
        });
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SettingsChange {
    pub theme_changed: bool,
}

impl std::ops::BitOrAssign for SettingsChange {
    fn bitor_assign(&mut self, rhs: Self) {
        self.theme_changed |= rhs.theme_changed;
    }
}

fn general_tab(ui: &mut egui::Ui, settings: &mut AppSettings) -> SettingsChange {
    let before = settings.theme;
    ui.heading("General");
    ui.add_space(8.0);
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
        });

    SettingsChange {
        theme_changed: settings.theme != before,
    }
}

fn rendering_tab(ui: &mut egui::Ui, settings: &mut AppSettings) {
    let r = &mut settings.render;
    ui.heading("Rendering");
    ui.add_space(8.0);
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

            ui.label("Bridge decimated columns")
                .on_hover_text("Connect adjacent min/max columns so smooth slopes read as a continuous line instead of disjoint bars.");
            ui.checkbox(&mut r.bridge_columns, "");
            ui.end_row();
        });

    ui.add_space(10.0);
    if ui.button("Reset to defaults").clicked() {
        settings.render = RenderTuning::default();
    }
}

fn scene3d_tab(ui: &mut egui::Ui, settings: &mut AppSettings) {
    let s = &mut settings.scene3d;
    ui.heading("3D View");
    ui.add_space(8.0);
    egui::Grid::new("settings-scene3d-grid")
        .num_columns(2)
        .spacing(egui::vec2(16.0, 10.0))
        .show(ui, |ui| {
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

    ui.add_space(10.0);
    if ui.button("Reset to defaults").clicked() {
        settings.scene3d = Scene3dSettings::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_use_catppuccin_mocha() {
        assert_eq!(AppSettings::default().theme, ThemeChoice::CatppuccinMocha);
    }

    #[test]
    fn settings_tabs_are_named_for_stable_navigation() {
        let labels: Vec<_> = SettingsTab::ALL
            .into_iter()
            .map(SettingsTab::label)
            .collect();
        assert_eq!(labels, ["General", "Rendering", "3D View"]);
    }

    #[test]
    fn default_settings_hide_fps_and_render_reactively() {
        let s = AppSettings::default();
        assert!(!s.show_fps);
        assert_eq!(s.render_mode, RenderMode::Reactive);
    }

    #[test]
    fn old_config_without_new_fields_uses_defaults() {
        // A config written before these fields existed.
        // ThemeChoice serialises with snake_case, so CatppuccinMocha → "catppuccin_mocha".
        let json = r#"{"theme":"catppuccin_mocha"}"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert!(!s.show_fps);
        assert_eq!(s.render_mode, RenderMode::Reactive);
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
        // ~10 cells across the view, continuous (no 1-2-5 snap), LOD on.
        let (cell, lod) = s.resolved_grid(100.0);
        assert!(lod);
        assert!((cell - 10.0).abs() < 1e-3);
        // Height drives it continuously and monotonically — no discrete jumps.
        assert!(s.resolved_grid(50.0).0 < s.resolved_grid(5_000.0).0);
    }

    #[test]
    fn auto_grid_cell_follows_height_not_orbit_radius() {
        // Regression: orbiting tightly around an airborne vehicle means a small
        // orbit radius but a large height above the y=0 grid. The cell follows
        // the height (≈100 m up → ≈10 m cells), and being continuous it does not
        // pop between sizes as the orbit pitch nudges the height.
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
        // A scene3d config written before fog_enabled / grid_cell_auto existed.
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
}
