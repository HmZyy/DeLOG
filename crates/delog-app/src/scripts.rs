use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::logging::{LogLevel, PendingLog, log};
use crate::settings::AutoOpenVariables;
use delog_core::ingest::IngestSender;
use delog_core::metrics::MetricsRegistry;
use delog_core::snapshot::DataStore;
use delog_script::library::ScriptLibrary;
use delog_script::params::{ParamSpec, ParamValue};
use delog_script::{MarkerCommand, ScriptCommand, ScriptEngine, ScriptEvent};
use egui_code_editor::{CodeEditor, ColorTheme, Syntax};

use crate::parsers::{ParserUiAction, ParsersPanel};
use crate::repl_complete::{self, ReplCompletion};
use crate::repl_history::ReplHistory;

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

/// One script's declared params, snapshotted so the Variables window doesn't
/// hold the store lock across egui closures.
struct ScriptVarsView {
    name: String,
    has_snapshot: bool,
    // Not read yet: reserved for a future "live params" indicator.
    #[allow(dead_code)]
    has_live: bool,
    specs: Vec<ParamSpec>,
    values: HashMap<String, ParamValue>,
}

fn should_open_variables(
    mode: AutoOpenVariables,
    prior: &HashSet<String>,
    current: &HashSet<String>,
) -> bool {
    match mode {
        AutoOpenVariables::Never => false,
        AutoOpenVariables::EveryRun => !current.is_empty(),
        AutoOpenVariables::NewlyAdded => current.difference(prior).next().is_some(),
    }
}

struct PendingAutoOpen {
    script: String,
    prior_names: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsoleEventKind {
    Output,
    Error,
}

fn should_open_scripting_console(
    mode: crate::settings::AutoOpenScriptingConsole,
    event: ConsoleEventKind,
) -> bool {
    match mode {
        crate::settings::AutoOpenScriptingConsole::OnOutput => true,
        crate::settings::AutoOpenScriptingConsole::OnErrors => event == ConsoleEventKind::Error,
        crate::settings::AutoOpenScriptingConsole::Never => false,
    }
}

pub struct ScriptsPanel {
    pub open: bool,
    pub console_open: bool,
    engine: Option<ScriptEngine>,
    library: ScriptLibrary,
    current_name: String,
    editing_original_name: Option<String>,
    editor_text: String,
    repl_input: String,
    refocus_repl_input: bool,
    console: String,
    status: String,
    pending_delete: Option<String>,
    running: bool,
    parsers: ParsersPanel,
    deferred_parser_actions: VecDeque<ParserUiAction>,
    params: delog_script::params::SharedParams,
    params_file: std::path::PathBuf,
    pub variables_open: bool,
    pending_auto_open: Option<PendingAutoOpen>,
    auto_open_mode: AutoOpenVariables,
    use_original_timestamps: bool,
    pending_logs: Vec<PendingLog>,
    pending_marker_commands: Vec<MarkerCommand>,
    completion: ReplCompletion,
    history: ReplHistory,
}

impl ScriptsPanel {
    pub fn new(
        scripts_dir: std::path::PathBuf,
        parsers_dir: std::path::PathBuf,
        params_file: std::path::PathBuf,
    ) -> Self {
        let library = ScriptLibrary::new(scripts_dir);
        let params = delog_script::params::shared_empty();
        {
            let loaded = crate::script_params_io::load(&params_file);
            crate::script_params_io::apply_loaded(&mut params.lock().unwrap(), loaded);
        }
        Self {
            open: false,
            console_open: false,
            engine: None,
            library,
            current_name: String::new(),
            editing_original_name: None,
            editor_text: String::new(),
            repl_input: String::new(),
            refocus_repl_input: false,
            console: String::new(),
            status: String::new(),
            pending_delete: None,
            running: false,
            parsers: ParsersPanel::new(parsers_dir),
            deferred_parser_actions: VecDeque::new(),
            params,
            params_file,
            variables_open: false,
            pending_auto_open: None,
            auto_open_mode: AutoOpenVariables::default(),
            use_original_timestamps: false,
            pending_logs: Vec::new(),
            pending_marker_commands: Vec::new(),
            completion: ReplCompletion::new(),
            history: ReplHistory::new(),
        }
    }

