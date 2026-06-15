//! Performance metrics dock UI (PLAN.md §16, PRF-02).

use delog_core::metrics::MetricStats;

#[derive(Debug, Default)]
pub struct PerformanceDock {
    pub open: bool,
}

impl PerformanceDock {
    pub fn ui(&mut self, ui: &mut egui::Ui, metrics: &[(&'static str, MetricStats)]) {
        ui.horizontal(|ui| {
            ui.strong("Performance");
            ui.weak(format!("{} metric(s)", metrics.len()));
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
                if metrics.is_empty() {
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

                        for (name, stats) in metrics {
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

fn format_value(value: f32) -> String {
    if value.abs() >= 100.0 {
        format!("{value:.0}")
    } else if value.abs() >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.3}")
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
}
