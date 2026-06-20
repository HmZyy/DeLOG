//! Custom Python parser editor and parser-run lifecycle state (SCR-11).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use delog_script::ParserEvent;
use delog_script::parser_library::ParserLibrary;
use egui_code_editor::{CodeEditor, ColorTheme, Syntax};

#[allow(dead_code)] // Used by the Task 6 menu through `add_new`.
const NEW_PARSER_NAME: &str = "new_parser.py";
#[allow(dead_code)] // Used by the Task 6 menu through `add_new`.
const NEW_PARSER_TEMPLATE: &str = r#"def Parse(raw_data):
    # Return [(name, values, tooltip), ...].
    return []
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseRequest {
    pub parser_name: String,
    pub source: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParserUiAction {
    ValidateAndSave {
        old_name: Option<String>,
        name: String,
        source: String,
    },
}

#[derive(Debug, Clone)]
struct PendingSave {
    old_name: Option<String>,
    name: String,
    source: String,
}

#[allow(dead_code)] // Labels are consumed by the Task 6 toolbar.
#[derive(Debug, Clone)]
enum ParsePhase {
    Queued { parser: String, path: PathBuf },
    Reading { parser: String, path: PathBuf },
    Running { parser: String, path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingParse {
    parser: String,
    path: PathBuf,
}

pub struct ParsersPanel {
    library: ParserLibrary,
    filename: String,
    source: String,
    status: String,
    open: bool,
    saved_original_name: Option<String>,
    pending_delete: Option<String>,
    pending_save: Option<PendingSave>,
    validation_dispatched: bool,
    pending_parses: VecDeque<PendingParse>,
    phase: Option<ParsePhase>,
    error_popup: Option<String>,
    diagnostics: Vec<String>,
    last_list_error: Option<String>,
    parse_requests: Receiver<ParseRequest>,
    #[allow(dead_code)] // Native picker entry point is surfaced by Task 6.
    parse_requests_tx: Sender<ParseRequest>,
}

impl ParsersPanel {
    pub fn new(parsers_dir: PathBuf) -> Self {
        let (parse_requests_tx, parse_requests) = mpsc::channel();
        Self {
            library: ParserLibrary::new(parsers_dir),
            filename: String::new(),
            source: String::new(),
            status: String::new(),
            open: false,
            saved_original_name: None,
            pending_delete: None,
            pending_save: None,
            validation_dispatched: false,
            pending_parses: VecDeque::new(),
            phase: None,
            error_popup: None,
            diagnostics: Vec::new(),
            last_list_error: None,
            parse_requests,
            parse_requests_tx,
        }
    }

    #[allow(dead_code)] // Task 6 menu facade.
    pub fn list(&mut self) -> std::io::Result<Vec<String>> {
        match self.library.list() {
            Ok(names) => {
                self.last_list_error = None;
                Ok(names)
            }
            Err(error) => {
                let message = format!("list custom parsers: {error}");
                if self.last_list_error.as_deref() != Some(&message) {
                    self.diagnostics.push(message.clone());
                    self.last_list_error = Some(message);
                }
                Err(error)
            }
        }
    }

    #[allow(dead_code)] // Task 6 menu facade.
    pub fn add_new(&mut self) {
        if self.pending_save.is_some() {
            self.status = "finish the current parser validation before creating another".into();
            return;
        }
        self.filename = NEW_PARSER_NAME.to_owned();
        self.source = NEW_PARSER_TEMPLATE.to_owned();
        self.saved_original_name = None;
        self.pending_save = None;
        self.validation_dispatched = false;
        self.status.clear();
        self.open = true;
    }

    #[allow(dead_code)] // Task 6 menu facade.
    pub fn edit(&mut self, name: &str) {
        if self.pending_save.is_some() {
            self.status = "finish the current parser validation before reopening a parser".into();
            return;
        }
        match self.library.load(name) {
            Ok(source) => {
                self.filename = name.to_owned();
                self.source = source;
                self.saved_original_name = Some(name.to_owned());
                self.pending_save = None;
                self.validation_dispatched = false;
                self.status = format!("editing {name}");
                self.open = true;
            }
            Err(error) => self.record_error(format!("load parser {name}: {error}")),
        }
    }

    /// Load the parser before opening the native dialog so the worker owns an
    /// immutable source snapshot rather than reading mutable editor state.
    #[allow(dead_code)] // Task 6 menu facade.
    pub fn request_open(&mut self, ctx: &egui::Context, name: &str) {
        let source = match self.library.load(name) {
            Ok(source) => source,
            Err(error) => {
                self.record_error(format!("load parser {name}: {error}"));
                return;
            }
        };
        let parser_name = name.to_owned();
        let tx = self.parse_requests_tx.clone();
        let ctx = ctx.clone();
        let thread_name = format!("delog-parser-picker-{name}");
        if let Err(error) = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let path = rfd::FileDialog::new()
                    .set_title(format!("Open file with {parser_name}"))
                    .add_filter("All files", &["*"])
                    .pick_file();
                if let Some(path) = path {
                    let _ = tx.send(ParseRequest {
                        parser_name,
                        source,
                        path,
                    });
                    ctx.request_repaint();
                }
            })
        {
            self.record_error(format!("open parser file picker: {error}"));
        }
    }

    pub fn take_parse_requests(&mut self) -> Vec<ParseRequest> {
        self.parse_requests.try_iter().collect()
    }

    pub fn stage_save(&mut self) -> Result<ParserUiAction, String> {
        if self.pending_save.is_some() {
            return Err("parser validation is already in progress".to_owned());
        }
        let name = self
            .library
            .normalize_name(&self.filename)
            .map_err(|error| error.to_string())?;
        let pending = PendingSave {
            old_name: self.saved_original_name.clone(),
            name,
            source: self.source.clone(),
        };
        let action = ParserUiAction::ValidateAndSave {
            old_name: pending.old_name.clone(),
            name: pending.name.clone(),
            source: pending.source.clone(),
        };
        self.status = format!("validating {}", pending.name);
        self.pending_save = Some(pending);
        Ok(action)
    }

    pub fn mark_validation_dispatched(&mut self, name: &str) {
        if self
            .pending_save
            .as_ref()
            .is_some_and(|pending| pending.name == name)
        {
            self.validation_dispatched = true;
        }
    }

    pub fn validation_dispatch_failed(&mut self, name: &str, detail: &str) {
        if self
            .pending_save
            .as_ref()
            .is_none_or(|pending| pending.name != name)
        {
            return;
        }
        self.pending_save = None;
        self.validation_dispatched = false;
        let concise = format!("Parser validation could not start for {name}.");
        self.status = concise.clone();
        self.error_popup = Some(concise);
        self.diagnostics.push(format!(
            "custom parser validation dispatch {name}: {detail}"
        ));
    }

    pub fn parse_dispatch_failed(&mut self, parser_name: &str, path: &Path, detail: &str) {
        let concise = format!(
            "Parser {parser_name} could not start for {}.",
            file_label(path)
        );
        self.status = concise.clone();
        self.error_popup = Some(concise);
        self.diagnostics.push(format!(
            "custom parser dispatch {parser_name} ({}): {detail}",
            path.display()
        ));
    }

    pub fn mark_parse_dispatched(&mut self, parser_name: &str, path: &Path) {
        let pending = PendingParse {
            parser: parser_name.to_owned(),
            path: path.to_owned(),
        };
        if self.pending_parses.is_empty() {
            self.phase = Some(ParsePhase::Queued {
                parser: pending.parser.clone(),
                path: pending.path.clone(),
            });
        }
        self.pending_parses.push_back(pending);
    }

    pub fn has_pending_work(&self) -> bool {
        self.validation_dispatched || !self.pending_parses.is_empty()
    }

    pub fn handle_event(&mut self, event: ParserEvent) {
        match event {
            ParserEvent::SyntaxValid { name } => {
                if self
                    .pending_save
                    .as_ref()
                    .is_none_or(|pending| pending.name != name)
                {
                    return;
                }
                let pending = self.pending_save.take().expect("matching pending save");
                self.validation_dispatched = false;
                match self
                    .library
                    .save(pending.old_name.as_deref(), &pending.name, &pending.source)
                {
                    Ok(saved_name) => {
                        self.saved_original_name = Some(saved_name.clone());
                        self.status = format!("saved {saved_name}");
                    }
                    Err(error) => {
                        self.record_error(format!("save parser {}: {error}", pending.name))
                    }
                }
            }
            ParserEvent::Reading { parser_name, path } => {
                self.status = format!("reading {}", path.display());
                self.phase = Some(ParsePhase::Reading {
                    parser: parser_name,
                    path,
                });
            }
            ParserEvent::Running { parser_name, path } => {
                self.status = format!("running {parser_name}");
                self.phase = Some(ParsePhase::Running {
                    parser: parser_name,
                    path,
                });
            }
            ParserEvent::Succeeded {
                parser_name, path, ..
            } => {
                self.complete_parse(&parser_name, &path);
                if self.pending_parses.is_empty() {
                    self.status.clear();
                } else {
                    self.status = self.active_label();
                }
            }
            ParserEvent::Failed {
                parser_name,
                path,
                message,
            } => {
                if let Some(path) = &path {
                    self.complete_parse(&parser_name, path);
                } else if self
                    .pending_save
                    .as_ref()
                    .is_some_and(|pending| pending.name == parser_name)
                {
                    self.pending_save = None;
                    self.validation_dispatched = false;
                }
                let concise = match &path {
                    Some(path) => format!(
                        "Parser {parser_name} failed for {}. See Diagnostics for details.",
                        file_label(path)
                    ),
                    None => format!("Syntax error in {parser_name}. See Diagnostics for details."),
                };
                self.status = concise.clone();
                self.error_popup = Some(concise);
                self.diagnostics.push(format!(
                    "custom parser {parser_name}{}: {message}",
                    path.as_ref()
                        .map(|path| format!(" ({})", path.display()))
                        .unwrap_or_default()
                ));
            }
        }
    }

    pub fn is_running(&self) -> bool {
        !self.pending_parses.is_empty()
    }

    #[allow(dead_code)] // Task 6 toolbar facade.
    pub fn active_label(&self) -> String {
        match &self.phase {
            Some(ParsePhase::Queued { parser, path }) => {
                format!("Queued {} with {parser}", file_label(path))
            }
            Some(ParsePhase::Reading { parser, path }) => {
                format!("Reading {} with {parser}", file_label(path))
            }
            Some(ParsePhase::Running { parser, path }) => {
                format!("Running {parser} on {}", file_label(path))
            }
            None => String::new(),
        }
    }

    pub fn take_diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.diagnostics)
    }

    pub fn request_delete(&mut self) {
        self.pending_delete = self.saved_original_name.clone();
    }

    pub fn confirm_delete(&mut self, confirmed: bool) {
        let Some(name) = self.pending_delete.take() else {
            return;
        };
        if !confirmed {
            return;
        }
        match self.library.delete(&name) {
            Ok(()) => {
                self.saved_original_name = None;
                self.status = format!("deleted {name}");
            }
            Err(error) => self.record_error(format!("delete parser {name}: {error}")),
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<ParserUiAction> {
        self.delete_confirm_ui(ctx);
        self.error_ui(ctx);
        if !self.open {
            return Vec::new();
        }

        let mut actions = Vec::new();
        let mut open = self.open;
        egui::Window::new("Custom Python Parser")
            .open(&mut open)
            .collapsible(false)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .default_size([720.0, 480.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filename).desired_width(f32::INFINITY),
                    );
                    if image_text_button(
                        ui,
                        crate::icons::save(),
                        "Save",
                        self.pending_save.is_none(),
                    )
                    .clicked()
                    {
                        match self.stage_save() {
                            Ok(action) => actions.push(action),
                            Err(error) => self.record_error(format!("save parser: {error}")),
                        }
                    }
                    if image_text_button(ui, crate::icons::trash(), "Delete", self.delete_enabled())
                        .clicked()
                    {
                        self.request_delete();
                    }
                });
                ui.label(&self.status);
                CodeEditor::default()
                    .id_source("custom_parser_editor")
                    .with_rows(25)
                    .with_theme(ColorTheme::GITHUB_DARK)
                    .with_numlines(true)
                    .show(ui, &mut self.source, &Syntax::python());
            });
        self.open = open;
        actions
    }

    fn delete_confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(name) = self.pending_delete.clone() else {
            return;
        };
        let mut window_open = true;
        let mut decision = None;
        egui::Window::new("Delete parser?")
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .show(ctx, |ui| {
                ui.label(format!("Delete {name}? This cannot be undone."));
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
        if !window_open {
            decision = decision.or(Some(false));
        }
        if let Some(confirmed) = decision {
            self.confirm_delete(confirmed);
        }
    }

    fn error_ui(&mut self, ctx: &egui::Context) {
        let Some(message) = self.error_popup.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Parser error")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .show(ctx, |ui| {
                ui.label(message);
                if ui.button("OK").clicked() {
                    self.error_popup = None;
                }
            });
        if !open {
            self.error_popup = None;
        }
    }

    fn record_error(&mut self, message: String) {
        self.status = message.clone();
        self.error_popup = Some(message.clone());
        self.diagnostics.push(message);
    }

    fn complete_parse(&mut self, parser_name: &str, path: &Path) {
        let index = self
            .pending_parses
            .iter()
            .position(|pending| pending.parser == parser_name && pending.path == path)
            .unwrap_or(0);
        if !self.pending_parses.is_empty() {
            self.pending_parses.remove(index);
        }
        self.phase = self
            .pending_parses
            .front()
            .map(|pending| ParsePhase::Queued {
                parser: pending.parser.clone(),
                path: pending.path.clone(),
            });
    }

    #[cfg(test)]
    pub fn filename(&self) -> &str {
        &self.filename
    }

    #[cfg(test)]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[cfg(test)]
    pub fn status(&self) -> &str {
        &self.status
    }

    #[cfg(test)]
    pub fn saved_original_name(&self) -> Option<&str> {
        self.saved_original_name.as_deref()
    }

    pub fn delete_enabled(&self) -> bool {
        self.saved_original_name.is_some()
    }

    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    #[cfg(test)]
    pub fn error_popup(&self) -> Option<&str> {
        self.error_popup.as_deref()
    }

    #[cfg(test)]
    pub fn pending_delete_name(&self) -> Option<&str> {
        self.pending_delete.as_deref()
    }

    #[cfg(test)]
    pub fn set_filename(&mut self, filename: impl Into<String>) {
        self.filename = filename.into();
    }

    #[cfg(test)]
    pub fn set_source(&mut self, source: impl Into<String>) {
        self.source = source.into();
    }
}

