//! Performance metrics dock UI.

use delog_core::metrics::MetricStats;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Scope timers and millisecond gauges.
    Time,
    /// Byte gauges (names ending in `_bytes`).
    Bytes,
    /// Monotonic counters recorded via `add()` (no ring samples).
    Count,
}

impl Unit {
    /// Order metrics are grouped in (Time, then Bytes, then Count).
    const ORDER: [Unit; 3] = [Unit::Time, Unit::Bytes, Unit::Count];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SortKey {
    #[default]
    Name,
    Last,
    Avg,
    Min,
    Max,
    P99,
    Samples,
    Counter,
}

/// Classify a metric by unit family. `Count` wins for counter-only metrics
/// (no samples); byte gauges go by the `_bytes` name suffix; everything else
/// is time.
fn metric_unit(name: &str, stats: &MetricStats) -> Unit {
    if stats.n == 0 && stats.counter > 0 {
        Unit::Count
    } else if name.ends_with("_bytes") {
        Unit::Bytes
    } else {
        Unit::Time
    }
}

/// Group metrics by [`Unit`] in [`Unit::ORDER`], sorting the rows within each
/// group by `sort`/`ascending`. Empty groups are omitted.
fn organize(
    metrics: &[(&'static str, MetricStats)],
    sort: SortKey,
    ascending: bool,
) -> Vec<(Unit, Vec<(&'static str, MetricStats)>)> {
    let mut groups = Vec::new();
    for unit in Unit::ORDER {
        let mut rows: Vec<(&'static str, MetricStats)> = metrics
            .iter()
            .filter(|(name, stats)| metric_unit(name, stats) == unit)
            .copied()
            .collect();
        if rows.is_empty() {
            continue;
        }
        rows.sort_by(|a, b| {
            let ord = match sort {
                SortKey::Name => a.0.cmp(b.0),
                SortKey::Last => a.1.last.total_cmp(&b.1.last),
                SortKey::Avg => a.1.avg.total_cmp(&b.1.avg),
                SortKey::Min => a.1.min.total_cmp(&b.1.min),
                SortKey::Max => a.1.max.total_cmp(&b.1.max),
                SortKey::P99 => a.1.p99.total_cmp(&b.1.p99),
                SortKey::Samples => a.1.n.cmp(&b.1.n),
                SortKey::Counter => a.1.counter.cmp(&b.1.counter),
            };
            // Stable tiebreak by name so equal values keep a deterministic order.
            let ord = ord.then_with(|| a.0.cmp(b.0));
            if ascending { ord } else { ord.reverse() }
        });
        groups.push((unit, rows));
    }
    groups
}

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

/// Which section of the performance dock is shown. Held on the dock so the
/// selection persists across frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PerfTab {
    #[default]
    Resources,
    Traces,
    Metrics,
}

#[derive(Debug)]
pub struct PerformanceDock {
    pub open: bool,
    tab: PerfTab,
    sort_key: SortKey,
    sort_ascending: bool,
}

impl Default for PerformanceDock {
    fn default() -> Self {
        Self {
            open: false,
            tab: PerfTab::default(),
            sort_key: SortKey::Name,
            sort_ascending: true,
        }
    }
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

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, PerfTab::Resources, "Resources");
            ui.selectable_value(&mut self.tab, PerfTab::Traces, "Traces");
            ui.selectable_value(&mut self.tab, PerfTab::Metrics, "Metrics");
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.tab {
                PerfTab::Resources => {
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
                            summary_row(
                                ui,
                                "GPU bytes",
                                format_bytes(snapshot.resources.gpu_bytes),
                            );
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
                }
                PerfTab::Traces => {
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
                }
                PerfTab::Metrics => {
                    if snapshot.metrics.is_empty() {
                        ui.weak("No metrics recorded yet.");
                        return;
                    }

                    let groups = organize(&snapshot.metrics, self.sort_key, self.sort_ascending);
                    egui::Grid::new("performance-metrics-grid")
                        .num_columns(8)
                        .striped(true)
                        .spacing([14.0, 4.0])
                        .show(ui, |ui| {
                            for (label, key) in HEADERS {
                                sort_header(
                                    ui,
                                    label,
                                    key,
                                    &mut self.sort_key,
                                    &mut self.sort_ascending,
                                );
                            }
                            ui.end_row();

                            for (unit, rows) in &groups {
                                for (name, stats) in rows {
                                    metric_row(ui, name, stats, *unit);
                                }
                            }
                        });
                }
            });
    }
}

fn summary_row(ui: &mut egui::Ui, key: &str, value: String) {
    ui.strong(key);
    ui.label(value);
    ui.end_row();
}

/// The Metrics table columns and the sort key each maps to, left to right.
const HEADERS: [(&str, SortKey); 8] = [
    ("Metric", SortKey::Name),
    ("Last", SortKey::Last),
    ("Avg", SortKey::Avg),
    ("Min", SortKey::Min),
    ("Max", SortKey::Max),
    ("P99", SortKey::P99),
    ("Samples", SortKey::Samples),
    ("Counter", SortKey::Counter),
];

const NA: &str = "-";

fn sort_header(
    ui: &mut egui::Ui,
    label: &str,
    key: SortKey,
    sort_key: &mut SortKey,
    ascending: &mut bool,
) {
    let active = *sort_key == key;
    let clicked = ui
        .horizontal(|ui| {
            let clicked = ui
                .add(egui::Button::new(egui::RichText::new(label).strong()).frame(false))
                .clicked();
            if active {
                let icon = egui::Image::new(crate::icons::chevron_down())
                    .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width * 0.8))
                    .tint(ui.visuals().text_color());
                let icon = if *ascending {
                    icon.rotate(std::f32::consts::PI, egui::Vec2::splat(0.5))
                } else {
                    icon
                };
                ui.add(icon);
            }
            clicked
        })
        .inner;
    if clicked {
        if active {
            *ascending = !*ascending;
        } else {
            *sort_key = key;
            // Names read best ascending; numeric metrics best largest-first.
            *ascending = key == SortKey::Name;
        }
    }
}

