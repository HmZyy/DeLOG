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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    #[default]
    General,
    Rendering,
}

impl SettingsTab {
    const ALL: [Self; 2] = [Self::General, Self::Rendering];

    const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Rendering => "Rendering",
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

            ui.label("Render mode")
                .on_hover_text(
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
        assert_eq!(labels, ["General", "Rendering"]);
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
    fn render_mode_labels_are_stable() {
        let labels: Vec<_> = RenderMode::ALL.into_iter().map(RenderMode::label).collect();
        assert_eq!(labels, ["Reactive", "Continuous"]);
    }
}
