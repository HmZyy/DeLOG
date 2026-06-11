//! Top-level eframe application state.

use delog_cache::CacheManager;

use crate::about;
use crate::browser::{self, BrowserModel};
use crate::gpu::{self, GpuBridge};
use crate::plot::PlotPane;
use crate::session::Session;

pub struct DelogApp {
    session: Session,
    gpu: GpuBridge,
    caches: CacheManager,
    pane: PlotPane,
    frame: u64,
    last_epoch: u64,
    origin_us: i64,
    path_input: String,
    show_about: bool,
}

impl DelogApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        Self {
            session: Session::new(cc.egui_ctx.clone()),
            gpu: GpuBridge::from_creation_context(cc),
            caches: CacheManager::new(),
            pane: PlotPane::default(),
            frame: 0,
            last_epoch: u64::MAX,
            origin_us: 0,
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
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.handle_dropped_files(ui.ctx());
        self.session.prune_finished();
        self.frame = self.frame.wrapping_add(1);

        let snapshot = self.session.snapshot();

        // Cache lifecycle: shared origin, frame recency, drain builds, and an
        // epoch-driven incremental append + GC (§8.5).
        if let Some(range) = snapshot.global_time_range() {
            self.origin_us = range.min_us;
            self.caches.set_origin(self.origin_us);
            self.pane.init_view(range);
        }
        self.caches.begin_frame(self.frame);
        self.caches.poll_builds();
        if snapshot.epoch != self.last_epoch {
            self.caches.on_epoch(&snapshot);
            self.last_epoch = snapshot.epoch;
        }
        // Keep every plotted trace's cache requested/warm.
        for field in self.pane.fields().collect::<Vec<_>>() {
            self.caches.request(field, &snapshot);
        }
        self.caches.evict_over_budget();

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

        let model = BrowserModel::from_snapshot(&snapshot);
        egui::Panel::left("data_browser")
            .resizable(true)
            .default_size(280.0)
            .show_inside(ui, |ui| {
                browser::ui(ui, &model);
            });

        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            if model.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak("Drop a flight log to begin.");
                });
                return;
            }

            // The central panel is a drop zone: dragging a field from the
            // browser onto it plots that field (PLT-13).
            let frame_style = egui::Frame::default();
            let (_, dropped) =
                ui.dnd_drop_zone::<delog_core::identity::FieldId, ()>(frame_style, |ui| {
                    let rect = ui.available_rect_before_wrap();
                    let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                    self.handle_plot_interaction(&response, rect);

                    if self.pane.is_empty() {
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "Drag a field here to plot it",
                            egui::FontId::proportional(14.0),
                            ui.visuals().weak_text_color(),
                        );
                    } else if self.gpu.is_available() {
                        self.gpu.render_pane(
                            ui,
                            frame,
                            rect,
                            &mut self.caches,
                            &self.pane,
                            self.origin_us,
                        );
                    }
                });
            if let Some(field) = dropped
                && self.pane.add_trace(*field)
            {
                self.caches.request(*field, &snapshot);
            }
        });

        about::window(ui.ctx(), &mut self.show_about);
    }
}

impl DelogApp {
    /// Pan (drag), zoom (wheel @ cursor) and reset (double-click) the X view
    /// from pointer input over the plot rect (PLT-04).
    fn handle_plot_interaction(&mut self, response: &egui::Response, rect: egui::Rect) {
        let Some(mut view) = self.pane.view() else {
            return;
        };

        if response.double_clicked() {
            if let Some(range) = self.session.snapshot().global_time_range() {
                self.pane.reset_view(range);
            }
            return;
        }

        if response.dragged() {
            gpu::apply_pan(&mut view, response.drag_delta().x, rect.width());
        }

        if response.hovered() {
            let scroll = response.ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let cursor_frac = response
                    .hover_pos()
                    .map(|p| (p.x - rect.left()) / rect.width().max(1.0))
                    .unwrap_or(0.5);
                gpu::apply_zoom(&mut view, cursor_frac, scroll);
            }
        }

        self.pane.set_view(view);
    }
}
