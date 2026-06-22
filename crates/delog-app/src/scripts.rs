//! Scripts window: REPL + editor + library. Feature-gated.

use std::collections::VecDeque;
use std::sync::Arc;

use delog_core::ingest::IngestSender;
use delog_core::metrics::MetricsRegistry;
use delog_core::snapshot::DataStore;
use delog_script::library::ScriptLibrary;
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};
use egui_code_editor::{CodeEditor, ColorTheme, Syntax};

use crate::parsers::{ParserUiAction, ParsersPanel};

enum PreparedParserCommand {
    Validation {
        name: String,
        source: String,
    },
    Parse {
        parser_name: String,
        source: String,
        path: std::path::PathBuf,
    },
}

impl PreparedParserCommand {
    fn command(&self) -> ScriptCommand {
        match self {
            Self::Validation { name, source } => ScriptCommand::ValidateParser {
                name: name.clone(),
                source: source.clone(),
            },
            Self::Parse {
                parser_name,
                source,
                path,
            } => ScriptCommand::ParseFile {
                parser_name: parser_name.clone(),
                source: source.clone(),
                path: path.clone(),
            },
        }
    }
}

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
    deferred_parser_actions: VecDeque<ParserUiAction>,
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
            deferred_parser_actions: VecDeque::new(),
        }
    }

    #[allow(dead_code)]
    pub fn parser_names(&mut self) -> std::io::Result<Vec<String>> {
        self.parsers.list()
    }

    #[allow(dead_code)]
    pub fn add(&mut self) {
        self.parsers.add_new();
    }

    #[allow(dead_code)]
    pub fn edit(&mut self, name: &str) {
        self.parsers.edit(name);
    }

    #[allow(dead_code)]
    pub fn delete_parser(&mut self, name: &str) {
        self.parsers.request_delete_named(name);
    }

    #[allow(dead_code)]
    pub fn request_open(&mut self, ctx: &egui::Context, name: &str) -> bool {
        if !self.parser_dispatch_enabled() {
            self.status = "finish the running console command before opening a parser file".into();
            return false;
        }
        self.parsers.request_open(ctx, name);
        true
    }

    #[allow(dead_code)]
    pub fn is_parser_running(&self) -> bool {
        self.parsers.is_running()
    }

    #[allow(dead_code)]
    pub fn parser_active_label(&self) -> String {
        self.parsers.active_label()
    }

    pub fn take_parser_diagnostics(&mut self) -> Vec<String> {
        self.parsers.take_diagnostics()
    }

    pub fn request_interrupt(&self) {
        if self.can_interrupt_console()
            && let Some(engine) = &self.engine
        {
            engine.request_interrupt();
        }
    }

    pub fn ordinary_dispatch_enabled(&self) -> bool {
        !self.running && !self.should_poll_parser_events()
    }

    pub fn parser_dispatch_enabled(&self) -> bool {
        !self.running
    }

    fn can_interrupt_console(&self) -> bool {
        self.running
    }

    fn reject_ordinary_dispatch(&mut self) -> bool {
        self.status = if self.should_poll_parser_events() {
            "parser work is pending; console command not started".into()
        } else {
            "another console command is already running".into()
        };
        false
    }

    pub fn cancel_parsers(&mut self) {
        let result = self
            .engine
            .as_ref()
            .map(ScriptEngine::cancel_parsers)
            .unwrap_or(Ok(()));
        self.finish_cancel_dispatch(result);
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
    ) -> bool {
        if !self.ordinary_dispatch_enabled() {
            return self.reject_ordinary_dispatch();
        }
        match self.library.load(name) {
            Ok(source) => self.dispatch_run(name.to_owned(), source, store, sender, metrics),
            Err(e) => {
                self.status = format!("load failed: {e}");
                false
            }
        }
    }

    fn dispatch_run(
        &mut self,
        name: String,
        source: String,
        store: Arc<DataStore>,
        sender: IngestSender,
        metrics: Arc<MetricsRegistry>,
    ) -> bool {
        if !self.ordinary_dispatch_enabled() {
            return self.reject_ordinary_dispatch();
        }
        match self
            .engine(store, sender, metrics)
            .send(ScriptCommand::RunScript {
                name: name.clone(),
                source,
            }) {
            Ok(()) => {
                self.console.push_str(&format!("# run {name}\n"));
                self.status = format!("running {name}");
                self.running = true;
                true
            }
            Err(error) => {
                self.status = format!("run dispatch failed: {error}");
                false
            }
        }
    }

    fn dispatch_eval(
        &mut self,
        line: String,
        store: Arc<DataStore>,
        sender: IngestSender,
        metrics: Arc<MetricsRegistry>,
    ) -> bool {
        if !self.ordinary_dispatch_enabled() {
            return self.reject_ordinary_dispatch();
        }
        match self
            .engine(store, sender, metrics)
            .send(ScriptCommand::Eval(line.clone()))
        {
            Ok(()) => {
                self.console.push_str(">>> ");
                self.console.push_str(&line);
                self.console.push('\n');
                self.status = "running REPL input".into();
                self.running = true;
                true
            }
            Err(error) => {
                self.status = format!("REPL dispatch failed: {error}");
                false
            }
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

    fn finish_validation_dispatch(&mut self, name: &str, result: Result<(), String>) {
        match result {
            Ok(()) => self.parsers.mark_validation_dispatched(name),
            Err(error) => self.parsers.validation_dispatch_failed(name, &error),
        }
    }

    fn finish_parse_dispatch(
        &mut self,
        parser_name: &str,
        path: &std::path::Path,
        result: Result<(), String>,
    ) {
        match result {
            Ok(()) => self.parsers.mark_parse_dispatched(parser_name, path),
            Err(error) => self
                .parsers
                .parse_dispatch_failed(parser_name, path, &error),
        }
    }

    fn finish_cancel_dispatch(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.parsers.cancel_dispatch_failed(&error);
        }
    }

    fn queue_parser_action(&mut self, action: ParserUiAction) {
        self.deferred_parser_actions.push_back(action);
    }

    fn take_ready_parser_commands(&mut self) -> Vec<PreparedParserCommand> {
        if !self.parser_dispatch_enabled() {
            return Vec::new();
        }
        let mut commands = Vec::new();
        for action in self.deferred_parser_actions.drain(..) {
            match action {
                ParserUiAction::ValidateAndSave { name, source, .. } => {
                    commands.push(PreparedParserCommand::Validation { name, source });
                }
            }
        }
        for request in self.parsers.take_parse_requests() {
            commands.push(PreparedParserCommand::Parse {
                parser_name: request.parser_name,
                source: request.source,
                path: request.path,
            });
        }
        commands
    }

    fn finish_parser_dispatch(
        &mut self,
        command: PreparedParserCommand,
        result: Result<(), String>,
    ) {
        match command {
            PreparedParserCommand::Validation { name, .. } => {
                self.finish_validation_dispatch(&name, result);
            }
            PreparedParserCommand::Parse {
                parser_name, path, ..
            } => self.finish_parse_dispatch(&parser_name, &path, result),
        }
    }

    fn should_poll_parser_events(&self) -> bool {
        self.parsers.has_pending_work()
    }

    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        store: Arc<DataStore>,
        sender: IngestSender,
        metrics: Arc<MetricsRegistry>,
    ) {
        self.drain();

        for action in self.parsers.ui(ctx, self.parser_dispatch_enabled()) {
            self.queue_parser_action(action);
        }
        for command in self.take_ready_parser_commands() {
            let result = self
                .engine(store.clone(), sender.clone(), Arc::clone(&metrics))
                .send(command.command());
            self.finish_parser_dispatch(command, result);
        }

        // Drawn unconditionally so a Remove triggered from the menu can be
        // confirmed even when the Console window is closed.
        self.delete_confirm_ui(ctx);

        if !self.open {
            if self.should_poll_parser_events() {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
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
        let mut decision: Option<bool> = None;
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
                        let resp = ui.add_enabled(
                            self.ordinary_dispatch_enabled(),
                            egui::TextEdit::singleline(&mut self.repl_input)
                                .desired_width(f32::INFINITY),
                        );
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let line = std::mem::take(&mut self.repl_input);
                            self.dispatch_eval(
                                line,
                                store.clone(),
                                sender.clone(),
                                Arc::clone(metrics),
                            );
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

                if icon_btn_enabled(
                    ui,
                    self.ordinary_dispatch_enabled(),
                    crate::icons::play(),
                    "Run",
                )
                .clicked()
                {
                    let source = self.editor_text.clone();
                    self.dispatch_run(
                        run_name.clone(),
                        source,
                        store.clone(),
                        sender.clone(),
                        Arc::clone(metrics),
                    );
                }

                if icon_btn_enabled(
                    ui,
                    self.can_interrupt_console(),
                    crate::icons::square(),
                    "Stop",
                )
                .clicked()
                {
                    self.request_interrupt();
                }

                if icon_btn_enabled(
                    ui,
                    has_live,
                    crate::icons::unplug(),
                    "Unregister live transform",
                )
                .clicked()
                {
                    if let Some(e) = &self.engine {
                        let _ = e.send(ScriptCommand::UnregisterLive {
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

    use delog_core::ingest::ingest_channel;
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
        panel
            .parsers
            .mark_parse_dispatched("raw.py", std::path::Path::new("flight.raw"));

        panel.handle_event(ScriptEvent::Parser(ParserEvent::Running {
            parser_name: "raw.py".into(),
            path: PathBuf::from("flight.raw"),
        }));

        assert!(panel.running);
        assert_eq!(panel.status, "running console script");
        assert!(panel.is_parser_running());
        assert!(panel.parser_active_label().contains("raw.py"));
        assert!(!panel.ordinary_dispatch_enabled());
        assert!(panel.can_interrupt_console());
    }

    #[test]
    fn parser_pending_blocks_saved_runs_and_repl_until_terminal_event() {
        let root = std::env::temp_dir().join(format!(
            "delog-scripts-shared-worker-{}",
            std::process::id()
        ));
        let mut panel = ScriptsPanel::new(root.join("scripts"), root.join("parsers"));
        panel.library.save("saved", "print('saved')").unwrap();
        panel
            .parsers
            .mark_parse_dispatched("raw.py", std::path::Path::new("flight.raw"));
        let store = Arc::new(DataStore::new());
        let (sender, _receiver) = ingest_channel();
        let metrics = Arc::new(MetricsRegistry::new());

        assert!(!panel.run_named(
            "saved",
            Arc::clone(&store),
            sender.clone(),
            Arc::clone(&metrics),
        ));
        assert!(!panel.dispatch_eval("1 + 1".into(), Arc::clone(&store), sender, metrics,));
        assert!(panel.engine.is_none());
        assert!(!panel.running);
        assert!(!panel.can_interrupt_console());

        panel.handle_event(ScriptEvent::Parser(ParserEvent::Succeeded {
            parser_name: "raw.py".into(),
            path: PathBuf::from("flight.raw"),
            topics: 1,
            rows: 1,
        }));

        assert!(panel.ordinary_dispatch_enabled());
    }

    #[test]
    fn ordinary_run_defers_picker_request_until_done_is_processed() {
        let root = std::env::temp_dir().join(format!(
            "delog-scripts-reverse-parser-{}",
            std::process::id()
        ));
        let mut panel = ScriptsPanel::new(root.join("scripts"), root.join("parsers"));
        let path = PathBuf::from("flight.raw");
        panel.running = true;
        assert!(!panel.request_open(&egui::Context::default(), "raw.py"));
        assert!(!panel.parser_dispatch_enabled());
        panel
            .parsers
            .enqueue_parse_request(crate::parsers::ParseRequest {
                parser_name: "raw.py".into(),
                source: "def Parse(data): return []".into(),
                path: path.clone(),
            });

        assert!(panel.take_ready_parser_commands().is_empty());
        assert!(!panel.parsers.has_pending_work());
        assert!(panel.can_interrupt_console());

        panel.handle_event(ScriptEvent::Done);
        let commands = panel.take_ready_parser_commands();
        assert_eq!(commands.len(), 1);
        assert!(matches!(
            commands[0].command(),
            ScriptCommand::ParseFile { parser_name, path: command_path, .. }
                if parser_name == "raw.py" && command_path.as_path() == path.as_path()
        ));
        panel.finish_parser_dispatch(commands.into_iter().next().unwrap(), Ok(()));

        assert!(panel.parsers.has_pending_work());
        assert!(!panel.can_interrupt_console());
    }

    #[test]
    fn ordinary_run_defers_validation_without_marking_it_dispatched() {
        let root = std::env::temp_dir().join(format!(
            "delog-scripts-reverse-validation-{}",
            std::process::id()
        ));
        let mut panel = ScriptsPanel::new(root.join("scripts"), root.join("parsers"));
        panel.parsers.add_new();
        let action = panel.parsers.stage_save().unwrap();
        panel.running = true;
        panel.queue_parser_action(action);

        assert!(panel.take_ready_parser_commands().is_empty());
        assert!(!panel.parsers.validation_dispatched());

        panel.handle_event(ScriptEvent::Done);
        let commands = panel.take_ready_parser_commands();
        assert_eq!(commands.len(), 1);
        assert!(matches!(
            commands[0].command(),
            ScriptCommand::ValidateParser { name, .. } if name == "new_parser.py"
        ));
        panel.finish_parser_dispatch(commands.into_iter().next().unwrap(), Ok(()));
        assert!(panel.parsers.validation_dispatched());
        assert!(!panel.can_interrupt_console());
    }

    #[test]
    fn dispatched_validation_polls_events_without_changing_console_state() {
        let root = std::env::temp_dir().join(format!(
            "delog-scripts-parser-pending-{}",
            std::process::id()
        ));
        let mut panel = ScriptsPanel::new(root.join("scripts"), root.join("parsers"));
        panel.running = false;
        panel.parsers.add_new();
        let action = panel.parsers.stage_save().unwrap();

        let ParserUiAction::ValidateAndSave { name, .. } = action;
        panel.finish_validation_dispatch(&name, Ok(()));

        assert!(panel.should_poll_parser_events());
        assert!(!panel.running);
        panel.handle_event(ScriptEvent::Parser(ParserEvent::SyntaxValid {
            name: "new_parser.py".into(),
        }));
        assert!(!panel.should_poll_parser_events());
    }

    #[test]
    fn first_parse_terminal_keeps_polling_for_second_dispatch() {
        let root =
            std::env::temp_dir().join(format!("delog-scripts-parser-queue-{}", std::process::id()));
        let mut panel = ScriptsPanel::new(root.join("scripts"), root.join("parsers"));
        let first = PathBuf::from("first.raw");
        let second = PathBuf::from("second.raw");
        panel.parsers.mark_parse_dispatched("raw.py", &first);
        panel.parsers.mark_parse_dispatched("raw.py", &second);

        panel.handle_event(ScriptEvent::Parser(ParserEvent::Succeeded {
            parser_name: "raw.py".into(),
            path: first,
            topics: 1,
            rows: 1,
        }));

        assert!(panel.should_poll_parser_events());
        assert!(panel.is_parser_running());
        panel.handle_event(ScriptEvent::Parser(ParserEvent::Failed {
            parser_name: "raw.py".into(),
            path: Some(second),
            message: "failed".into(),
        }));
        assert!(!panel.should_poll_parser_events());
        assert!(!panel.is_parser_running());
    }

    #[test]
    fn parser_names_propagates_library_errors() {
        let root = std::env::temp_dir().join(format!(
            "delog-scripts-parser-list-error-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&root);
        std::fs::write(&root, "not a directory").unwrap();
        let mut panel = ScriptsPanel::new(root.join("scripts"), root.clone());

        assert!(panel.parser_names().is_err());
        assert!(panel.parser_names().is_err());
        assert_eq!(panel.take_parser_diagnostics().len(), 1);

        std::fs::remove_file(root).unwrap();
    }

    #[test]
    fn failed_validation_dispatch_does_not_start_polling() {
        let root = std::env::temp_dir().join(format!(
            "delog-scripts-parser-dispatch-error-{}",
            std::process::id()
        ));
        let mut panel = ScriptsPanel::new(root.join("scripts"), root.join("parsers"));
        panel.parsers.add_new();
        panel.parsers.stage_save().unwrap();

        panel.finish_validation_dispatch("new_parser.py", Err("disconnected".into()));

        assert!(!panel.should_poll_parser_events());
        assert!(
            panel
                .take_parser_diagnostics()
                .join("\n")
                .contains("disconnected")
        );
    }

    #[test]
    fn failed_parse_dispatch_does_not_start_polling() {
        let root = std::env::temp_dir().join(format!(
            "delog-scripts-parse-dispatch-error-{}",
            std::process::id()
        ));
        let mut panel = ScriptsPanel::new(root.join("scripts"), root.join("parsers"));
        let path = PathBuf::from("flight.raw");

        panel.finish_parse_dispatch("raw.py", &path, Err("disconnected".into()));

        assert!(!panel.should_poll_parser_events());
        assert!(
            panel
                .take_parser_diagnostics()
                .join("\n")
                .contains("disconnected")
        );
    }

    #[test]
    fn failed_parser_cancel_records_diagnostic_without_clearing_pending() {
        let root = std::env::temp_dir().join(format!(
            "delog-scripts-parser-cancel-error-{}",
            std::process::id()
        ));
        let mut panel = ScriptsPanel::new(root.join("scripts"), root.join("parsers"));
        panel
            .parsers
            .mark_parse_dispatched("raw.py", &PathBuf::from("flight.raw"));

        panel.finish_cancel_dispatch(Err("pending-call queue full".into()));

        assert!(panel.is_parser_running());
        assert!(
            panel
                .take_parser_diagnostics()
                .join("\n")
                .contains("pending-call queue full")
        );
    }
}
