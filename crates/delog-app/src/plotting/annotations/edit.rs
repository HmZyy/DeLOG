use super::{AnnotationLayer, DataPos, Geometry, Kind};

pub fn create_at(
    layer: &mut AnnotationLayer,
    kind: Kind,
    at: DataPos,
    span_us: i64,
    y_span: f64,
    trace_count: usize,
) -> u64 {
    close_editor(layer);
    let id = layer.add(kind, at, span_us, y_span, trace_count);
    layer.selected = Some(id);
    layer.editing = (kind == Kind::Text).then_some(id);
    id
}

pub fn close_editor(layer: &mut AnnotationLayer) {
    let Some(id) = layer.editing.take() else {
        return;
    };
    let empty_text = layer
        .get(id)
        .is_some_and(|a| matches!(a.geom, Geometry::Text { .. }) && a.label.trim().is_empty());
    if empty_text {
        layer.remove(id);
    }
}

fn icon_for(kind: Kind) -> egui::ImageSource<'static> {
    match kind {
        Kind::Text => crate::ui::icons::text_cursor(),
        Kind::Segment => crate::ui::icons::arrow_right(),
        Kind::Rect => crate::ui::icons::square(),
        Kind::Ellipse => crate::ui::icons::circle(),
        Kind::HLine => crate::ui::icons::minus(),
    }
}

fn menu_icon(ui: &egui::Ui, source: egui::ImageSource<'static>) -> egui::Image<'static> {
    egui::Image::new(source)
        .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
        .tint(ui.visuals().text_color())
}

pub fn menu(
    ui: &mut egui::Ui,
    layer: &mut AnnotationLayer,
    span_us: i64,
    y_span: f64,
    fallback: DataPos,
    trace_count: usize,
) {
    crate::ui::components::dense_rows(ui);
    let at = layer.last_cursor.unwrap_or(fallback);
    for kind in Kind::ALL {
        let button = egui::Button::image_and_text(menu_icon(ui, icon_for(kind)), kind.label());
        if ui.add(button).clicked() {
            create_at(layer, kind, at, span_us, y_span, trace_count);
            ui.close();
        }
    }
}

const SECONDS_ROW: &str = "t";
const VALUE_ROW: &str = "y";
const T1_ROW: &str = "t1";
const Y1_ROW: &str = "y1";
const T2_ROW: &str = "t2";
const Y2_ROW: &str = "y2";

fn seconds_of(t_us: i64, origin_us: i64) -> f64 {
    (t_us as i128 - origin_us as i128) as f64 * 1e-6
}

fn micros_of(seconds: f64, origin_us: i64) -> i64 {
    origin_us.saturating_add((seconds * 1e6).round() as i64)
}

pub fn anchor_seconds(geom: &Geometry, origin_us: i64) -> Vec<(&'static str, f64)> {
    match *geom {
        Geometry::HLine { y } => vec![(VALUE_ROW, y)],
        Geometry::Text { at } => vec![
            (SECONDS_ROW, seconds_of(at.t_us, origin_us)),
            (VALUE_ROW, at.y),
        ],
        Geometry::Segment { from, to } => vec![
            (T1_ROW, seconds_of(from.t_us, origin_us)),
            (Y1_ROW, from.y),
            (T2_ROW, seconds_of(to.t_us, origin_us)),
            (Y2_ROW, to.y),
        ],
        Geometry::Rect { a, b } | Geometry::Ellipse { a, b } => vec![
            (T1_ROW, seconds_of(a.t_us, origin_us)),
            (Y1_ROW, a.y),
            (T2_ROW, seconds_of(b.t_us, origin_us)),
            (Y2_ROW, b.y),
        ],
    }
}

pub fn set_anchor_seconds(geom: &mut Geometry, index: usize, value: f64, origin_us: i64) {
    match geom {
        Geometry::HLine { y } => {
            if index == 0 {
                *y = value;
            }
        }
        Geometry::Text { at } => set_point(at, index, value, origin_us),
        Geometry::Segment { from, to } => match index {
            0 | 1 => set_point(from, index, value, origin_us),
            2 | 3 => set_point(to, index - 2, value, origin_us),
            _ => {}
        },
        Geometry::Rect { a, b } | Geometry::Ellipse { a, b } => match index {
            0 | 1 => set_point(a, index, value, origin_us),
            2 | 3 => set_point(b, index - 2, value, origin_us),
            _ => {}
        },
    }
}

fn set_point(point: &mut DataPos, index: usize, value: f64, origin_us: i64) {
    match index {
        0 => point.t_us = micros_of(value, origin_us),
        1 => point.y = value,
        _ => {}
    }
}

pub fn editor(
    ctx: &egui::Context,
    id: egui::Id,
    layer: &mut AnnotationLayer,
    origin_us: i64,
) {
    let Some(editing) = layer.editing else {
        return;
    };
    let Some(current) = layer.get(editing).cloned() else {
        layer.editing = None;
        return;
    };
    let mut draft = current.clone();
    let mut delete = false;
    let mut close = false;
    let modal = egui::Modal::new(id).show(ctx, |ui| {
        ui.set_width(300.0);
        ui.label(format!("{} annotation", draft.geom.kind().label()));
        ui.add(
            egui::TextEdit::singleline(&mut draft.label)
                .desired_width(f32::INFINITY)
                .hint_text("label"),
        );
        ui.horizontal(|ui| {
            ui.label("Color");
            let mut color = draft.style.color32();
            if egui::color_picker::color_edit_button_srgba(
                ui,
                &mut color,
                egui::color_picker::Alpha::Opaque,
            )
            .changed()
            {
                draft.style.color = crate::plotting::legend::color32_to_srgb(color);
            }
        });
        let kind = draft.geom.kind();
        if kind != Kind::Text {
            ui.add(egui::Slider::new(&mut draft.style.stroke_px, 0.5..=6.0).text("Stroke"));
        }
        if matches!(kind, Kind::Rect | Kind::Ellipse) {
            ui.add(egui::Slider::new(&mut draft.style.fill_opacity, 0.0..=1.0).text("Fill"));
        }
        ui.add(egui::Slider::new(&mut draft.style.font_px, 8.0..=24.0).text("Font"));
        if kind == Kind::Segment {
            ui.checkbox(&mut draft.style.arrow, "Arrowhead");
        }
        let rows = anchor_seconds(&draft.geom, origin_us);
        for (index, (name, value)) in rows.into_iter().enumerate() {
            let mut edited = value;
            ui.horizontal(|ui| {
                ui.label(name);
                if ui.add(egui::DragValue::new(&mut edited).speed(0.01)).changed() {
                    set_anchor_seconds(&mut draft.geom, index, edited, origin_us);
                }
            });
        }
        ui.horizontal(|ui| {
            if ui.button("Close").clicked() {
                close = true;
            }
            if ui
                .add(egui::Button::image_and_text(
                    menu_icon(ui, crate::ui::icons::trash()),
                    "Delete",
                ))
                .clicked()
            {
                delete = true;
            }
        });
    });
    if draft != current
        && let Some(annot) = layer.get_mut(editing)
    {
        *annot = draft;
    }
    if delete {
        layer.remove(editing);
        layer.editing = None;
    } else if close || modal.should_close() {
        close_editor(layer);
    }
}
