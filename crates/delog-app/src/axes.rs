use std::cmp::Ordering;

pub const X_GUTTER: f32 = 22.0;
const AXIS_FONT_SIZE: f32 = 11.0;

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

pub fn decimals_for_step(step: f64) -> usize {
    if step <= 0.0 || !step.is_finite() {
        return 0;
    }
    let d = -step.log10().floor();
    d.clamp(0.0, 8.0) as usize
}

pub fn format_tick(value: f64, step: f64) -> String {
    format!("{value:.*}", decimals_for_step(step))
}

#[derive(Debug, Clone, PartialEq)]
struct YAxisTick {
    value: f64,
    label: String,
}

#[derive(Debug, Clone, PartialEq)]
struct YAxisFormatting {
    ticks: Vec<YAxisTick>,
    offset_annotation: Option<String>,
}

fn y_axis_formatting(origin: f64, y_range: (f64, f64), plot_height: f32) -> YAxisFormatting {
    let (y0, y1) = y_range;
    let target = (plot_height / 48.0).round().max(2.0) as usize;
    let step = step_for(y0, y1, target);
    let values = nice_ticks(y0, y1, target);
    let absolute_labels: Vec<_> = values
        .iter()
        .map(|value| format_tick(origin + value, step))
        .collect();
    let absolute_is_distinct = values.windows(2).all(|pair| {
        let left = origin + pair[0];
        let right = origin + pair[1];
        left.is_finite() && right.is_finite() && left != right
    }) && absolute_labels.windows(2).all(|pair| pair[0] != pair[1]);
    let absorbed_tick = values
        .iter()
        .any(|value| *value != 0.0 && origin + value == origin);
    let relative = !absolute_is_distinct || absorbed_tick;
    let labels = if relative {
        values
            .iter()
            .map(|value| format_tick(*value, step))
            .collect()
    } else {
        absolute_labels
    };
    YAxisFormatting {
        ticks: values
            .into_iter()
            .zip(labels)
            .map(|(value, label)| YAxisTick { value, label })
            .collect(),
        offset_annotation: relative.then(|| format!("offset {origin:+e}")),
    }
}

fn y_axis_header(y_unit: Option<&str>, offset_annotation: Option<&str>) -> Option<String> {
    match (y_unit, offset_annotation) {
        (Some(unit), Some(offset)) => Some(format!("{unit} · {offset}")),
        (Some(unit), None) => Some(unit.to_owned()),
        (None, Some(offset)) => Some(offset.to_owned()),
        (None, None) => None,
    }
}

pub fn y_gutter(ui: &egui::Ui, y_range: (f64, f64), y_unit: Option<&str>, plot_height: f32) -> f32 {
    y_gutter_relative(ui, 0.0, y_range, y_unit, plot_height)
}

pub fn y_gutter_relative(
    ui: &egui::Ui,
    origin: f64,
    y_range: (f64, f64),
    y_unit: Option<&str>,
    plot_height: f32,
) -> f32 {
    let formatting = y_axis_formatting(origin, y_range, plot_height);
    let font = egui::FontId::proportional(AXIS_FONT_SIZE);
    let color = ui.visuals().weak_text_color();
    let painter = ui.painter();
    let mut label_width = formatting
        .ticks
        .iter()
        .map(|tick| {
            painter
                .layout_no_wrap(tick.label.clone(), font.clone(), color)
                .rect
                .width()
        })
        .fold(0.0_f32, f32::max);

    if let Some(header) = y_axis_header(y_unit, formatting.offset_annotation.as_deref()) {
        label_width = label_width.max(painter.layout_no_wrap(header, font, color).rect.width());
    }

    (label_width + ui.spacing().item_spacing.x).ceil()
}

/// `x_range` is seconds, `y_range` is data units. Drawn before the GPU trace
/// callback so traces sit on top of the grid.
pub fn draw(
    ui: &egui::Ui,
    plot_rect: egui::Rect,
    x_range: (f32, f32),
    y_range: (f64, f64),
    y_unit: Option<&str>,
) {
    let formatting = y_axis_formatting(0.0, y_range, plot_rect.height());
    draw_y_grid_formatted(ui, plot_rect, y_range, &formatting);
    draw_x(ui, plot_rect, x_range);
    draw_y_unit(
        ui,
        plot_rect,
        y_unit,
        formatting.offset_annotation.as_deref(),
    );
    draw_border(ui, plot_rect);
}