fn image_text_button(
    ui: &mut egui::Ui,
    icon: egui::ImageSource<'static>,
    text: &str,
    enabled: bool,
) -> egui::Response {
    let image = egui::Image::new(icon)
        .fit_to_exact_size(egui::vec2(16.0, 16.0))
        .tint(ui.visuals().text_color());
    ui.add_enabled(enabled, egui::Button::image_and_text(image, text))
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use delog_script::ParserEvent;

    use super::{ParseRequest, ParserUiAction, ParsersPanel};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "delog-parser-panel-{}-{sequence}",
                std::process::id()
            )))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn add_new_uses_compatible_template_and_disables_delete() {
        let temp = TestDir::new();
        let mut panel = ParsersPanel::new(temp.0.clone());

        panel.add_new();

        assert_eq!(panel.filename(), "new_parser.py");
        assert!(panel.source().contains("def Parse(raw_data):"));
        assert!(panel.source().contains("name, values, tooltip"));
        assert!(!panel.delete_enabled());
        assert!(panel.is_open());
    }

    #[test]
    fn edit_loads_saved_parser() {
        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        fs::write(
            temp.0.join("saved.py"),
            "def Parse(raw_data):\n    return []\n",
        )
        .unwrap();
        let mut panel = ParsersPanel::new(temp.0.clone());

        panel.edit("saved.py");

        assert_eq!(panel.filename(), "saved.py");
        assert_eq!(panel.source(), "def Parse(raw_data):\n    return []\n");
        assert_eq!(panel.saved_original_name(), Some("saved.py"));
        assert!(panel.delete_enabled());
    }

    #[test]
    fn syntax_success_saves_exact_staged_source_and_renames() {
        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        fs::write(temp.0.join("old.py"), "old source").unwrap();
        let mut panel = ParsersPanel::new(temp.0.clone());
        panel.edit("old.py");
        panel.set_filename("renamed.py");
        panel.set_source("staged source");

        assert_eq!(
            panel.stage_save().unwrap(),
            ParserUiAction::ValidateAndSave {
                old_name: Some("old.py".into()),
                name: "renamed.py".into(),
                source: "staged source".into(),
            }
        );
        panel.set_source("newer unsaved edit");
        panel.handle_event(ParserEvent::SyntaxValid {
            name: "renamed.py".into(),
        });

        assert_eq!(
            fs::read_to_string(temp.0.join("renamed.py")).unwrap(),
            "staged source"
        );
        assert!(!temp.0.join("old.py").exists());
        assert_eq!(panel.source(), "newer unsaved edit");
        assert_eq!(panel.saved_original_name(), Some("renamed.py"));
    }

    #[test]
    fn stale_syntax_success_does_not_discard_newer_pending_save() {
        let temp = TestDir::new();
        let mut panel = ParsersPanel::new(temp.0.clone());
        panel.add_new();
        panel.set_filename("second.py");
        panel.set_source("second source");
        panel.stage_save().unwrap();

        panel.handle_event(ParserEvent::SyntaxValid {
            name: "first.py".into(),
        });
        panel.handle_event(ParserEvent::SyntaxValid {
            name: "second.py".into(),
        });

        assert_eq!(
            fs::read_to_string(temp.0.join("second.py")).unwrap(),
            "second source"
        );
        assert!(!temp.0.join("first.py").exists());
    }

    #[test]
    fn syntax_failure_does_not_save_or_start_parser() {
        let temp = TestDir::new();
        let mut panel = ParsersPanel::new(temp.0.clone());
        panel.add_new();
        panel.set_source("def broken(:");
        panel.stage_save().unwrap();

        panel.handle_event(ParserEvent::Failed {
            parser_name: "new_parser.py".into(),
            path: None,
            message: "SyntaxError: invalid syntax at line 1".into(),
        });

        assert!(panel.list().unwrap().is_empty());
        assert!(!panel.is_running());
        assert!(panel.error_popup().unwrap().contains("Syntax error"));
        assert!(
            panel
                .take_diagnostics()
                .join("\n")
                .contains("invalid syntax at line 1")
        );
    }

    #[test]
    fn runtime_failure_stops_and_keeps_concise_and_full_errors() {
        let temp = TestDir::new();
        let mut panel = ParsersPanel::new(temp.0.clone());
        let path = temp.0.join("flight.raw");
        panel.mark_parse_dispatched("raw.py", &path);
        panel.handle_event(ParserEvent::Running {
            parser_name: "raw.py".into(),
            path: path.clone(),
        });

        panel.handle_event(ParserEvent::Failed {
            parser_name: "raw.py".into(),
            path: Some(path.clone()),
            message: "Traceback full detail".into(),
        });

        assert!(!panel.is_running());
        let concise = panel.error_popup().unwrap();
        assert!(concise.contains("raw.py"));
        assert!(concise.contains("flight.raw"));
        assert!(!concise.contains("Traceback full detail"));
        assert!(
            panel
                .take_diagnostics()
                .join("\n")
                .contains("Traceback full detail")
        );
    }

    #[test]
    fn parser_lifecycle_reports_reading_running_and_success() {
        let temp = TestDir::new();
        let mut panel = ParsersPanel::new(temp.0.clone());
        let path = PathBuf::from("flight.raw");
        panel.mark_parse_dispatched("raw.py", &path);

        panel.handle_event(ParserEvent::Reading {
            parser_name: "raw.py".into(),
            path: path.clone(),
        });
        assert!(panel.is_running());
        assert!(panel.active_label().contains("Reading"));
        panel.handle_event(ParserEvent::Running {
            parser_name: "raw.py".into(),
            path: path.clone(),
        });
        assert!(panel.active_label().contains("Running"));
        panel.handle_event(ParserEvent::Succeeded {
            parser_name: "raw.py".into(),
            path,
            topics: 2,
            rows: 8,
        });
        assert!(!panel.is_running());
        assert_eq!(panel.active_label(), "");
        assert_eq!(panel.status(), "");
    }

    #[test]
    fn delete_requires_confirmation_and_removes_saved_parser() {
        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        fs::write(temp.0.join("saved.py"), "source").unwrap();
        let mut panel = ParsersPanel::new(temp.0.clone());
        panel.edit("saved.py");

        panel.request_delete();
        assert_eq!(panel.pending_delete_name(), Some("saved.py"));
        panel.confirm_delete(false);
        assert!(temp.0.join("saved.py").exists());
        panel.request_delete();
        panel.confirm_delete(true);

        assert!(!temp.0.join("saved.py").exists());
        assert_eq!(panel.pending_delete_name(), None);
        assert!(!panel.delete_enabled());
    }

    #[test]
    fn parse_request_preserves_parser_source_and_path() {
        let request = ParseRequest {
            parser_name: "raw.py".into(),
            source: "def Parse(raw_data): return []".into(),
            path: PathBuf::from("flight.raw"),
        };

        assert_eq!(request.parser_name, "raw.py");
        assert!(request.source.contains("Parse"));
        assert_eq!(request.path, PathBuf::from("flight.raw"));
    }

    #[test]
    fn dispatched_validation_is_pending_until_matching_result() {
        let temp = TestDir::new();
        let mut panel = ParsersPanel::new(temp.0.clone());
        panel.add_new();
        panel.stage_save().unwrap();

        panel.mark_validation_dispatched("new_parser.py");

        assert!(panel.has_pending_work());
        panel.handle_event(ParserEvent::SyntaxValid {
            name: "new_parser.py".into(),
        });
        assert!(!panel.has_pending_work());
    }

    #[test]
    fn dispatched_parse_is_pending_before_reading_and_clears_on_success() {
        let temp = TestDir::new();
        let mut panel = ParsersPanel::new(temp.0.clone());
        let path = PathBuf::from("flight.raw");

        panel.mark_parse_dispatched("raw.py", &path);

        assert!(panel.has_pending_work());
        assert!(panel.is_running());
        assert!(panel.active_label().contains("Queued"));
        panel.handle_event(ParserEvent::Succeeded {
            parser_name: "raw.py".into(),
            path,
            topics: 1,
            rows: 3,
        });
        assert!(!panel.has_pending_work());
        assert!(!panel.is_running());
    }

    #[test]
    fn first_terminal_event_keeps_second_dispatched_parse_pending() {
        let temp = TestDir::new();
        let mut panel = ParsersPanel::new(temp.0.clone());
        let first = PathBuf::from("first.raw");
        let second = PathBuf::from("second.raw");
        panel.mark_parse_dispatched("raw.py", &first);
        panel.mark_parse_dispatched("raw.py", &second);

        panel.handle_event(ParserEvent::Succeeded {
            parser_name: "raw.py".into(),
            path: first,
            topics: 1,
            rows: 3,
        });

        assert!(panel.has_pending_work());
        assert!(panel.is_running());
        panel.handle_event(ParserEvent::Running {
            parser_name: "raw.py".into(),
            path: second.clone(),
        });
        panel.handle_event(ParserEvent::Failed {
            parser_name: "raw.py".into(),
            path: Some(second),
            message: "second failed".into(),
        });
        assert!(!panel.has_pending_work());
        assert!(!panel.is_running());
    }

    #[test]
    fn pending_validation_blocks_reopen_and_saves_original_stage_only() {
        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        fs::write(temp.0.join("saved.py"), "original").unwrap();
        let mut panel = ParsersPanel::new(temp.0.clone());
        panel.edit("saved.py");
        panel.set_source("first staged source");
        panel.stage_save().unwrap();
        panel.mark_validation_dispatched("saved.py");
        fs::write(temp.0.join("saved.py"), "newer invalid source").unwrap();

        panel.edit("saved.py");
        panel.add_new();
        assert_eq!(panel.source(), "first staged source");
        assert!(panel.stage_save().is_err());
        panel.handle_event(ParserEvent::SyntaxValid {
            name: "saved.py".into(),
        });

        assert_eq!(
            fs::read_to_string(temp.0.join("saved.py")).unwrap(),
            "first staged source"
        );
        assert!(!panel.has_pending_work());
    }

    #[test]
    fn list_propagates_non_directory_errors() {
        let temp = TestDir::new();
        fs::write(&temp.0, "not a directory").unwrap();
        let mut panel = ParsersPanel::new(temp.0.clone());

        assert!(panel.list().is_err());
    }

    #[test]
    fn failed_validation_dispatch_clears_pending_and_records_diagnostic() {
        let temp = TestDir::new();
        let mut panel = ParsersPanel::new(temp.0.clone());
        panel.add_new();
        panel.stage_save().unwrap();

        panel.validation_dispatch_failed("new_parser.py", "worker disconnected");

        assert!(!panel.has_pending_work());
        assert!(panel.status().contains("could not start"));
        assert!(
            panel
                .take_diagnostics()
                .join("\n")
                .contains("worker disconnected")
        );
    }
}
