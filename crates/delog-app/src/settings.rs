//! Settings dialog state and tab rendering.

use crate::theme::ThemeChoice;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AppSettings {
    pub theme: ThemeChoice,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    #[default]
    General,
}

impl SettingsTab {
    const ALL: [Self; 1] = [Self::General];

    const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
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
        });

    SettingsChange {
        theme_changed: settings.theme != before,
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
        assert_eq!(labels, ["General"]);
    }
}