pub fn draw_y(ui: &egui::Ui, plot_rect: egui::Rect, y_range: (f64, f64), y_unit: Option<&str>) {
    let formatting = y_axis_formatting(0.0, y_range, plot_rect.height());
    draw_y_grid_formatted(ui, plot_rect, y_range, &formatting);
    draw_y_unit(
        ui,
        plot_rect,
        y_unit,
        formatting.offset_annotation.as_deref(),
    );
}

pub fn draw_y_grid(ui: &egui::Ui, plot_rect: egui::Rect, y_range: (f64, f64)) {
    let formatting = y_axis_formatting(0.0, y_range, plot_rect.height());
    draw_y_grid_formatted(ui, plot_rect, y_range, &formatting);
}

pub fn draw_y_relative(
    ui: &egui::Ui,
    plot_rect: egui::Rect,
    origin: f64,
    y_range: (f64, f64),
    y_unit: Option<&str>,
) {
    let formatting = y_axis_formatting(origin, y_range, plot_rect.height());
    draw_y_grid_formatted(ui, plot_rect, y_range, &formatting);
    draw_y_unit(
        ui,
        plot_rect,
        y_unit,
        formatting.offset_annotation.as_deref(),
    );
}

fn draw_y_grid_formatted(
    ui: &egui::Ui,
    plot_rect: egui::Rect,
    y_range: (f64, f64),
    formatting: &YAxisFormatting,
) {
    if !usable_plot_rect(plot_rect) {
        return;
    }
    let painter = ui.painter();
    let visuals = ui.visuals();
    let grid = visuals.weak_text_color().gamma_multiply(0.35);
    let label = visuals.weak_text_color();
    let (y0, y1) = y_range;
    let to_y = |v: f64| plot_rect.bottom() - ((v - y0) / (y1 - y0)) as f32 * plot_rect.height();

    for tick in &formatting.ticks {
        let y = to_y(tick.value);
        painter.hline(plot_rect.x_range(), y, egui::Stroke::new(1.0, grid));
        painter.text(
            egui::pos2(plot_rect.left() - 4.0, y),
            egui::Align2::RIGHT_CENTER,
            &tick.label,
            egui::FontId::proportional(AXIS_FONT_SIZE),
            label,
        );
    }
}

pub fn draw_y_unit(
    ui: &egui::Ui,
    plot_rect: egui::Rect,
    y_unit: Option<&str>,
    offset_annotation: Option<&str>,
) {
    if !usable_plot_rect(plot_rect) {
        return;
    }
    if let Some(header) = y_axis_header(y_unit, offset_annotation) {
        let label = ui.visuals().weak_text_color();
        let painter = ui.painter();
        painter.text(
            egui::pos2(plot_rect.left() - 4.0, plot_rect.top() - 2.0),
            egui::Align2::RIGHT_BOTTOM,
            header,
            egui::FontId::proportional(AXIS_FONT_SIZE),
            label,
        );
    }
}

pub fn draw_x(ui: &egui::Ui, plot_rect: egui::Rect, x_range: (f32, f32)) {
    if !usable_plot_rect(plot_rect) {
        return;
    }
    let painter = ui.painter();
    let visuals = ui.visuals();
    let grid = visuals.weak_text_color().gamma_multiply(0.35);
    let label = visuals.weak_text_color();
    let (x0, x1) = (x_range.0 as f64, x_range.1 as f64);
    let x_target = (plot_rect.width() / 90.0).round().max(2.0) as usize;
    let x_step = step_for(x0, x1, x_target);
    let to_x = |v: f64| plot_rect.left() + ((v - x0) / (x1 - x0)) as f32 * plot_rect.width();

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

    painter.text(
        egui::pos2(plot_rect.right(), plot_rect.bottom() + 3.0),
        egui::Align2::RIGHT_TOP,
        "s",
        egui::FontId::proportional(AXIS_FONT_SIZE),
        label,
    );
}