    fn save_params(&self) {
        if let Err(e) =
            crate::script_params_io::save(&self.params_file, &self.params.lock().unwrap())
        {
            eprintln!("failed to save script params: {e}");
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

    pub fn open_parser_editor(&mut self) {
        self.parsers.open_editor();
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

    pub fn take_logs(&mut self) -> Vec<PendingLog> {
        std::mem::take(&mut self.pending_logs)
    }

    pub fn take_marker_commands(&mut self) -> Vec<MarkerCommand> {
        std::mem::take(&mut self.pending_marker_commands)
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

    pub fn set_console_open(&mut self, open: bool) {
        if open && !self.console_open {
            self.request_repl_refocus();
        }
        self.console_open = open;
    }

    fn request_repl_refocus(&mut self) {
        self.refocus_repl_input = true;
    }

    fn take_repl_refocus_request(&mut self) -> bool {
        if !self.ordinary_dispatch_enabled() || !self.refocus_repl_input {
            return false;
        }
        self.refocus_repl_input = false;
        true
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

    pub fn edit_named(&mut self, name: &str) {
        match self.library.load(name) {
            Ok(source) => {
                self.current_name = name.to_owned();
                self.editing_original_name = Some(name.to_owned());
                self.editor_text = source;
                self.status = format!("editing {name}");
                self.open = true;
            }
            Err(e) => self.status = format!("load failed: {e}"),
        }
    }

    pub fn new_script(&mut self) {
        self.current_name = "new_script".to_owned();
        self.editing_original_name = None;
        self.editor_text.clear();
        self.status.clear();
        self.open = true;
    }

    pub fn duplicate_script(&mut self, name: &str) {
        match self.library.load(name) {
            Ok(source) => {
                let copy_name = available_copy_name(&self.script_names(), name);
                match self.library.save(&copy_name, &source) {
                    Ok(()) => {
                        self.current_name = copy_name.clone();
                        self.editing_original_name = Some(copy_name.clone());
                        self.editor_text = source;
                        self.status = format!("duplicated {name} as {copy_name}");
                        self.open = true;
                    }
                    Err(e) => self.status = format!("duplicate failed: {e}"),
                }
            }
            Err(e) => self.status = format!("load failed: {e}"),
        }
    }

    pub fn request_delete(&mut self, name: &str) {
        self.pending_delete = Some(name.to_owned());
    }

    /// Read fresh from disk so newly-saved scripts appear without restarting.
    pub fn script_names(&self) -> Vec<String> {
        self.library.list().unwrap_or_default()
    }

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
        let prior_names: HashSet<String> = self
            .params
            .lock()
            .unwrap()
            .scripts
            .get(&name)
            .map(|sp| sp.specs.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default();
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
                self.pending_auto_open = Some(PendingAutoOpen {
                    script: name.clone(),
                    prior_names,
                });
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
        let params = Arc::clone(&self.params);
        let use_original_timestamps = self.use_original_timestamps;
        let engine = self
            .engine
            .get_or_insert_with(|| ScriptEngine::spawn(store, sender, metrics, params));
        engine.set_use_original_timestamps(use_original_timestamps);
        engine
    }

    /// Returns `None` rather than spawning the engine: a live transform only
    /// exists if a script already ran (which spawned the engine).
    pub fn live_batch_sender_if_running(
        &self,
    ) -> Option<std::sync::mpsc::Sender<delog_script::LiveBatchInput>> {
        self.engine.as_ref().map(|e| e.live_batch_sender())
    }

    fn drain(&mut self, auto_open_console: crate::settings::AutoOpenScriptingConsole) {
        let events = self
            .engine
            .as_ref()
            .map(ScriptEngine::drain_events)
            .unwrap_or_default();
        for event in events {
            self.handle_event_with_console_policy(event, auto_open_console);
        }
    }

    #[cfg(test)]
    fn handle_event(&mut self, event: ScriptEvent) {
        self.handle_event_with_console_policy(
            event,
            crate::settings::AutoOpenScriptingConsole::Never,
        );
    }

    fn handle_event_with_console_policy(
        &mut self,
        event: ScriptEvent,
        auto_open_console: crate::settings::AutoOpenScriptingConsole,
    ) {
        match event {
            ScriptEvent::Output(s) => {
                self.console.push_str(&s);
                if should_open_scripting_console(auto_open_console, ConsoleEventKind::Output) {
                    self.set_console_open(true);
                }
            }
            ScriptEvent::Result(r) => {
                self.console.push_str(&r);
                self.console.push('\n');
                if should_open_scripting_console(auto_open_console, ConsoleEventKind::Output) {
                    self.set_console_open(true);
                }
            }
            ScriptEvent::Error(e) => {
                self.console.push_str(&e);
                self.console.push('\n');
                self.pending_logs.push(log(LogLevel::Error, e.clone()));
                self.status = "error".into();
                self.running = false;
                self.pending_auto_open = None;
                if should_open_scripting_console(auto_open_console, ConsoleEventKind::Error) {
                    self.set_console_open(true);
                }
            }
            ScriptEvent::Done => {
                self.status = "done".into();
                self.running = false;
                if let Some(pending) = self.pending_auto_open.take() {
                    let current_names: HashSet<String> = self
                        .params
                        .lock()
                        .unwrap()
                        .scripts
                        .get(&pending.script)
                        .map(|sp| sp.specs.iter().map(|s| s.name.clone()).collect())
                        .unwrap_or_default();
                    if should_open_variables(
                        self.auto_open_mode,
                        &pending.prior_names,
                        &current_names,
                    ) {
                        self.variables_open = true;
                    }
                }
            }
            ScriptEvent::Markers(command) => self.pending_marker_commands.push(command),
            ScriptEvent::LiveBatchProcessed => {}
            ScriptEvent::Parser(event) => self.parsers.handle_event(event),
            ScriptEvent::Completions { seq, matches } => {
                self.completion
                    .on_completions(seq, matches, &mut self.repl_input);
            }
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
        auto_open: AutoOpenVariables,
        auto_open_console: crate::settings::AutoOpenScriptingConsole,
        use_original_timestamps: bool,
    ) {
        self.auto_open_mode = auto_open;
        self.use_original_timestamps = use_original_timestamps;
        if let Some(engine) = &self.engine {
            engine.set_use_original_timestamps(use_original_timestamps);
        }
        self.drain(auto_open_console);

        for action in self.parsers.ui(ctx, self.parser_dispatch_enabled()) {
            self.queue_parser_action(action);
        }
        for command in self.take_ready_parser_commands() {
            let result = self
                .engine(store.clone(), sender.clone(), Arc::clone(&metrics))
                .send(command.command());
            self.finish_parser_dispatch(command, result);
        }

        // Unconditional so a menu-triggered Remove can be confirmed even when
        // the Console window is closed.
        self.delete_confirm_ui(ctx);

        self.variables_window(ctx, &store, &sender, &metrics);

        if self.open {
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
        } else if self.should_poll_parser_events() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }

        if self.open || self.variables_open || self.console_open {
            ctx.request_repaint(); // keep draining engine events while open
        }
    }

    pub fn console_dock_ui(
        &mut self,
        ui: &mut egui::Ui,
        store: &Arc<DataStore>,
        sender: &IngestSender,
        metrics: &Arc<MetricsRegistry>,
    ) {
        egui::Panel::bottom("scripting_console_input")
            .resizable(false)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(">>>");
                    let dispatch_enabled = self.ordinary_dispatch_enabled();
                    // Pin the trash button flush right, then let the REPL input fill
                    // the remaining space to its left.
                    let resp = ui
                        .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let trash = egui::Image::new(crate::icons::trash())
                                .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
                                .tint(ui.visuals().text_color());
                            if ui
                                .add(egui::Button::image(trash))
                                .on_hover_text("Clear console")
                                .clicked()
                            {
                                self.console.clear();
                            }

                            let repl_width = ui.available_width();
                            ui.add_enabled(
                                dispatch_enabled,
                                egui::TextEdit::singleline(&mut self.repl_input)
                                    .desired_width(repl_width)
                                    .lock_focus(true),
                            )
                        })
                        .inner;
                    if dispatch_enabled && self.take_repl_refocus_request() {
                        resp.request_focus();
                    }

                    // The popup owns Up/Down/Tab/Enter/Esc while it is open.
                    let popup_took_enter =
                        self.completion
                            .handle_popup(ui, &resp, &mut self.repl_input);

                    // Typing (not navigation) while the popup is open dismisses it
                    // and lets the character pass through to the input.
                    if resp.changed() {
                        self.completion.dismiss();
                    }

                    // A completion that mutated the buffer moves the caret to the
                    // end of the inserted text.
                    if let Some(byte) = self.completion.take_pending_cursor() {
                        repl_complete::set_cursor_byte(ui.ctx(), resp.id, &self.repl_input, byte);
                        resp.request_focus();
                    }

                    // With no popup open, Up/Down walk the command history.
                    if dispatch_enabled && resp.has_focus() && !self.completion.is_open() {
                        let up = ui.input_mut(|i| {
                            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                        });
                        let down = ui.input_mut(|i| {
                            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                        });
                        let recalled = if up {
                            self.history.older(&self.repl_input)
                        } else if down {
                            self.history.newer()
                        } else {
                            None
                        };
                        if let Some(line) = recalled {
                            self.repl_input = line;
                            let end = self.repl_input.len();
                            repl_complete::set_cursor_byte(
                                ui.ctx(),
                                resp.id,
                                &self.repl_input,
                                end,
                            );
                            resp.request_focus();
                        }
                    }

                    // Tab (or Ctrl+N) with no popup open requests completions for
                    // the token at the cursor.
                    if dispatch_enabled
                        && resp.has_focus()
                        && !self.completion.is_open()
                        && ui.input_mut(|i| {
                            i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                                || i.consume_key(egui::Modifiers::CTRL, egui::Key::N)
                        })
                    {
                        let cursor =
                            repl_complete::cursor_byte(ui.ctx(), resp.id, &self.repl_input);
                        if let Some((start, token)) =
                            repl_complete::completable_token(&self.repl_input, cursor)
                        {
                            let token = token.to_string();
                            let seq = self.completion.begin_request(start, cursor, token.clone());
                            let _ = self
                                .engine(store.clone(), sender.clone(), Arc::clone(metrics))
                                .send(ScriptCommand::Complete { seq, text: token });
                        }
                    }

                    if !popup_took_enter
                        && !self.completion.is_open()
                        && resp.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        let line = std::mem::take(&mut self.repl_input);
                        self.history.push(&line);
                        if self.dispatch_eval(
                            line,
                            store.clone(),
                            sender.clone(),
                            Arc::clone(metrics),
                        ) {
                            self.request_repl_refocus();
                        }
                    }
                });
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.monospace(self.console.as_str());
                });
        });
    }

    fn variables_window(
        &mut self,
        ctx: &egui::Context,
        store: &Arc<DataStore>,
        sender: &IngestSender,
        metrics: &Arc<MetricsRegistry>,
    ) {
        if !self.variables_open {
            return;
        }
        let mut open = self.variables_open;

        // Snapshot the store so we don't hold the lock across egui closures.
        let mut views: Vec<ScriptVarsView> = {
            let s = self.params.lock().unwrap();
            let mut v: Vec<_> = s
                .scripts
                .iter()
                .filter(|(_, sp)| !sp.specs.is_empty())
                .map(|(name, sp)| ScriptVarsView {
                    name: name.clone(),
                    has_snapshot: sp.has_snapshot,
                    has_live: sp.has_live,
                    specs: sp.specs.clone(),
                    values: sp.values.clone(),
                })
                .collect();
            v.sort_by(|a, b| a.name.cmp(&b.name));
            v
        };

        // Edits committed this frame: (script, param, new value, has_snapshot).
        let mut commits: Vec<(String, String, ParamValue, bool)> = Vec::new();
        // Resets committed this frame: (script, param, has_snapshot).
        let mut resets: Vec<(String, String, bool)> = Vec::new();

        egui::Window::new("Script Variables")
            .open(&mut open)
            .collapsible(false)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .default_size([360.0, 420.0])
            .show(ctx, |ui| {
                if views.is_empty() {
                    ui.label("No script has declared variables yet.");
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for view in views.iter_mut() {
                        egui::CollapsingHeader::new(view.name.as_str())
                            .default_open(true)
                            .show(ui, |ui| {
                                egui::Grid::new(format!("vars_{}", view.name))
                                    .num_columns(3)
                                    .spacing([8.0, 6.0])
                                    .show(ui, |ui| {
                                        for spec in view.specs.iter() {
                                            let value = view
                                                .values
                                                .get(&spec.name)
                                                .cloned()
                                                .unwrap_or_else(|| spec.default.clone());
                                            ui.label(&spec.label);
                                            let committed = render_param_widget(ui, spec, value);
                                            if let Some(new_value) = committed {
                                                commits.push((
                                                    view.name.clone(),
                                                    spec.name.clone(),
                                                    new_value,
                                                    view.has_snapshot,
                                                ));
                                            }
                                            let reset =
                                                ui
                                                    .add(
                                                        egui::Button::image(
                                                            egui::Image::new(
                                                                crate::icons::rotate_ccw(),
                                                            )
                                                            .fit_to_exact_size(egui::vec2(
                                                                14.0, 14.0,
                                                            ))
                                                            .tint(ui.visuals().text_color()),
                                                        )
                                                        .frame(false),
                                                    )
                                                    .on_hover_text("Reset to default");
                                            if reset.clicked() {
                                                resets.push((
                                                    view.name.clone(),
                                                    spec.name.clone(),
                                                    view.has_snapshot,
                                                ));
                                            }
                                            ui.end_row();
                                        }
                                    });
                            });
                    }
                });
            });
        self.variables_open = open;

        if commits.is_empty() && resets.is_empty() {
            return;
        }

        // Apply edits: write store, persist, and re-run named snapshot scripts.
        let named: std::collections::HashSet<String> = self
            .library
            .list()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut to_rerun: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        {
            let mut s = self.params.lock().unwrap();
            for (script, name, value, has_snapshot) in &commits {
                s.set_value(script, name, value.clone());
                if crate::script_params_io::should_rerun(*has_snapshot, named.contains(script)) {
                    to_rerun.insert(script.clone());
                }
            }
            for (script, name, has_snapshot) in &resets {
                s.reset_value(script, name);
                if crate::script_params_io::should_rerun(*has_snapshot, named.contains(script)) {
                    to_rerun.insert(script.clone());
                }
            }
        }
        self.save_params();
        for script in to_rerun {
            self.run_named(
                &script,
                Arc::clone(store),
                sender.clone(),
                Arc::clone(metrics),
            );
        }
    }

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
        // Closing via [x] is treated as cancel.
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
        egui::Panel::left("scripts_library_drawer")
            .resizable(true)
            .default_size(180.0)
            .size_range(140.0..=260.0)
            .show_inside(ui, |ui| self.script_drawer(ui));
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
                        Ok(()) => {
                            if let Some(original) = self.editing_original_name.take() {
                                if original != self.current_name {
                                    let _ = self.library.delete(&original);
                                }
                            }
                            self.editing_original_name = Some(self.current_name.clone());
                            self.status = format!("saved {}", self.current_name);
                        }
                        Err(e) => self.status = format!("save failed: {e}"),
                    }
                }

                // An unsaved buffer runs as "scratch" so its output (and any
                // live transform) stays addressable.
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

    fn script_drawer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Scripts");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+ New").clicked() {
                    self.new_script();
                }
            });
        });
        ui.separator();
        let names = self.script_names();
        if names.is_empty() {
            ui.weak("No saved scripts.");
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for name in names {
                    ui.horizontal(|ui| {
                        let selected = self.editing_original_name.as_deref() == Some(name.as_str());
                        if ui
                            .selectable_label(selected, name.as_str())
                            .on_hover_text("Load script")
                            .clicked()
                        {
                            self.edit_named(&name);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.menu_button("...", |ui| {
                                if ui.button("Edit").clicked() {
                                    self.edit_named(&name);
                                    ui.close();
                                }
                                if ui.button("Duplicate").clicked() {
                                    self.duplicate_script(&name);
                                    ui.close();
                                }
                                if ui.button("Remove").clicked() {
                                    self.request_delete(&name);
                                    ui.close();
                                }
                            });
                        });
                    });
                }
            });
    }
}

