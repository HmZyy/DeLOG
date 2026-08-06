use super::{AnnotationLayer, DataPos, Geometry, Kind};

pub fn create_at(
    layer: &mut AnnotationLayer,
    kind: Kind,
    at: DataPos,
    span_us: i64,
    y_span: f64,
) -> u64 {
    let id = layer.add(kind, at, span_us, y_span);
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
) -> bool {
    let mut created = false;
    crate::ui::components::dense_rows(ui);
    let at = layer.last_cursor.unwrap_or(fallback);
    for kind in Kind::ALL {
        let button = egui::Button::image_and_text(menu_icon(ui, icon_for(kind)), kind.label());
        if ui.add(button).clicked() {
            create_at(layer, kind, at, span_us, y_span);
            created = true;
            ui.close();
        }
    }
    created
}
