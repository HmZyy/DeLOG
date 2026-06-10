//! Top-level eframe application state.

pub struct DelogApp {}

impl DelogApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        Self {}
    }
}

impl eframe::App for DelogApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Frame::central_panel(ui.style()).show(ui, |_ui| {
            // Workspace-first window (PLAN.md §19.1); tiles arrive with PLT-01.
        });
    }
}
