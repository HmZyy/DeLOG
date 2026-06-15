//! Performance metrics dock UI (PLAN.md §16, PRF-02).

use delog_core::metrics::MetricStats;

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

#[derive(Debug, Default)]
pub struct PerformanceDock {
    pub open: bool,
}

impl PerformanceDock {
    pub fn ui(&mut self, ui: &mut egui::Ui, snapshot: &PerformanceSnapshot) {
        ui.horizontal(|ui| {
            ui.strong("Performance");
            ui.weak(format!("{} metric(s)", snapshot.metrics.len()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.open = false;
                }
            });
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.heading("Resources");
                egui::Grid::new("performance-resources-grid")
                    .num_columns(2)
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        summary_row(
                            ui,
                            "GPU buffers",
                            snapshot.resources.gpu_buffer_count.to_string(),
                        );
                        summary_row(ui, "GPU bytes", format_bytes(snapshot.resources.gpu_bytes));
                        summary_row(
                            ui,
                            "Ready CPU caches",
                            snapshot.resources.cache_ready_count.to_string(),
                        );
                        summary_row(
                            ui,
                            "CPU cache bytes",
                            format_bytes(snapshot.resources.cache_cpu_bytes),
                        );
                    });

                ui.separator();
                ui.heading("Traces");
                if snapshot.traces.is_empty() {
                    ui.weak("No plotted traces.");
                } else {
                    egui::Grid::new("performance-traces-grid")
                        .num_columns(5)
                        .striped(true)
                        .spacing([14.0, 4.0])
                        .show(ui, |ui| {
                            ui.strong("Trace");
                            ui.strong("Samples");
                            ui.strong("Visible");
                            ui.strong("CPU cache");
                            ui.strong("GPU");
                            ui.end_row();

                            for trace in &snapshot.traces {
                                ui.label(trace.label.as_str());
                                ui.label(format_optional_usize(trace.samples));
                                ui.label(format_optional_usize(trace.visible_samples));
                                ui.label(format_bytes(trace.cache_cpu_bytes));
                                ui.label(format_bytes(trace.gpu_bytes));
                                ui.end_row();
                            }
                        });
                }

                ui.separator();
                ui.heading("Metrics");
                if snapshot.metrics.is_empty() {
                    ui.weak("No metrics recorded yet.");
                    return;
                }

                egui::Grid::new("performance-metrics-grid")
                    .num_columns(8)
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong("Metric");
                        ui.strong("Last");
                        ui.strong("Avg");
                        ui.strong("Min");
                        ui.strong("Max");
                        ui.strong("P99");
                        ui.strong("Samples");
                        ui.strong("Counter");
                        ui.end_row();

                        for (name, stats) in &snapshot.metrics {
                            ui.monospace(*name);
                            ui.label(format_value(stats.last));
                            ui.label(format_value(stats.avg));
                            ui.label(format_value(stats.min));
                            ui.label(format_value(stats.max));
                            ui.label(format_value(stats.p99));
                            ui.label(stats.n.to_string());
                            ui.label(stats.counter.to_string());
                            ui.end_row();
                        }
                    });
            });
    }
}

fn summary_row(ui: &mut egui::Ui, key: &str, value: String) {
    ui.strong(key);
    ui.label(value);
    ui.end_row();
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
