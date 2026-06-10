//! Top-level eframe application state.

use crate::about;

#[derive(Default)]
pub struct DelogApp {
    show_about: bool,
}

impl DelogApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        Self::default()
    }
}

impl eframe::App for DelogApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("main_menu").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });
            });
        });

        egui::Frame::central_panel(ui.style()).show(ui, |_ui| {
            // Workspace-first window (PLAN.md §19.1); tiles arrive with PLT-01.
        });

        about::window(ui.ctx(), &mut self.show_about);
    }
}
