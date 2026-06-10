//! Top-level eframe application state.

use crate::about;
use crate::browser::{self, BrowserModel};
use crate::session::Session;

pub struct DelogApp {
    session: Session,
    path_input: String,
    show_about: bool,
}

impl DelogApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        Self {
            session: Session::new(cc.egui_ctx.clone()),
            path_input: String::new(),
            show_about: false,
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
}

impl eframe::App for DelogApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_dropped_files(ui.ctx());
        self.session.prune_finished();

        egui::Panel::top("main_menu").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });

                ui.separator();
                // Minimal open affordance until the toolbar + native dialog
                // (UIX-02) land: type or paste a path, or drop a file anywhere.
                ui.label("Open:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.path_input)
                        .hint_text("path/to/log.BIN")
                        .desired_width(280.0),
                );
                let submit = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Open").clicked() || submit) && !self.path_input.trim().is_empty() {
                    self.session.open_path(self.path_input.trim().to_owned());
                    self.path_input.clear();
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

        let snapshot = self.session.snapshot();
        let model = BrowserModel::from_snapshot(&snapshot);
        egui::Panel::left("data_browser")
            .resizable(true)
            .default_size(280.0)
            .show_inside(ui, |ui| {
                browser::ui(ui, &model);
            });

        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            // Workspace-first window (PLAN.md §19.1); tiles arrive with PLT-01.
            if model.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak("Drop a flight log to begin.");
                });
            }
        });

        about::window(ui.ctx(), &mut self.show_about);
    }
}
