use delog_flow::signal::{MAX_ORDER, SignalFilter};

use super::inspector_tables::Rows;

const ZERO_PHASE_HINT: &str = "Run the filter forward and backward: no time lag. Off reproduces a causal onboard filter, lag included.";
const ORDER_HINT: &str = "Higher orders roll off more steeply past the cutoff.";
const TIME_WINDOW_HINT: &str = "Width of the centered time window around each sample.";

pub(super) fn signal_rows(rows: &mut Rows<'_>, filter: &mut SignalFilter) {
    match filter {
        SignalFilter::Lowpass {
            cutoff_hz,
            order,
            zero_phase,
        }
        | SignalFilter::Highpass {
            cutoff_hz,
            order,
            zero_phase,
        } => {
            rows.property("Cutoff", |ui| hertz(ui, cutoff_hz));
            rows.hinted("Order", ORDER_HINT, |ui| filter_order(ui, order));
            rows.hinted("Zero phase", ZERO_PHASE_HINT, |ui| {
                ui.checkbox(zero_phase, "");
            });
        }
        SignalFilter::Bandpass {
            low_hz,
            high_hz,
            order,
            zero_phase,
        } => {
            rows.property("Low cutoff", |ui| hertz(ui, low_hz));
            rows.property("High cutoff", |ui| hertz(ui, high_hz));
            rows.hinted("Order", ORDER_HINT, |ui| filter_order(ui, order));
            rows.hinted("Zero phase", ZERO_PHASE_HINT, |ui| {
                ui.checkbox(zero_phase, "");
            });
        }
        SignalFilter::Notch {
            center_hz,
            bandwidth_hz,
            zero_phase,
        } => {
            rows.property("Center", |ui| hertz(ui, center_hz));
            rows.hinted(
                "Bandwidth",
                "Width of the rejected band around the center frequency.",
                |ui| hertz(ui, bandwidth_hz),
            );
            rows.hinted("Zero phase", ZERO_PHASE_HINT, |ui| {
                ui.checkbox(zero_phase, "");
            });
        }
        SignalFilter::MovingAverage { window_s } | SignalFilter::Median { window_s } => {
            rows.hinted("Window", TIME_WINDOW_HINT, |ui| {
                sized(
                    ui,
                    egui::DragValue::new(window_s)
                        .speed(0.01)
                        .range(0.0..=f64::MAX)
                        .suffix(" s"),
                );
            });
        }
        SignalFilter::SavitzkyGolay { window, poly_order } => {
            rows.hinted(
                "Window",
                "Odd number of samples fitted around each point.",
                |ui| {
                    sized(
                        ui,
                        egui::DragValue::new(window)
                            .speed(0.1)
                            .range(3..=u32::MAX)
                            .suffix(" samples"),
                    );
                },
            );
            rows.hinted(
                "Polynomial order",
                "Degree of the polynomial fitted in each window.",
                |ui| {
                    sized(ui, egui::DragValue::new(poly_order).speed(0.05));
                },
            );
        }
    }
}

fn hertz(ui: &mut egui::Ui, value: &mut f64) {
    sized(
        ui,
        egui::DragValue::new(value)
            .speed(0.1)
            .range(0.0..=f64::MAX)
            .suffix(" Hz"),
    );
}

fn filter_order(ui: &mut egui::Ui, order: &mut u32) {
    sized(
        ui,
        egui::DragValue::new(order).speed(0.05).range(1..=MAX_ORDER),
    );
}

fn sized(ui: &mut egui::Ui, widget: egui::DragValue<'_>) {
    ui.add_sized([ui.available_width(), ui.spacing().interact_size.y], widget);
}
