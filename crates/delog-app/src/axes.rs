//! Plot axes: 1-2-5 tick chooser, labels and grid, painted by egui.
//!
//! Axes/ticks/labels are CPU-painted by egui above/below the GPU trace callback.
//! The tick chooser is pure and unit-tested; the draw routine maps data
//! to the same `plot_rect` the GPU viewport uses, so labels line up with the
//! rendered lines.

use std::cmp::Ordering;

/// Height reserved at the bottom for X tick labels.
pub const X_GUTTER: f32 = 22.0;
const AXIS_FONT_SIZE: f32 = 11.0;

/// "Nice" tick values across `[min, max]` at a 1-2-5 × 10ᵏ step, aiming for
/// roughly `target` ticks. Empty when the range is degenerate.
pub fn nice_ticks(min: f64, max: f64, target: usize) -> Vec<f64> {
    if target == 0
        || !min.is_finite()
        || !max.is_finite()
        || max.partial_cmp(&min) != Some(Ordering::Greater)
    {
        return Vec::new();
    }
    let step = nice_step((max - min) / target as f64);
    if step <= 0.0 {
        return Vec::new();
    }
    let first = (min / step).ceil() * step;
    let mut ticks = Vec::new();
    let mut v = first;
    // Guard against pathological ranges producing unbounded ticks.
    while v <= max + step * 1e-6 && ticks.len() < 1000 {
        ticks.push(v);
        v += step;
    }
    ticks
}