pub fn draw_border(ui: &egui::Ui, plot_rect: egui::Rect) {
    if !usable_plot_rect(plot_rect) {
        return;
    }
    let painter = ui.painter();
    let border = ui
        .visuals()
        .widgets
        .noninteractive
        .fg_stroke
        .color
        .gamma_multiply(0.5);
    painter.rect_stroke(
        plot_rect,
        0.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
}

pub fn usable_plot_rect(rect: egui::Rect) -> bool {
    rect.width().is_finite()
        && rect.height().is_finite()
        && rect.width() >= 2.0
        && rect.height() >= 2.0
}

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
        let t = nice_ticks(0.0, 1.0, 5);
        assert_eq!(t.len(), 6);
        assert!((t[1] - 0.2).abs() < 1e-9);
        assert_eq!(
            nice_ticks(0.0, 30.0, 5),
            vec![0.0, 5.0, 10.0, 15.0, 20.0, 25.0, 30.0]
        );
    }

    #[test]
    fn ticks_start_on_a_step_boundary_inside_the_range() {
        let t = nice_ticks(3.0, 17.0, 5);
        assert_eq!(t.first().copied(), Some(4.0));
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

    #[test]
    fn huge_origin_small_span_uses_distinct_relative_labels_and_one_offset() {
        let formatting = y_axis_formatting(1.0e20, (-1.0, 1.0), 240.0);
        let labels: Vec<_> = formatting
            .ticks
            .iter()
            .map(|tick| tick.label.as_str())
            .collect();
        assert!(labels.windows(2).all(|pair| pair[0] != pair[1]));
        assert_eq!(
            formatting.offset_annotation.as_deref(),
            Some("offset +1e20")
        );
    }

    #[test]
    fn huge_origin_single_absorbed_tick_uses_relative_label_header_and_gutter() {
        let origin = 1.0e20;
        let y_range = (0.11, 0.19);
        let plot_height = 96.0;
        let formatting = y_axis_formatting(origin, y_range, plot_height);
        assert_eq!(formatting.ticks.len(), 1);
        assert_eq!(formatting.ticks[0].label, "0.15");
        assert_eq!(
            formatting.offset_annotation.as_deref(),
            Some("offset +1e20")
        );
        assert_eq!(
            y_axis_header(None, formatting.offset_annotation.as_deref()).as_deref(),
            Some("offset +1e20"),
        );

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = egui::FontId::proportional(AXIS_FONT_SIZE);
            let color = ui.visuals().weak_text_color();
            let expected_header_width = ui
                .painter()
                .layout_no_wrap("offset +1e20".to_owned(), font, color)
                .rect
                .width();
            let gutter = y_gutter_relative(ui, origin, y_range, None, plot_height);
            assert_eq!(
                gutter,
                (expected_header_width + ui.spacing().item_spacing.x).ceil()
            );
        });
    }

    #[test]
    fn ordinary_origin_keeps_absolute_tick_labels_without_offset() {
        let formatting = y_axis_formatting(10.0, (0.0, 2.0), 96.0);
        let labels: Vec<_> = formatting
            .ticks
            .iter()
            .map(|tick| tick.label.as_str())
            .collect();
        assert_eq!(labels, ["10", "11", "12"]);
        assert_eq!(formatting.offset_annotation, None);
    }

    #[test]
    fn draw_delegates_to_axis_and_border_helpers() {
        let source = include_str!("axes.rs");
        let draw = source
            .split("pub fn draw(")
            .nth(1)
            .expect("draw function should exist")
            .split("pub fn step_for")
            .next()
            .expect("draw function should precede step_for");
        assert!(draw.contains("y_axis_formatting(0.0, y_range, plot_rect.height())"));
        assert!(draw.contains("draw_y_grid_formatted(ui, plot_rect, y_range, &formatting);"));
        assert!(draw.contains("draw_x(ui, plot_rect, x_range);"));
        assert!(draw.contains("draw_y_unit("));
        assert!(draw.contains("draw_border(ui, plot_rect);"));
    }

    #[test]
    fn normal_draw_preserves_grid_unit_and_border_paint_order() {
        let source = include_str!("axes.rs");
        let draw = source
            .split("pub fn draw(")
            .nth(1)
            .expect("draw function should exist")
            .split("pub fn draw_y")
            .next()
            .expect("draw should precede Y helpers");
        let y_grid = draw
            .find("draw_y_grid_formatted(")
            .expect("Y grid delegation");
        let x = draw.find("draw_x(").expect("X delegation");
        let y_unit = draw.find("draw_y_unit(").expect("Y unit delegation");
        let border = draw.find("draw_border(").expect("border delegation");
        assert!(y_grid < x && x < y_unit && y_unit < border);
    }

    #[test]
    fn adaptive_axes_reject_non_positive_and_tiny_rects() {
        assert!(!usable_plot_rect(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(0.0, 100.0),
        )));
        assert!(!usable_plot_rect(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1.0e-7, 100.0),
        )));
        assert!(usable_plot_rect(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(2.0, 2.0),
        )));
    }
}
