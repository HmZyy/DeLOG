use super::controller::PreviewStats;
use egui_extras::{Column, TableBody, TableBuilder};
use std::hash::Hash;

const LABEL_RATIO: f32 = 0.4;
const LABEL_MIN: f32 = 52.0;
const LABEL_MAX: f32 = 104.0;
const ACTION_WIDTH: f32 = 26.0;
const MIN_CELL: f32 = 24.0;
const SECTION_GAP: f32 = 12.0;
const CARD_PAD: i8 = 8;
const ROW_EXTRA: f32 = 8.0;
const HEADER_HEIGHT: f32 = 24.0;

pub(super) fn section(ui: &mut egui::Ui, title: &str) {
    if ui.min_rect().height() > 0.0 {
        ui.add_space(SECTION_GAP);
    }
    ui.add(
        egui::Label::new(
            egui::RichText::new(title.to_uppercase())
                .small()
                .color(ui.visuals().weak_text_color()),
        )
        .truncate(),
    );
}

pub(super) struct Rows<'a> {
    body: TableBody<'a>,
    height: f32,
    divider: Divider,
    first: bool,
}

impl Rows<'_> {
    pub(super) fn property(&mut self, label: &str, value: impl FnOnce(&mut egui::Ui)) {
        self.row(label, None, value);
    }

    pub(super) fn hinted(&mut self, label: &str, hint: &str, value: impl FnOnce(&mut egui::Ui)) {
        self.row(label, Some(hint), value);
    }

    fn row(&mut self, label: &str, hint: Option<&str>, value: impl FnOnce(&mut egui::Ui)) {
        let height = self.height;
        let divider = (!std::mem::take(&mut self.first)).then_some(self.divider);
        self.body.row(height, |mut row| {
            row.col(|ui| {
                if let Some(divider) = divider {
                    divider.paint(ui);
                }
                ui.add(egui::Label::new(egui::RichText::new(label).weak()).truncate())
                    .on_hover_text(hint.unwrap_or(label));
            });
            row.col(value);
        });
    }
}

pub(super) fn properties(ui: &mut egui::Ui, id: impl Hash, rows: impl FnOnce(&mut Rows<'_>)) {
    card(ui, |ui| {
        let height = row_height(ui);
        let divider = Divider::capture(ui);
        let label = label_width(ui);
        let value = (content_width(ui, 2) - label).max(MIN_CELL);
        table(ui, id)
            .column(Column::exact(label))
            .column(Column::exact(value))
            .body(|body| {
                rows(&mut Rows {
                    body,
                    height,
                    divider,
                    first: true,
                });
            });
    });
}

pub(super) fn text_value(ui: &mut egui::Ui, value: impl Into<String>) {
    let value = value.into();
    ui.add(egui::Label::new(&value).truncate())
        .on_hover_text(value);
}

pub(super) fn number(ui: &mut egui::Ui, value: &mut f64) {
    ui.add_sized(
        [ui.available_width(), ui.spacing().interact_size.y],
        egui::DragValue::new(value),
    );
}

pub(super) fn text_edit(ui: &mut egui::Ui, value: &mut String, hint: &str) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), ui.spacing().interact_size.y],
        egui::TextEdit::singleline(value)
            .desired_width(0.0)
            .hint_text(hint),
    )
}

pub(super) fn combo(
    ui: &mut egui::Ui,
    id: impl Hash,
    selected: &str,
    options: impl FnOnce(&mut egui::Ui),
) {
    egui::ComboBox::from_id_salt(id)
        .width(ui.available_width())
        .truncate()
        .selected_text(selected)
        .show_ui(ui, options);
}

pub(super) enum PortEdit {
    Remove(usize),
    Add,
}

pub(super) fn ports<'a>(
    ui: &mut egui::Ui,
    id: impl Hash,
    noun: &str,
    with_units: bool,
    add_label: &str,
    fields: impl Iterator<Item = (&'a mut String, Option<&'a mut Option<String>>)>,
) -> Option<PortEdit> {
    let mut removed = None;
    card(ui, |ui| {
        let height = row_height(ui);
        let divider = Divider::capture(ui);
        let (name_width, unit_width) = port_widths(ui, with_units);
        let mut builder = table(ui, id).column(Column::exact(name_width));
        if let Some(unit_width) = unit_width {
            builder = builder.column(Column::exact(unit_width));
        }
        builder
            .column(Column::exact(ACTION_WIDTH))
            .header(HEADER_HEIGHT, |mut row| {
                row.col(|ui| header(ui, "Name"));
                if unit_width.is_some() {
                    row.col(|ui| header(ui, "Unit"));
                }
                row.col(|_| {});
            })
            .body(|mut body| {
                for (index, (name, unit)) in fields.enumerate() {
                    body.row(height, |mut row| {
                        let label = remove_label(noun, name);
                        row.col(|ui| {
                            divider.paint(ui);
                            text_edit(ui, name, "Name");
                        });
                        if let Some(unit) = unit {
                            row.col(|ui| {
                                let mut value = unit.clone().unwrap_or_default();
                                if text_edit(ui, &mut value, "Unit").changed() {
                                    *unit = (!value.is_empty()).then_some(value);
                                }
                            });
                        }
                        row.col(|ui| {
                            if remove_button(ui, &label) {
                                removed = Some(PortEdit::Remove(index));
                            }
                        });
                    });
                }
            });
        if add_row(ui, add_label) {
            removed.get_or_insert(PortEdit::Add);
        }
    });
    removed
}

#[cfg(feature = "scripting")]
pub(super) fn wide_button(ui: &mut egui::Ui, label: &str) -> bool {
    ui.add_sized(
        [ui.available_width(), row_height(ui)],
        egui::Button::new(egui::RichText::new(label).small()),
    )
    .clicked()
}