fn metric_row(ui: &mut egui::Ui, name: &str, stats: &MetricStats, unit: Unit) {
    ui.monospace(name);
    match unit {
        Unit::Time => {
            for v in [stats.last, stats.avg, stats.min, stats.max, stats.p99] {
                ui.label(format_value(v));
            }
            ui.label(stats.n.to_string());
            ui.label(NA);
        }
        Unit::Bytes => {
            for v in [stats.last, stats.avg, stats.min, stats.max, stats.p99] {
                ui.label(format_bytes(v as u64));
            }
            ui.label(stats.n.to_string());
            ui.label(NA);
        }
        Unit::Count => {
            for _ in 0..6 {
                ui.label(NA);
            }
            ui.label(stats.counter.to_string());
        }
    }
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

    fn stats(avg: f32, n: u64, counter: u64) -> MetricStats {
        MetricStats {
            last: avg,
            min: avg,
            max: avg,
            avg,
            p99: avg,
            n,
            counter,
        }
    }

    #[test]
    fn classifies_timers_gauges_and_counters() {
        assert_eq!(metric_unit("frame_total", &stats(2.0, 100, 0)), Unit::Time);
        assert_eq!(
            metric_unit("upload_bytes", &stats(4096.0, 100, 0)),
            Unit::Bytes
        );
        // Counter-only: no samples, counter set.
        assert_eq!(
            metric_unit("gpu_full_uploads", &stats(0.0, 0, 5)),
            Unit::Count
        );
    }

    #[test]
    fn organize_groups_units_in_display_order() {
        let metrics = vec![
            ("upload_bytes", stats(4096.0, 100, 0)),
            ("gpu_full_uploads", stats(0.0, 0, 5)),
            ("frame_total", stats(2.0, 100, 0)),
        ];
        let groups = organize(&metrics, SortKey::Name, true);
        let units: Vec<Unit> = groups.iter().map(|(u, _)| *u).collect();
        assert_eq!(units, vec![Unit::Time, Unit::Bytes, Unit::Count]);
        assert_eq!(groups[0].1[0].0, "frame_total");
        assert_eq!(groups[1].1[0].0, "upload_bytes");
        assert_eq!(groups[2].1[0].0, "gpu_full_uploads");
    }

    #[test]
    fn organize_sorts_within_a_group_by_column_and_direction() {
        let metrics = vec![
            ("b_timer", stats(3.0, 1, 0)),
            ("a_timer", stats(1.0, 1, 0)),
            ("c_timer", stats(2.0, 1, 0)),
        ];
        // Avg descending.
        let desc = organize(&metrics, SortKey::Avg, false);
        let names: Vec<&str> = desc[0].1.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec!["b_timer", "c_timer", "a_timer"]);
        // Name ascending.
        let asc = organize(&metrics, SortKey::Name, true);
        let names: Vec<&str> = asc[0].1.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec!["a_timer", "b_timer", "c_timer"]);
    }

    #[test]
    fn organize_omits_empty_groups() {
        let metrics = vec![("frame_total", stats(2.0, 100, 0))];
        let groups = organize(&metrics, SortKey::Name, true);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, Unit::Time);
    }

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
