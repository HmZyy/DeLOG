#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct PendingLog {
    pub level: LogLevel,
    pub target: String,
    pub message: String,
}

#[track_caller]
pub fn log(level: LogLevel, message: impl Into<String>) -> PendingLog {
    PendingLog::new(level, message)
}

impl PendingLog {
    #[track_caller]
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        let location = std::panic::Location::caller();
        Self::with_target(level, caller_target(location.file()), message)
    }

    pub fn with_target(
        level: LogLevel,
        target: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            level,
            target: target.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogRecord {
    pub seq: u64,
    pub elapsed_ms: u128,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct LoggingDock {
    pub open: bool,
    min_level: LogLevel,
    target: String,
    search: String,
}

impl Default for LoggingDock {
    fn default() -> Self {
        Self {
            open: false,
            min_level: LogLevel::Debug,
            target: String::new(),
            search: String::new(),
        }
    }
}

impl LoggingDock {
    pub fn ui(&mut self, ui: &mut egui::Ui, records: &[LogRecord]) -> LoggingAction {
        let mut action = LoggingAction::default();
        let targets = targets(records);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("logging-level")
                .selected_text(level_filter_label(self.min_level))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.min_level, LogLevel::Debug, "Debug+");
                    ui.selectable_value(&mut self.min_level, LogLevel::Info, "Info+");
                    ui.selectable_value(&mut self.min_level, LogLevel::Warning, "Warnings+");
                    ui.selectable_value(&mut self.min_level, LogLevel::Error, "Errors");
                });

            egui::ComboBox::from_id_salt("logging-target")
                .width(180.0)
                .selected_text(if self.target.is_empty() {
                    "All targets"
                } else {
                    self.target.as_str()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.target, String::new(), "All targets");
                    for target in &targets {
                        ui.selectable_value(&mut self.target, target.clone(), target);
                    }
                });

            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search")
                    .desired_width(220.0),
            );
            ui.add_space(ui.available_width());

            let trash = egui::Image::new(crate::icons::trash())
                .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
                .tint(ui.visuals().text_color());
            if ui
                .add(egui::Button::image(trash))
                .on_hover_text("Clear logs")
                .clicked()
            {
                action.clear = true;
            }
        });

        let filtered = filtered_records(records, self.min_level, &self.target, &self.search);
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if filtered.is_empty() {
                    ui.weak("No logs match the current filters.");
                    return;
                }
                egui::Grid::new("logging-grid")
                    .num_columns(5)
                    .striped(true)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong("Level");
                        ui.strong("Seq");
                        ui.strong("Time");
                        ui.strong("Target");
                        ui.strong("Message");
                        ui.end_row();

                        for record in filtered {
                            ui.colored_label(
                                level_color(ui, record.level),
                                level_label(record.level),
                            );
                            ui.label(record.seq.to_string());
                            ui.label(format_elapsed(record.elapsed_ms));
                            ui.monospace(record.target.as_str());
                            ui.label(record.message.as_str());
                            ui.end_row();
                        }
                    });
            });
        action
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoggingAction {
    pub clear: bool,
}

fn filtered_records<'a>(
    records: &'a [LogRecord],
    min_level: LogLevel,
    target_filter: &str,
    search: &str,
) -> Vec<&'a LogRecord> {
    let needle = search.trim().to_lowercase();
    records
        .iter()
        .filter(|record| record.level >= min_level)
        .filter(|record| target_filter.is_empty() || record.target == target_filter)
        .filter(|record| {
            needle.is_empty()
                || record.message.to_lowercase().contains(&needle)
                || record.target.to_lowercase().contains(&needle)
                || level_label(record.level).to_lowercase().contains(&needle)
                || record.seq.to_string().contains(&needle)
                || format_elapsed(record.elapsed_ms).contains(&needle)
        })
        .collect()
}

fn targets(records: &[LogRecord]) -> Vec<String> {
    let mut out = records
        .iter()
        .map(|record| record.target.clone())
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

fn caller_target(file: &str) -> String {
    let normalized = file.replace('\\', "/");
    let path = normalized
        .strip_prefix("crates/delog-app/src/")
        .or_else(|| normalized.strip_prefix("src/"))
        .unwrap_or(normalized.as_str());
    path.trim_end_matches(".rs").replace('/', "::")
}

fn format_elapsed(elapsed_ms: u128) -> String {
    let total_seconds = elapsed_ms / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds / 60) % 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn level_label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => "INFO",
        LogLevel::Warning => "WARNING",
        LogLevel::Error => "ERROR",
    }
}

fn level_filter_label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Debug => "Debug+",
        LogLevel::Info => "Info+",
        LogLevel::Warning => "Warnings+",
        LogLevel::Error => "Errors",
    }
}

fn level_color(ui: &egui::Ui, level: LogLevel) -> egui::Color32 {
    match level {
        LogLevel::Debug => egui::Color32::from_rgb(137, 180, 250),
        LogLevel::Info => ui.visuals().text_color(),
        LogLevel::Warning => egui::Color32::from_rgb(245, 194, 97),
        LogLevel::Error => egui::Color32::from_rgb(243, 139, 168),
    }
}

#[cfg(test)]
mod tests {
    use super::{LogLevel, LogRecord, caller_target, filtered_records};

    #[test]
    fn filters_by_level_target_and_search() {
        let records = vec![
            LogRecord {
                seq: 0,
                elapsed_ms: 10,
                level: LogLevel::Debug,
                target: "vehicle-profile".into(),
                message: "loaded".into(),
            },
            LogRecord {
                seq: 1,
                elapsed_ms: 20,
                level: LogLevel::Error,
                target: "vehicle-profile".into(),
                message: "save failed".into(),
            },
        ];

        let filtered = filtered_records(&records, LogLevel::Warning, "vehicle-profile", "save");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].seq, 1);
    }

    #[test]
    fn caller_target_uses_source_relative_module_path() {
        assert_eq!(
            caller_target("crates/delog-app/src/vehicle_dialog.rs"),
            "vehicle_dialog"
        );
        assert_eq!(caller_target("crates/delog-app/src/foo/bar.rs"), "foo::bar");
    }

    #[test]
    fn elapsed_time_formats_as_hh_mm_ss() {
        assert_eq!(super::format_elapsed(0), "00:00:00");
        assert_eq!(super::format_elapsed(3_723_000), "01:02:03");
    }
}
