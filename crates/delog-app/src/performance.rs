use delog_core::metrics::MetricStats;
use egui_extras::{Column, TableBuilder};

#[derive(Debug, Clone, Default)]
pub struct PerformanceSnapshot {
    pub metrics: Vec<(&'static str, MetricStats)>,
    pub resources: ResourceSummary,
    pub traces: Vec<TraceSummary>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceSummary {
    pub gpu_buffer_count: usize,
    pub gpu_bytes: u64,
    pub cache_ready_count: usize,
    pub cache_cpu_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceSummary {
    pub label: String,
    pub samples: Option<usize>,
    pub visible_samples: Option<usize>,
    pub cache_cpu_bytes: u64,
    pub gpu_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PerfTab {
    #[default]
    Resources,
    Traces,
    Metrics,
}

#[derive(Debug, Default)]
pub struct PerformanceDock {
    pub open: bool,
    tab: PerfTab,
}

impl PerformanceDock {
    pub fn ui(&mut self, ui: &mut egui::Ui, snapshot: &PerformanceSnapshot) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, PerfTab::Resources, "Resources");
            ui.selectable_value(&mut self.tab, PerfTab::Traces, "Traces");
            ui.selectable_value(&mut self.tab, PerfTab::Metrics, "Metrics");
        });
        ui.add_space(4.0);

        let row_height =
            egui::TextStyle::Body.resolve(ui.style()).size.max(ui.spacing().interact_size.y);
        match self.tab {
            PerfTab::Resources => {
                let rows = [
                    (
                        "GPU buffers",
                        snapshot.resources.gpu_buffer_count.to_string(),
                    ),
                    ("GPU bytes", format_bytes(snapshot.resources.gpu_bytes)),
                    (
                        "Ready CPU caches",
                        snapshot.resources.cache_ready_count.to_string(),
                    ),
                    (
                        "CPU cache bytes",
                        format_bytes(snapshot.resources.cache_cpu_bytes),
                    ),
                ];
                TableBuilder::new(ui)
                    .id_salt("performance-resources-table")
                    .striped(true)
                    .resizable(true)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .auto_shrink([false, false])
                    .column(Column::auto().at_least(140.0))
                    .column(Column::remainder().clip(true))
                    .body(|mut body| {
                        for (key, value) in rows {
                            body.row(row_height, |mut row| {
                                row.col(|ui| {
                                    ui.strong(key);
                                });
                                row.col(|ui| {
                                    ui.label(value);
                                });
                            });
                        }
                    });
            }
            PerfTab::Traces => {
                if snapshot.traces.is_empty() {
                    ui.weak("No plotted traces.");
                } else {
                    TableBuilder::new(ui)
                        .id_salt("performance-traces-table")
                        .striped(true)
                        .resizable(true)
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                        .auto_shrink([false, false])
                        .column(Column::remainder().clip(true))
                        .column(Column::auto().at_least(72.0))
                        .column(Column::auto().at_least(64.0))
                        .column(Column::auto().at_least(80.0))
                        .column(Column::auto().at_least(72.0))
                        .header(row_height, |mut header| {
                            header.col(|ui| {
                                ui.strong("Trace");
                            });
                            header.col(|ui| {
                                ui.strong("Samples");
                            });
                            header.col(|ui| {
                                ui.strong("Visible");
                            });
                            header.col(|ui| {
                                ui.strong("CPU cache");
                            });
                            header.col(|ui| {
                                ui.strong("GPU");
                            });
                        })
                        .body(|body| {
                            body.rows(row_height, snapshot.traces.len(), |mut row| {
                                let trace = &snapshot.traces[row.index()];
                                row.col(|ui| {
                                    ui.label(trace.label.as_str());
                                });
                                row.col(|ui| {
                                    ui.label(format_optional_usize(trace.samples));
                                });
                                row.col(|ui| {
                                    ui.label(format_optional_usize(trace.visible_samples));
                                });
                                row.col(|ui| {
                                    ui.label(format_bytes(trace.cache_cpu_bytes));
                                });
                                row.col(|ui| {
                                    ui.label(format_bytes(trace.gpu_bytes));
                                });
                            });
                        });
                }
            }
            PerfTab::Metrics => {
                if snapshot.metrics.is_empty() {
                    ui.weak("No metrics recorded yet.");
                } else {
                    TableBuilder::new(ui)
                        .id_salt("performance-metrics-table")
                        .striped(true)
                        .resizable(true)
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                        .auto_shrink([false, false])
                        .column(Column::auto().at_least(96.0))
                        .columns(Column::auto().at_least(56.0), 6)
                        .column(Column::remainder().clip(true))
                        .header(row_height, |mut header| {
                            header.col(|ui| {
                                ui.strong("Metric");
                            });
                            header.col(|ui| {
                                ui.strong("Last");
                            });
                            header.col(|ui| {
                                ui.strong("Avg");
                            });
                            header.col(|ui| {
                                ui.strong("Min");
                            });
                            header.col(|ui| {
                                ui.strong("Max");
                            });
                            header.col(|ui| {
                                ui.strong("P99");
                            });
                            header.col(|ui| {
                                ui.strong("Samples");
                            });
                            header.col(|ui| {
                                ui.strong("Counter");
                            });
                        })
                        .body(|body| {
                            body.rows(row_height, snapshot.metrics.len(), |mut row| {
                                let (name, stats) = &snapshot.metrics[row.index()];
                                row.col(|ui| {
                                    ui.monospace(*name);
                                });
                                row.col(|ui| {
                                    ui.label(format_value(stats.last));
                                });
                                row.col(|ui| {
                                    ui.label(format_value(stats.avg));
                                });
                                row.col(|ui| {
                                    ui.label(format_value(stats.min));
                                });
                                row.col(|ui| {
                                    ui.label(format_value(stats.max));
                                });
                                row.col(|ui| {
                                    ui.label(format_value(stats.p99));
                                });
                                row.col(|ui| {
                                    ui.label(stats.n.to_string());
                                });
                                row.col(|ui| {
                                    ui.label(stats.counter.to_string());
                                });
                            });
                        });
                }
            }
        }
    }
}

fn format_optional_usize(value: Option<usize>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".into())
}

fn format_value(value: f32) -> String {
    if value.abs() >= 100.0 {
        format!("{value:.0}")
    } else if value.abs() >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.3}")
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.2} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_values_by_magnitude() {
        assert_eq!(format_value(123.4), "123");
        assert_eq!(format_value(12.34), "12.3");
        assert_eq!(format_value(1.234), "1.234");
    }

    #[test]
    fn formats_bytes_by_magnitude() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.00 MiB");
    }
}
