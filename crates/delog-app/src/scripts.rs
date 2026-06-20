//! Scripts window: REPL + editor + library (SCR-07). Feature-gated.

use std::sync::Arc;

use delog_core::ingest::IngestSender;
use delog_core::metrics::MetricsRegistry;
use delog_core::snapshot::DataStore;
use delog_script::library::ScriptLibrary;
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};
use egui_code_editor::{CodeEditor, ColorTheme, Syntax};

use crate::parsers::{ParserUiAction, ParsersPanel};

/// All UI + engine state for the scripts window (the editor + REPL console). The
/// saved-script library is surfaced through the Tools / Scripts / Run menu, not
/// in this window.
pub struct ScriptsPanel {
    pub open: bool,
    engine: Option<ScriptEngine>,
    library: ScriptLibrary,
    current_name: String,
    editor_text: String,
    repl_input: String,
    console: String,
    status: String,
    /// Script name awaiting a delete confirmation (Tools / Scripts / Run / Remove).
    pending_delete: Option<String>,
    /// Whether a command is in flight (toggles the Run/Cancel button).
    running: bool,
    parsers: ParsersPanel,
}

impl ScriptsPanel {
    pub fn new(scripts_dir: std::path::PathBuf, parsers_dir: std::path::PathBuf) -> Self {
        let library = ScriptLibrary::new(scripts_dir);
        Self {
            open: false,
            engine: None,
            library,
            current_name: String::new(),
            editor_text: String::new(),
            repl_input: String::new(),
            console: String::new(),
            status: String::new(),
            pending_delete: None,
            running: false,
            parsers: ParsersPanel::new(parsers_dir),
        }
    }

    #[allow(dead_code)] // Consumed by the Task 6 Parsers menu.
    pub fn parser_names(&self) -> Vec<String> {
        self.parsers.list().unwrap_or_default()
    }

    #[allow(dead_code)] // Consumed by the Task 6 Parsers menu.
    pub fn add(&mut self) {
        self.parsers.add_new();
    }

    #[allow(dead_code)] // Consumed by the Task 6 Parsers menu.
    pub fn edit(&mut self, name: &str) {
        self.parsers.edit(name);
    }

    #[allow(dead_code)] // Consumed by the Task 6 Parsers menu.
    pub fn request_open(&mut self, ctx: &egui::Context, name: &str) {
        self.parsers.request_open(ctx, name);
    }

    #[allow(dead_code)] // Consumed by the Task 6 combined toolbar state.
    pub fn is_parser_running(&self) -> bool {
        self.parsers.is_running()
    }

    #[allow(dead_code)] // Consumed by the Task 6 toolbar.
    pub fn parser_active_label(&self) -> String {
        self.parsers.active_label()
    }

    pub fn take_parser_diagnostics(&mut self) -> Vec<String> {
        self.parsers.take_diagnostics()
    }

    #[allow(dead_code)] // Consumed by the Task 6 combined stop control.
    pub fn request_interrupt(&self) {
        if let Some(engine) = &self.engine {
            engine.request_interrupt();
        }
    }

    /// Load a saved script into the Console editor and open the window for editing.
    pub fn edit_named(&mut self, name: &str) {
        match self.library.load(name) {
            Ok(source) => {
                self.current_name = name.to_owned();
                self.editor_text = source;
                self.status = format!("editing {name}");
                self.open = true;
            }
            Err(e) => self.status = format!("load failed: {e}"),
        }
    }

    /// Stage a script for deletion; the confirmation dialog is shown by [`ui`].
    pub fn request_delete(&mut self, name: &str) {
        self.pending_delete = Some(name.to_owned());
    }

    /// Saved script names for the Tools / Scripts / Run submenu (read fresh from
    /// disk so newly-saved scripts appear without restarting).
    pub fn script_names(&self) -> Vec<String> {
        self.library.list().unwrap_or_default()
    }

    /// Load a saved script by name and run it through the engine (Tools / Scripts
    /// / Run / <name>). The editor buffer is left untouched; output buffers in the
    /// console and the derived source appears in the data browser.
    pub fn run_named(
        &mut self,
        name: &str,
        store: Arc<DataStore>,
        sender: IngestSender,
        metrics: Arc<MetricsRegistry>,
    ) {
        match self.library.load(name) {
            Ok(source) => {
                self.console.push_str(&format!("# run {name}\n"));
                self.status = format!("running {name}");
                self.running = true;
                self.engine(store, sender, metrics)
                    .send(ScriptCommand::RunScript {
                        name: name.to_owned(),
                        source,
                    });
            }
            Err(e) => self.status = format!("load failed: {e}"),
        }
    }