fn available_copy_name(existing: &[String], name: &str) -> String {
    let base = format!("{name}_copy");
    if !existing.iter().any(|candidate| candidate == &base) {
        return base;
    }
    for i in 2.. {
        let candidate = format!("{base}_{i}");
        if !existing.iter().any(|existing| existing == &candidate) {
            return candidate;
        }
    }
    unreachable!()
}

#[allow(dead_code)]
fn icon_btn(ui: &mut egui::Ui, icon: egui::ImageSource<'static>, hover: &str) -> egui::Response {
    icon_btn_enabled(ui, true, icon, hover)
}

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

/// Render one param's widget and return `Some(new_value)` only when the edit
/// is *committed* (slider drag released, checkbox/combo click, or Enter in a
/// text field) — not on every intermediate change.
fn render_param_widget(
    ui: &mut egui::Ui,
    spec: &ParamSpec,
    value: ParamValue,
) -> Option<ParamValue> {
    use delog_script::params::ParamKind;
    match (&spec.kind, value) {
        (
            ParamKind::Slider {
                min,
                max,
                step,
                integer,
            },
            ParamValue::Float(mut v),
        ) => {
            let mut slider = egui::Slider::new(&mut v, *min..=*max);
            if *integer {
                slider = slider.step_by(step.unwrap_or(1.0)).max_decimals(0);
            } else if let Some(s) = step {
                slider = slider.step_by(*s);
            }
            let resp = ui.add(slider);
            // Commit at the end of a drag or on a keyboard/typed change.
            if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                let out = if *integer { v.round() } else { v };
                Some(ParamValue::Float(out))
            } else {
                None
            }
        }
        (ParamKind::Checkbox, ParamValue::Bool(mut b)) => {
            if ui.checkbox(&mut b, "").changed() {
                Some(ParamValue::Bool(b))
            } else {
                None
            }
        }
        (ParamKind::Combo { options }, ParamValue::Text(current)) => {
            let mut selected = current.clone();
            let mut changed = false;
            egui::ComboBox::from_id_salt(format!("combo_{}", spec.name))
                .selected_text(selected.clone())
                .show_ui(ui, |ui| {
                    for opt in options {
                        if ui
                            .selectable_value(&mut selected, opt.clone(), opt)
                            .clicked()
                        {
                            changed = true;
                        }
                    }
                });
            changed.then_some(ParamValue::Text(selected))
        }
        (ParamKind::Text, ParamValue::Text(mut t)) => {
            let resp = ui.add(egui::TextEdit::singleline(&mut t).desired_width(160.0));
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                Some(ParamValue::Text(t))
            } else {
                None
            }
        }
        // Value/kind mismatch (shouldn't happen): render nothing editable.
        _ => {
            ui.label("(type mismatch)");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
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
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
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
    fn console_auto_open_policy_matches_setting() {
        assert!(should_open_scripting_console(
            crate::settings::AutoOpenScriptingConsole::OnOutput,
            ConsoleEventKind::Output,
        ));
        assert!(should_open_scripting_console(
            crate::settings::AutoOpenScriptingConsole::OnOutput,
            ConsoleEventKind::Error,
        ));
        assert!(!should_open_scripting_console(
            crate::settings::AutoOpenScriptingConsole::OnErrors,
            ConsoleEventKind::Output,
        ));
        assert!(should_open_scripting_console(
            crate::settings::AutoOpenScriptingConsole::OnErrors,
            ConsoleEventKind::Error,
        ));
        assert!(!should_open_scripting_console(
            crate::settings::AutoOpenScriptingConsole::Never,
            ConsoleEventKind::Error,
        ));
    }

    #[test]
    fn script_errors_are_buffered_for_logging_dock() {
        let root =
            std::env::temp_dir().join(format!("delog-scripts-error-log-{}", std::process::id()));
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );

        panel.handle_event(ScriptEvent::Error("python exploded".into()));

        let logs = panel.take_logs();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, crate::logging::LogLevel::Error);
        assert!(logs[0].message.contains("python exploded"));
        assert!(panel.take_logs().is_empty());
    }

    #[test]
    fn marker_events_are_buffered_without_console_or_completion_side_effects() {
        let root =
            std::env::temp_dir().join(format!("delog-scripts-marker-event-{}", std::process::id()));
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
        panel.running = true;
        panel.status = "running console script".into();
        panel.console = "existing output\n".into();
        panel.console_open = false;
        panel.variables_open = false;
        panel.refocus_repl_input = false;
        panel.repl_input = "ma".into();
        let completion_seq = panel.completion.begin_request(0, 2, "ma".into());
        let command = delog_script::MarkerCommand::Append {
            owner: "console".into(),
            generation: 7,
            markers: vec![],
        };

        panel.handle_event(ScriptEvent::Markers(command.clone()));

        assert!(panel.running);
        assert_eq!(panel.status, "running console script");
        assert_eq!(panel.console, "existing output\n");
        assert!(!panel.console_open);
        assert!(!panel.variables_open);
        assert!(!panel.refocus_repl_input);
        assert_eq!(panel.repl_input, "ma");
        assert_eq!(panel.take_marker_commands(), vec![command]);
        assert!(panel.take_marker_commands().is_empty());

        panel.handle_event(ScriptEvent::Completions {
            seq: completion_seq,
            matches: vec!["marker".into()],
        });
        assert_eq!(panel.repl_input, "marker");
    }

    #[test]
    fn new_script_starts_named_empty_buffer_in_editor() {
        let root =
            std::env::temp_dir().join(format!("delog-scripts-new-buffer-{}", std::process::id()));
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
        panel.current_name = "old".into();
        panel.editor_text = "print('old')".into();

        panel.new_script();

        assert_eq!(panel.current_name, "new_script");
        assert!(panel.editor_text.is_empty());
        assert!(panel.open);
    }

    #[test]
    fn duplicate_script_copies_source_to_available_name_and_opens_copy() {
        let root =
            std::env::temp_dir().join(format!("delog-scripts-duplicate-{}", std::process::id()));
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
        panel.library.save("demo", "print('demo')").unwrap();
        panel.library.save("demo_copy", "old copy").unwrap();

        panel.duplicate_script("demo");

        assert_eq!(panel.current_name, "demo_copy_2");
        assert_eq!(panel.editing_original_name.as_deref(), Some("demo_copy_2"));
        assert_eq!(panel.editor_text, "print('demo')");
        assert_eq!(panel.library.load("demo_copy_2").unwrap(), "print('demo')");
        assert!(panel.open);
    }

    #[test]
    fn parser_pending_blocks_saved_runs_and_repl_until_terminal_event() {
        let root = std::env::temp_dir().join(format!(
            "delog-scripts-shared-worker-{}",
            std::process::id()
        ));
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
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
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
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
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
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
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
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
    fn repl_refocus_request_waits_until_prompt_is_enabled() {
        let root =
            std::env::temp_dir().join(format!("delog-scripts-repl-focus-{}", std::process::id()));
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );

        panel.request_repl_refocus();
        panel.running = true;
        assert!(!panel.take_repl_refocus_request());

        panel.handle_event(ScriptEvent::Done);
        assert!(panel.take_repl_refocus_request());
        assert!(!panel.take_repl_refocus_request());
    }

    #[test]
    fn opening_console_requests_repl_focus_once() {
        let root =
            std::env::temp_dir().join(format!("delog-scripts-open-focus-{}", std::process::id()));
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );

        panel.set_console_open(true);
        assert!(panel.console_open);
        assert!(panel.take_repl_refocus_request());
        assert!(!panel.take_repl_refocus_request());

        panel.set_console_open(true);
        assert!(!panel.take_repl_refocus_request());
    }

    #[test]
    fn first_parse_terminal_keeps_polling_for_second_dispatch() {
        let root =
            std::env::temp_dir().join(format!("delog-scripts-parser-queue-{}", std::process::id()));
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
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
        let mut panel =
            ScriptsPanel::new(root.join("scripts"), root.clone(), root.join("params.json"));

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
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
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
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
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
        let mut panel = ScriptsPanel::new(
            root.join("scripts"),
            root.join("parsers"),
            root.join("params.json"),
        );
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

    #[test]
    fn auto_open_never_stays_closed() {
        let prior = HashSet::new();
        let current: HashSet<String> = ["gain".into()].into_iter().collect();
        assert!(!should_open_variables(
            crate::settings::AutoOpenVariables::Never,
            &prior,
            &current
        ));
    }

    #[test]
    fn auto_open_every_run_opens_when_params_exist() {
        let prior: HashSet<String> = ["gain".into()].into_iter().collect();
        let current: HashSet<String> = ["gain".into()].into_iter().collect();
        assert!(should_open_variables(
            crate::settings::AutoOpenVariables::EveryRun,
            &prior,
            &current
        ));
    }

    #[test]
    fn auto_open_every_run_stays_closed_without_params() {
        let empty = HashSet::new();
        assert!(!should_open_variables(
            crate::settings::AutoOpenVariables::EveryRun,
            &empty,
            &empty
        ));
    }

    #[test]
    fn auto_open_newly_added_opens_on_first_param() {
        let prior = HashSet::new();
        let current: HashSet<String> = ["gain".into()].into_iter().collect();
        assert!(should_open_variables(
            crate::settings::AutoOpenVariables::NewlyAdded,
            &prior,
            &current
        ));
    }

    #[test]
    fn auto_open_newly_added_opens_on_added_param() {
        let prior: HashSet<String> = ["gain".into()].into_iter().collect();
        let current: HashSet<String> = ["gain".into(), "freq".into()].into_iter().collect();
        assert!(should_open_variables(
            crate::settings::AutoOpenVariables::NewlyAdded,
            &prior,
            &current
        ));
    }

    #[test]
    fn auto_open_newly_added_stays_closed_on_unchanged_params() {
        let prior: HashSet<String> = ["gain".into()].into_iter().collect();
        let current: HashSet<String> = ["gain".into()].into_iter().collect();
        assert!(!should_open_variables(
            crate::settings::AutoOpenVariables::NewlyAdded,
            &prior,
            &current
        ));
    }
}
