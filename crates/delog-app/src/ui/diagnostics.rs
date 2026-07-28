use delog_core::diagnostics::{DiagRecord, Severity};
use delog_core::identity::SourceId;
use delog_core::snapshot::StoreSnapshot;
use egui_extras::{Column, TableBuilder};

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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let trash = egui::Image::new(crate::ui::icons::trash())
                    .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
                    .tint(ui.visuals().text_color());
                if ui
                    .add(egui::Button::image(trash))
                    .on_hover_text("Clear diagnostics")
                    .clicked()
                {
                    clear = true;
                }
            });
        });

        let filtered = filtered_records(
            records,
            snapshot,
            self.min_severity,
            &self.origin,
            &self.search,
        );
        ui.add_space(4.0);
        action.clear = clear;
        if filtered.is_empty() {
            ui.weak("No diagnostics match the current filters.");
            return action;
        }
        let row_height = egui::TextStyle::Body
            .resolve(ui.style())
            .size
            .max(ui.spacing().interact_size.y);
        TableBuilder::new(ui)
            .id_salt("diagnostics-table")
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false])
            .column(Column::auto().at_least(72.0))
            .column(Column::auto().at_least(48.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::auto().at_least(64.0))
            .column(Column::auto().at_least(72.0))
            .column(Column::auto().at_least(56.0))
            .column(Column::remainder().clip(true))
            .header(row_height, |mut header| {
                header.col(|ui| {
                    ui.strong("Severity");
                });
                header.col(|ui| {
                    ui.strong("Count");
                });
                header.col(|ui| {
                    ui.strong("Origin");
                });
                header.col(|ui| {
                    ui.strong("Code");
                });
                header.col(|ui| {
                    ui.strong("Time");
                });
                header.col(|ui| {
                    ui.strong("Byte");
                });
                header.col(|ui| {
                    ui.strong("Message");
                });
            })
            .body(|body| {
                body.rows(row_height, filtered.len(), |mut row| {
                    let entry = &filtered[row.index()];
                    row.col(|ui| {
                        let color = severity_color(ui, entry.record.diag.severity);
                        ui.colored_label(color, severity_label(entry.record.diag.severity));
                    });
                    row.col(|ui| {
                        ui.label(entry.record.count.to_string());
                    });
                    row.col(|ui| {
                        ui.label(entry.origin.as_str());
                    });
                    row.col(|ui| {
                        ui.monospace(entry.record.diag.code);
                    });
                    row.col(|ui| {
                        if let Some(time_us) = entry.record.diag.time_us {
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
                    });
                    row.col(|ui| {
                        ui.label(
                            entry
                                .record
                                .diag
                                .byte_offset
                                .map(|b| b.to_string())
                                .unwrap_or_else(|| "-".into()),
                        );
                    });
                    row.col(|ui| {
                        ui.label(entry.record.diag.message.as_str());
                    });
                });
            });
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