    /// Lazily start the interpreter on first use.
    fn engine(
        &mut self,
        store: Arc<DataStore>,
        sender: IngestSender,
        metrics: Arc<MetricsRegistry>,
    ) -> &ScriptEngine {
        self.engine
            .get_or_insert_with(|| ScriptEngine::spawn(store, sender, metrics))
    }

    /// The live-batch sender IF the engine has already been spawned (e.g. the
    /// user ran a script). Never spawns the interpreter: a live transform can
    /// only be registered by running a script, which already spawns the engine.
    pub fn live_batch_sender_if_running(
        &self,
    ) -> Option<std::sync::mpsc::SyncSender<delog_core::ingest::ParsedBatch>> {
        self.engine.as_ref().map(|e| e.live_batch_sender())
    }

    fn drain(&mut self) {
        let events = self
            .engine
            .as_ref()
            .map(ScriptEngine::drain_events)
            .unwrap_or_default();
        for event in events {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: ScriptEvent) {
        match event {
            ScriptEvent::Output(s) => self.console.push_str(&s),
            ScriptEvent::Result(r) => {
                self.console.push_str(&r);
                self.console.push('\n');
            }
            ScriptEvent::Error(e) => {
                self.console.push_str(&e);
                self.console.push('\n');
                self.status = "error".into();
                self.running = false;
            }
            ScriptEvent::Done => {
                self.status = "done".into();
                self.running = false;
            }
            // Live-batch processing carries no command-completion semantics.
            ScriptEvent::LiveBatchProcessed => {}
            ScriptEvent::Parser(event) => self.parsers.handle_event(event),
        }
    }

    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        store: Arc<DataStore>,
        sender: IngestSender,
        metrics: Arc<MetricsRegistry>,
    ) {
        self.drain();

        for action in self.parsers.ui(ctx) {
            match action {
                ParserUiAction::ValidateAndSave { name, source, .. } => {
                    self.engine(store.clone(), sender.clone(), Arc::clone(&metrics))
                        .send(ScriptCommand::ValidateParser { name, source });
                }
            }
        }
        for request in self.parsers.take_parse_requests() {
            self.engine(store.clone(), sender.clone(), Arc::clone(&metrics))
                .send(ScriptCommand::ParseFile {
                    parser_name: request.parser_name,
                    source: request.source,
                    path: request.path,
                });
        }

        // Drawn unconditionally so a Remove triggered from the menu can be
        // confirmed even when the Console window is closed.
        self.delete_confirm_ui(ctx);

        if !self.open {
            if self.parsers.is_open() || self.parsers.is_running() {
                ctx.request_repaint();
            }
            return;
        }
        let mut open = self.open;
        egui::Window::new("Scripts")
            .open(&mut open)
            .collapsible(false)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .default_size([720.0, 480.0])
            .show(ctx, |ui| {
                self.window_contents(ui, &store, &sender, &metrics)
            });
        self.open = open;
        ctx.request_repaint(); // keep draining engine events while open
    }