/// Round a raw step up to the nearest 1, 2, 5 × 10ᵏ.
fn nice_step(raw: f64) -> f64 {
    if raw <= 0.0 {
        return 0.0;
    }
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let nice = if norm < 1.5 {
        1.0
    } else if norm < 3.0 {
        2.0
    } else if norm < 7.0 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

/// Decimal places to show for a tick at `step` resolution.
pub fn decimals_for_step(step: f64) -> usize {
    if step <= 0.0 || !step.is_finite() {
        return 0;
    }
    let d = -step.log10().floor();
    d.clamp(0.0, 8.0) as usize
}

/// Format a tick value at `step` resolution.
pub fn format_tick(value: f64, step: f64) -> String {
    format!("{value:.*}", decimals_for_step(step))
}

/// Width needed to paint the current Y labels without leaving a fixed gutter.
pub fn y_gutter(ui: &egui::Ui, y_range: (f32, f32), y_unit: Option<&str>, plot_height: f32) -> f32 {
    let (y0, y1) = (y_range.0 as f64, y_range.1 as f64);
    let y_target = (plot_height / 48.0).round().max(2.0) as usize;
    let y_step = step_for(y0, y1, y_target);
    let font = egui::FontId::proportional(AXIS_FONT_SIZE);
    let color = ui.visuals().weak_text_color();
    let painter = ui.painter();
    let mut label_width = nice_ticks(y0, y1, y_target)
        .into_iter()
        .map(|v| {
            painter
                .layout_no_wrap(format_tick(v, y_step), font.clone(), color)
                .rect
                .width()
        })
        .fold(0.0_f32, f32::max);

    if let Some(unit) = y_unit {
        label_width = label_width.max(
            painter
                .layout_no_wrap(unit.to_owned(), font, color)
                .rect
                .width(),
        );
    }

    (label_width + ui.spacing().item_spacing.x).ceil()
}

/// Paint grid lines, tick marks and labels around `plot_rect` for the visible
/// `x_range` (seconds) and `y_range` (data units). Drawn before the GPU trace
/// callback so traces sit on top of the grid.
pub fn draw(
    ui: &egui::Ui,
    plot_rect: egui::Rect,
    x_range: (f32, f32),
    y_range: (f32, f32),
    y_unit: Option<&str>,
) {
    let painter = ui.painter();
    let visuals = ui.visuals();
    let grid = visuals.weak_text_color().gamma_multiply(0.35);
    let label = visuals.weak_text_color();
    let border = visuals
        .widgets
        .noninteractive
        .fg_stroke
        .color
        .gamma_multiply(0.5);

    let (x0, x1) = (x_range.0 as f64, x_range.1 as f64);
    let (y0, y1) = (y_range.0 as f64, y_range.1 as f64);
    let x_target = (plot_rect.width() / 90.0).round().max(2.0) as usize;
    let y_target = (plot_rect.height() / 48.0).round().max(2.0) as usize;
    let x_step = step_for(x0, x1, x_target);
    let y_step = step_for(y0, y1, y_target);

    let to_x = |v: f64| plot_rect.left() + ((v - x0) / (x1 - x0)) as f32 * plot_rect.width();
    let to_y = |v: f64| plot_rect.bottom() - ((v - y0) / (y1 - y0)) as f32 * plot_rect.height();

    for v in nice_ticks(y0, y1, y_target) {
        let y = to_y(v);
        painter.hline(plot_rect.x_range(), y, egui::Stroke::new(1.0, grid));
        painter.text(
            egui::pos2(plot_rect.left() - 4.0, y),
            egui::Align2::RIGHT_CENTER,
            format_tick(v, y_step),
            egui::FontId::proportional(AXIS_FONT_SIZE),
            label,
        );
    }

    for v in nice_ticks(x0, x1, x_target) {
        let x = to_x(v);
        painter.vline(x, plot_rect.y_range(), egui::Stroke::new(1.0, grid));
        painter.text(
            egui::pos2(x, plot_rect.bottom() + 3.0),
            egui::Align2::CENTER_TOP,
            format_tick(v, x_step),
            egui::FontId::proportional(AXIS_FONT_SIZE),
            label,
        );
    }

    // X unit ("s") and optional Y unit, tucked in the corners.
    painter.text(
        egui::pos2(plot_rect.right(), plot_rect.bottom() + 3.0),
        egui::Align2::RIGHT_TOP,
        "s",
        egui::FontId::proportional(AXIS_FONT_SIZE),
        label,
    );
    if let Some(unit) = y_unit {
        painter.text(
            egui::pos2(plot_rect.left() - 4.0, plot_rect.top() - 2.0),
            egui::Align2::RIGHT_BOTTOM,
            unit,
            egui::FontId::proportional(AXIS_FONT_SIZE),
            label,
        );
    }

    painter.rect_stroke(
        plot_rect,
        0.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
}

/// The 1-2-5 step `nice_ticks` would use for `[min, max]` at `target` ticks —
/// exposed so callers can format labels at matching precision.
pub fn step_for(min: f64, max: f64, target: usize) -> f64 {
    if target == 0 || max.partial_cmp(&min) != Some(Ordering::Greater) {
        return 0.0;
    }
    nice_step((max - min) / target as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_use_1_2_5_steps() {
        assert_eq!(
            nice_ticks(0.0, 10.0, 5),
            vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0]
        );
        // 0..1 with ~5 ticks → step 0.2.
        let t = nice_ticks(0.0, 1.0, 5);
        assert_eq!(t.len(), 6);
        assert!((t[1] - 0.2).abs() < 1e-9);
        // A range that wants step 5.
        assert_eq!(
            nice_ticks(0.0, 30.0, 5),
            vec![0.0, 5.0, 10.0, 15.0, 20.0, 25.0, 30.0]
        );
    }

    #[test]
    fn ticks_start_on_a_step_boundary_inside_the_range() {
        let t = nice_ticks(3.0, 17.0, 5);
        assert_eq!(t.first().copied(), Some(4.0)); // step 2, first multiple ≥ 3
        assert!(*t.last().unwrap() <= 17.0 + 1e-6);
    }

    #[test]
    fn degenerate_ranges_yield_no_ticks() {
        assert!(nice_ticks(5.0, 5.0, 5).is_empty());
        assert!(nice_ticks(10.0, 0.0, 5).is_empty());
        assert!(nice_ticks(0.0, 1.0, 0).is_empty());
        assert!(nice_ticks(f64::NAN, 1.0, 5).is_empty());
    }

    #[test]
    fn decimals_track_step_magnitude() {
        assert_eq!(decimals_for_step(1.0), 0);
        assert_eq!(decimals_for_step(0.2), 1);
        assert_eq!(decimals_for_step(0.05), 2);
        assert_eq!(format_tick(0.2, 0.2), "0.2");
        assert_eq!(format_tick(4.0, 2.0), "4");
    }
}
