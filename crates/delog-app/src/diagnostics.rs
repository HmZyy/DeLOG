//! Diagnostics dock UI (PLAN.md §15, DIA-02).

use delog_core::diagnostics::{DiagRecord, Severity};
use delog_core::identity::SourceId;
use delog_core::snapshot::StoreSnapshot;

#[derive(Debug, Clone)]
pub struct DiagnosticsDock {
    pub open: bool,
    min_severity: Severity,
    origin: String,
    search: String,
}

impl Default for DiagnosticsDock {
    fn default() -> Self {
        Self {
            open: false,
            min_severity: Severity::Info,
            origin: String::new(),
            search: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct DisplayRecord<'a> {
    record: &'a DiagRecord,
    origin: String,
}

impl DiagnosticsDock {
    /// Render the dock and return user actions requested from diagnostic rows.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        records: &[DiagRecord],
        snapshot: &StoreSnapshot,
    ) -> DiagnosticsAction {
        let mut action = DiagnosticsAction::default();
        let mut clear = false;
        let origins = origins(records, snapshot);
        ui.horizontal(|ui| {
            ui.strong("Diagnostics");
            ui.weak(format!("{} retained", records.len()));
            if let Some(last) = records.last() {
                ui.separator();
                ui.label(format!("[{}] {}", last.diag.code, last.diag.message));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.open = false;
                }
                if ui.button("Clear").clicked() {
                    clear = true;
                }
            });
        });

        ui.separator();
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("diagnostics-severity")
                .selected_text(severity_filter_label(self.min_severity))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.min_severity, Severity::Info, "Info+");
                    ui.selectable_value(&mut self.min_severity, Severity::Warning, "Warnings+");
                    ui.selectable_value(&mut self.min_severity, Severity::Error, "Errors");
                });

            egui::ComboBox::from_id_salt("diagnostics-origin")
                .width(180.0)
                .selected_text(if self.origin.is_empty() {
                    "All origins"
                } else {
                    self.origin.as_str()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.origin, String::new(), "All origins");
                    for origin in &origins {
                        ui.selectable_value(&mut self.origin, origin.clone(), origin);
                    }
                });

            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search")
                    .desired_width(220.0),
            );
        });

        let filtered = filtered_records(
            records,
            snapshot,
            self.min_severity,
            &self.origin,
            &self.search,
        );
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if filtered.is_empty() {
                    ui.weak("No diagnostics match the current filters.");
                    return;
                }
                egui::Grid::new("diagnostics-grid")
                    .num_columns(7)
                    .striped(true)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong("Severity");
                        ui.strong("Count");
                        ui.strong("Origin");
                        ui.strong("Code");
                        ui.strong("Time");
                        ui.strong("Byte");
                        ui.strong("Message");
                        ui.end_row();

                        for row in filtered {
                            let color = severity_color(ui, row.record.diag.severity);
                            ui.colored_label(color, severity_label(row.record.diag.severity));
                            ui.label(row.record.count.to_string());
                            ui.label(row.origin);
                            ui.monospace(row.record.diag.code);
                            if let Some(time_us) = row.record.diag.time_us {
                                if ui
                                    .button(format_time(Some(time_us)))
                                    .on_hover_text("Jump playhead to this diagnostic")
                                    .clicked()
                                {
                                    action.jump_to_time_us = Some(time_us);
                                }
                            } else {
                                ui.label("-");
                            }
                            ui.label(
                                row.record
                                    .diag
                                    .byte_offset
                                    .map(|b| b.to_string())
                                    .unwrap_or_else(|| "-".into()),
                            );
                            ui.label(row.record.diag.message.as_str());
                            ui.end_row();
                        }
                    });
            });
        action.clear = clear;
        action
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagnosticsAction {
    pub clear: bool,
    pub jump_to_time_us: Option<i64>,
}

fn filtered_records<'a>(
    records: &'a [DiagRecord],
    snapshot: &StoreSnapshot,
    min_severity: Severity,
    origin_filter: &str,
    search: &str,
) -> Vec<DisplayRecord<'a>> {
    let needle = search.trim().to_lowercase();
    records
        .iter()
        .filter_map(|record| {
            if record.diag.severity < min_severity {
                return None;
            }
            let source = source_label(snapshot, record.diag.source);
            let origin = origin_label(&source, record.diag.code);
            if !origin_filter.is_empty() && origin != origin_filter {
                return None;
            }
            if !needle.is_empty() && !matches_search(record, &source, &origin, needle.as_str()) {
                return None;
            }
            Some(DisplayRecord { record, origin })
        })
        .collect()
}

fn origins(records: &[DiagRecord], snapshot: &StoreSnapshot) -> Vec<String> {
    let mut out = records
        .iter()
        .map(|record| {
            let source = source_label(snapshot, record.diag.source);
            origin_label(&source, record.diag.code)
        })
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

fn source_label(snapshot: &StoreSnapshot, source: Option<SourceId>) -> String {
    source
        .and_then(|id| snapshot.source(id))
        .map(|source| source.entry.label.clone())
        .unwrap_or_else(|| "-".into())
}

fn origin_label(source: &str, code: &str) -> String {
    if source == "-" {
        code.to_owned()
    } else {
        source.to_owned()
    }
}

fn matches_search(record: &DiagRecord, source: &str, origin: &str, needle: &str) -> bool {
    let diag = &record.diag;
    diag.message.to_lowercase().contains(needle)
        || diag.code.to_lowercase().contains(needle)
        || source.to_lowercase().contains(needle)
        || origin.to_lowercase().contains(needle)
        || diag
            .time_us
            .is_some_and(|time| time.to_string().contains(needle))
        || diag
            .byte_offset
            .is_some_and(|byte| byte.to_string().contains(needle))
}

fn format_time(time_us: Option<i64>) -> String {
    time_us
        .map(|us| format!("{:.3}s", us as f64 / 1_000_000.0))
        .unwrap_or_else(|| "-".into())
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "Info",
        Severity::Warning => "Warning",
        Severity::Error => "Error",
    }
}

fn severity_filter_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "Info+",
        Severity::Warning => "Warnings+",
        Severity::Error => "Errors",
    }
}

fn severity_color(ui: &egui::Ui, severity: Severity) -> egui::Color32 {
    match severity {
        Severity::Info => ui.visuals().text_color(),
        Severity::Warning => egui::Color32::from_rgb(245, 194, 97),
        Severity::Error => egui::Color32::from_rgb(243, 139, 168),
    }
}

#[cfg(test)]
mod tests {
    use delog_core::diagnostics::Diag;
    use delog_core::snapshot::StoreSnapshot;

    use super::*;

    #[test]
    fn filters_by_severity_origin_and_search() {
        let snapshot = StoreSnapshot::empty();
        let records = vec![
            DiagRecord {
                seq: 0,
                diag: Diag::info("layout-bind", "bound traces"),
                count: 1,
            },
            DiagRecord {
                seq: 1,
                diag: Diag::error("gpu", "validation failed"),
                count: 2,
            },
        ];

        let filtered = filtered_records(&records, &snapshot, Severity::Warning, "", "validation");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].record.diag.code, "gpu");

        let filtered = filtered_records(&records, &snapshot, Severity::Info, "layout-bind", "");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].record.diag.code, "layout-bind");
    }
}