    /// Modal confirmation for deleting a saved script.
    fn delete_confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(name) = self.pending_delete.clone() else {
            return;
        };
        let mut keep_open = true;
        let mut decision: Option<bool> = None; // Some(true) = delete, Some(false) = cancel
        egui::Window::new("Delete script?")
            .collapsible(false)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .resizable(false)
            .open(&mut keep_open)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Delete \u{201c}{name}\u{201d}? This cannot be undone."
                ));
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
        // Closing the window via its [x] is treated as cancel.
        if !keep_open {
            decision = decision.or(Some(false));
        }
        match decision {
            Some(true) => {
                self.status = match self.library.delete(&name) {
                    Ok(()) => format!("deleted {name}"),
                    Err(e) => format!("delete failed: {e}"),
                };
                self.pending_delete = None;
            }
            Some(false) => self.pending_delete = None,
            None => {}
        }
    }

    fn window_contents(
        &mut self,
        ui: &mut egui::Ui,
        store: &Arc<DataStore>,
        sender: &IngestSender,
        metrics: &Arc<MetricsRegistry>,
    ) {
        egui::Panel::bottom("scripts_repl")
            .resizable(true)
            .min_size(140.0)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("REPL:");
                    // Clear-console trash sits at the far right; the input fills
                    // the space between it and the label.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_btn(ui, crate::icons::trash(), "Clear console").clicked() {
                            self.console.clear();
                        }
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.repl_input)
                                .desired_width(f32::INFINITY),
                        );
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let line = std::mem::take(&mut self.repl_input);
                            self.console.push_str(">>> ");
                            self.console.push_str(&line);
                            self.console.push('\n');
                            self.engine(store.clone(), sender.clone(), Arc::clone(metrics))
                                .send(ScriptCommand::Eval(line));
                        }
                    });
                });
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        let mut console_view = self.console.as_str();
                        ui.add(
                            egui::TextEdit::multiline(&mut console_view)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace),
                        );
                    });
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.current_name)
                        .hint_text("script name")
                        .desired_width(160.0),
                );

                // Save (icon-only; disabled until the buffer has a name).
                let save_img = egui::Image::new(crate::icons::save())
                    .fit_to_exact_size(egui::vec2(16.0, 16.0))
                    .tint(ui.visuals().text_color());
                if ui
                    .add_enabled(!self.current_name.is_empty(), egui::Button::image(save_img))
                    .on_hover_text("Save")
                    .clicked()
                {
                    match self.library.save(&self.current_name, &self.editor_text) {
                        Ok(()) => self.status = format!("saved {}", self.current_name),
                        Err(e) => self.status = format!("save failed: {e}"),
                    }
                }

                // The run name: an unsaved buffer runs as "scratch" so its
                // output (and any live transform) is still addressable.
                let run_name = if self.current_name.is_empty() {
                    "scratch".to_owned()
                } else {
                    self.current_name.clone()
                };
                let has_live = self
                    .engine
                    .as_ref()
                    .is_some_and(|e| e.has_live_transform(&run_name));

                // Start: run the editor buffer (enabled only while idle).
                if icon_btn_enabled(ui, !self.running, crate::icons::play(), "Run").clicked() {
                    self.console.push_str(&format!("# run {run_name}\n"));
                    self.status = format!("running {run_name}");
                    self.running = true;
                    let source = self.editor_text.clone();
                    self.engine(store.clone(), sender.clone(), Arc::clone(metrics))
                        .send(ScriptCommand::RunScript {
                            name: run_name.clone(),
                            source,
                        });
                }

                // Stop: cooperatively interrupt the running script.
                if icon_btn_enabled(ui, self.running, crate::icons::square(), "Stop").clicked()
                    && let Some(e) = &self.engine
                {
                    e.request_interrupt();
                }

                // Unregister: drop this script's live transform and remove its
                // appendable source (enabled only when one is registered).
                if icon_btn_enabled(
                    ui,
                    has_live,
                    crate::icons::unplug(),
                    "Unregister live transform",
                )
                .clicked()
                {
                    if let Some(e) = &self.engine {
                        e.send(ScriptCommand::UnregisterLive {
                            name: run_name.clone(),
                        });
                    }
                    self.console
                        .push_str(&format!("# unregister live transform {run_name}\n"));
                }

                ui.label(&self.status);
            });
            // egui_code_editor 0.3.3 has no `with_syntax` builder; the syntax is
            // passed to `show` as a `&Syntax` argument instead.
            CodeEditor::default()
                .id_source("script_editor")
                .with_rows(25)
                .with_theme(ColorTheme::GITHUB_DARK)
                .with_numlines(true)
                .show(ui, &mut self.editor_text, &Syntax::python());
        });
    }
}

/// A compact icon-only button (16px, tinted to the text colour) with a tooltip.
fn icon_btn(ui: &mut egui::Ui, icon: egui::ImageSource<'static>, hover: &str) -> egui::Response {
    icon_btn_enabled(ui, true, icon, hover)
}

/// Like [`icon_btn`] but greys out (and ignores clicks) when `enabled` is false.
fn icon_btn_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    icon: egui::ImageSource<'static>,
    hover: &str,
) -> egui::Response {
    let image = egui::Image::new(icon)
        .fit_to_exact_size(egui::vec2(16.0, 16.0))
        .tint(ui.visuals().text_color());
    ui.add_enabled(enabled, egui::Button::image(image))
        .on_hover_text(hover)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use delog_script::ParserEvent;

    use super::*;

    #[test]
    fn parser_events_do_not_change_console_running_state() {
        let root = std::env::temp_dir().join(format!(
            "delog-scripts-parser-routing-{}",
            std::process::id()
        ));
        let mut panel = ScriptsPanel::new(root.join("scripts"), root.join("parsers"));
        panel.running = true;
        panel.status = "running console script".into();

        panel.handle_event(ScriptEvent::Parser(ParserEvent::Running {
            parser_name: "raw.py".into(),
            path: PathBuf::from("flight.raw"),
        }));

        assert!(panel.running);
        assert_eq!(panel.status, "running console script");
        assert!(panel.is_parser_running());
        assert!(panel.parser_active_label().contains("raw.py"));
    }
}