fn add_row(ui: &mut egui::Ui, label: &str) -> bool {
    divider(ui);
    let color = ui.visuals().weak_text_color();
    let icon = egui::Image::new(crate::ui::icons::plus())
        .fit_to_exact_size(egui::Vec2::splat(13.0))
        .tint(color);
    let text = egui::RichText::new(label).small().color(color);
    ui.add_sized(
        [ui.available_width(), row_height(ui)],
        egui::Button::image_and_text(icon, text).frame_when_inactive(false),
    )
    .clicked()
}

fn divider(ui: &mut egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(
        rect.x_range(),
        rect.top(),
        egui::Stroke::new(1.0, line_color(ui, 0.45)),
    );
}

pub(super) fn errors<'a>(ui: &mut egui::Ui, messages: impl Iterator<Item = &'a str>) {
    let color = ui.visuals().error_fg_color;
    egui::Frame::new()
        .fill(color.gamma_multiply(0.08))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.4)))
        .corner_radius(ui.visuals().widgets.noninteractive.corner_radius)
        .inner_margin(CARD_PAD)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for message in messages {
                ui.add(egui::Label::new(egui::RichText::new(message).color(color)).wrap());
            }
        });
}

pub(super) fn preview(ui: &mut egui::Ui, id: impl Hash, stats: PreviewStats) {
    properties(ui, id, |rows| {
        for (label, display, exact) in [
            ("Count", stats.count.to_string(), stats.count.to_string()),
            (
                "NaN",
                stats.nan_count.to_string(),
                stats.nan_count.to_string(),
            ),
            ("Min", format_stat(stats.min), stats.min.to_string()),
            ("Max", format_stat(stats.max), stats.max.to_string()),
            ("Mean", format_stat(stats.mean), stats.mean.to_string()),
            (
                "Stddev",
                format_stat(stats.stddev),
                stats.stddev.to_string(),
            ),
            (
                "Start (us)",
                stats.t0_us.to_string(),
                stats.t0_us.to_string(),
            ),
            ("End (us)", stats.t1_us.to_string(), stats.t1_us.to_string()),
        ] {
            rows.property(label, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(display).monospace()).truncate())
                        .on_hover_text(exact);
                });
            });
        }
    });
}

pub(super) fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, line_color(ui, 0.7)))
        .corner_radius(ui.visuals().widgets.noninteractive.corner_radius)
        .inner_margin(egui::Margin {
            left: CARD_PAD,
            right: CARD_PAD,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

#[derive(Clone, Copy)]
struct Divider {
    x_range: egui::Rangef,
    clip: egui::Rect,
    stroke: egui::Stroke,
}

impl Divider {
    fn capture(ui: &egui::Ui) -> Self {
        Self {
            x_range: ui.available_rect_before_wrap().x_range(),
            clip: ui.clip_rect(),
            stroke: egui::Stroke::new(1.0, line_color(ui, 0.45)),
        }
    }

    fn paint(self, ui: &egui::Ui) {
        ui.ctx()
            .layer_painter(ui.layer_id())
            .with_clip_rect(self.clip)
            .hline(self.x_range, ui.max_rect().top(), self.stroke);
    }
}

fn header(ui: &mut egui::Ui, title: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(title)
                .small()
                .color(ui.visuals().weak_text_color()),
        )
        .truncate(),
    );
}

fn remove_label(noun: &str, name: &str) -> String {
    if name.is_empty() {
        format!("Remove {noun}")
    } else {
        format!("Remove {noun} {name}")
    }
}

fn remove_button(ui: &mut egui::Ui, label: &str) -> bool {
    let icon = egui::Image::new(crate::ui::icons::trash())
        .fit_to_exact_size(egui::Vec2::splat(14.0))
        .tint(ui.visuals().weak_text_color());
    let size = egui::Vec2::splat(ui.available_width().min(ui.available_height()));
    let response = ui
        .add_sized(size, egui::Button::image(icon).frame(false))
        .on_hover_text(label);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    response.clicked()
}

fn line_color(ui: &egui::Ui, opacity: f32) -> egui::Color32 {
    ui.visuals()
        .widgets
        .noninteractive
        .bg_stroke
        .color
        .gamma_multiply(opacity)
}

fn row_height(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y.max(22.0) + ROW_EXTRA
}

fn label_width(ui: &egui::Ui) -> f32 {
    let total = content_width(ui, 2);
    (total * LABEL_RATIO).clamp(LABEL_MIN.min(total * 0.5), LABEL_MAX)
}

fn port_widths(ui: &egui::Ui, with_units: bool) -> (f32, Option<f32>) {
    if !with_units {
        return ((content_width(ui, 2) - ACTION_WIDTH).max(MIN_CELL), None);
    }
    let name = label_width(ui);
    let unit = (content_width(ui, 3) - ACTION_WIDTH - name).max(MIN_CELL);
    (name, Some(unit))
}

fn content_width(ui: &egui::Ui, columns: usize) -> f32 {
    let gaps = ui.spacing().item_spacing.x * (columns.saturating_sub(1)) as f32;
    (ui.available_width() - gaps).max(MIN_CELL * columns as f32)
}

fn table(ui: &mut egui::Ui, id: impl Hash) -> TableBuilder<'_> {
    TableBuilder::new(ui)
        .id_salt(id)
        .striped(false)
        .resizable(false)
        .vscroll(false)
        .auto_shrink([false, true])
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
}

fn format_stat(value: f64) -> String {
    if !value.is_finite() {
        value.to_string()
    } else if value != 0.0 && !(0.0001..10_000_000.0).contains(&value.abs()) {
        format!("{value:.6e}")
    } else {
        egui::emath::format_with_decimals_in_range(value, 0..=6)
    }
}
