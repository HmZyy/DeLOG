//! Scripts window: REPL + editor + library (SCR-07). Feature-gated.

use std::sync::Arc;

use delog_core::ingest::IngestSender;
use delog_core::snapshot::DataStore;
use delog_script::library::ScriptLibrary;
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};
use egui_code_editor::{CodeEditor, ColorTheme, Syntax};

/// All UI + engine state for the scripts window (the editor + REPL console). The
/// saved-script library is surfaced through the Tools ▸ Scripts ▸ Run menu, not
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
}

impl ScriptsPanel {
    pub fn new(scripts_dir: std::path::PathBuf) -> Self {
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
        }
    }

    /// Saved script names for the Tools ▸ Scripts ▸ Run submenu (read fresh from
    /// disk so newly-saved scripts appear without restarting).
    pub fn script_names(&self) -> Vec<String> {
        self.library.list().unwrap_or_default()
    }

    /// Load a saved script by name and run it through the engine (Tools ▸ Scripts
    /// ▸ Run ▸ <name>). The editor buffer is left untouched; output buffers in the
    /// console and the derived source appears in the data browser.
    pub fn run_named(&mut self, name: &str, store: Arc<DataStore>, sender: IngestSender) {
        match self.library.load(name) {
            Ok(source) => {
                self.console.push_str(&format!("# run {name}\n"));
                self.status = format!("running {name}");
                self.engine(store, sender).send(ScriptCommand::RunScript {
                    name: name.to_owned(),
                    source,
                });
            }
            Err(e) => self.status = format!("load failed: {e}"),
        }
    }

    /// Lazily start the interpreter on first use.
    fn engine(&mut self, store: Arc<DataStore>, sender: IngestSender) -> &ScriptEngine {
        self.engine
            .get_or_insert_with(|| ScriptEngine::spawn(store, sender))
    }

    fn drain(&mut self) {
        if let Some(engine) = &self.engine {
            for ev in engine.drain_events() {
                match ev {
                    ScriptEvent::Output(s) => self.console.push_str(&s),
                    ScriptEvent::Result(r) => {
                        self.console.push_str(&r);
                        self.console.push('\n');
                    }
                    ScriptEvent::Error(e) => {
                        self.console.push_str(&e);
                        self.console.push('\n');
                        self.status = "error".into();
                    }
                    ScriptEvent::Done => self.status = "done".into(),
                }
            }
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, store: Arc<DataStore>, sender: IngestSender) {
        if !self.open {
            return;
        }
        self.drain();
        let mut open = self.open;
        egui::Window::new("Scripts")
            .open(&mut open)
            .default_size([720.0, 480.0])
            .show(ctx, |ui| self.window_contents(ui, &store, &sender));
        self.open = open;
        ctx.request_repaint(); // keep draining engine events while open
    }

    fn window_contents(
        &mut self,
        ui: &mut egui::Ui,
        store: &Arc<DataStore>,
        sender: &IngestSender,
    ) {
        egui::Panel::bottom("scripts_repl")
            .resizable(true)
            .min_size(140.0)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("REPL:");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.repl_input)
                            .desired_width(f32::INFINITY),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let line = std::mem::take(&mut self.repl_input);
                        self.console.push_str(">>> ");
                        self.console.push_str(&line);
                        self.console.push('\n');
                        self.engine(store.clone(), sender.clone())
                            .send(ScriptCommand::Eval(line));
                    }
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
                ui.text_edit_singleline(&mut self.current_name);
                if ui.button("Save").clicked() && !self.current_name.is_empty() {
                    match self.library.save(&self.current_name, &self.editor_text) {
                        Ok(()) => self.status = format!("saved {}", self.current_name),
                        Err(e) => self.status = format!("save failed: {e}"),
                    }
                }
                if ui.button("Run \u{25b6}").clicked() && !self.current_name.is_empty() {
                    self.console
                        .push_str(&format!("# run {}\n", self.current_name));
                    let name = self.current_name.clone();
                    let source = self.editor_text.clone();
                    self.engine(store.clone(), sender.clone())
                        .send(ScriptCommand::RunScript { name, source });
                }
                if ui.button("Cancel \u{23f9}").clicked()
                    && let Some(e) = &self.engine
                {
                    e.request_interrupt();
                }
                ui.label(&self.status);
            });
            // egui_code_editor 0.3.3 has no `with_syntax` builder; the syntax is
            // passed to `show` as a `&Syntax` argument instead.
            CodeEditor::default()
                .id_source("script_editor")
                .with_theme(ColorTheme::GITHUB_DARK)
                .with_numlines(true)
                .show(ui, &mut self.editor_text, &Syntax::python());
        });
    }
}
