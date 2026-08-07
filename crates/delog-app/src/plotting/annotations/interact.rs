use super::hit;
use super::{AnnotationLayer, DataPos, Grab, PlotTransform};
use crate::plotting::gpu::PaneView;

pub fn begin_grab(layer: &mut AnnotationLayer, tf: &PlotTransform, pos: egui::Pos2) -> bool {
    if let Some((id, index)) = hit::handle_at(layer.items(), tf, pos)
        && layer.selected == Some(id)
    {
        layer.selected = Some(id);
        layer.grab = Some(Grab::Handle { id, index });
        return true;
    }
    if let Some(id) = hit::topmost(layer.items(), tf, pos) {
        let origin = layer.get(id).map(|a| a.geom);
        layer.selected = Some(id);
        layer.grab = origin.map(|origin| Grab::Body {
            id,
            origin,
            from: tf.to_data(pos),
        });
        return layer.grab.is_some();
    }
    layer.selected = None;
    layer.grab = None;
    false
}

pub fn apply_grab(layer: &mut AnnotationLayer, at: DataPos) {
    let Some(grab) = layer.grab else {
        return;
    };
    match grab {
        Grab::Handle { id, index } => {
            if let Some(annot) = layer.get_mut(id) {
                annot.geom.set_handle(index, at);
            }
        }
        Grab::Body { id, origin, from } => {
            let dt = at.t_us.saturating_sub(from.t_us);
            let dy = at.y - from.y;
            if let Some(annot) = layer.get_mut(id) {
                annot.geom = origin.translated(dt, dy);
            }
        }
    }
}

pub fn delete_selected(layer: &mut AnnotationLayer) {
    if let Some(id) = layer.selected {
        layer.remove(id);
    }
}

pub fn on_click(
    layer: &mut AnnotationLayer,
    tf: &PlotTransform,
    pos: egui::Pos2,
    double: bool,
) -> bool {
    let hit = hit::topmost(layer.items(), tf, pos);
    layer.selected = hit;
    if let Some(id) = hit
        && double
    {
        layer.editing = Some(id);
        return true;
    }
    false
}

pub fn interact(
    ui: &egui::Ui,
    response: &egui::Response,
    view: PaneView,
    origin_us: i64,
    layer: &mut AnnotationLayer,
) -> bool {
    let tf = PlotTransform::new(view, origin_us);

    if let Some(pos) = response.hover_pos()
        && tf.rect().contains(pos)
    {
        layer.last_cursor = Some(tf.to_data(pos));
    }

    let mut consumed = layer.grab.is_some();

    if response.drag_started_by(egui::PointerButton::Primary)
        && let Some(pos) = response.interact_pointer_pos()
        && tf.rect().contains(pos)
        && begin_grab(layer, &tf, pos)
    {
        consumed = true;
    }

    if layer.grab.is_some()
        && let Some(pos) = response.interact_pointer_pos()
    {
        apply_grab(layer, tf.to_data(pos));
        consumed = true;
    }

    if response.drag_stopped_by(egui::PointerButton::Primary) {
        layer.grab = None;
    }

    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && tf.rect().contains(pos)
        && on_click(layer, &tf, pos, response.double_clicked())
    {
        consumed = true;
    }

    if response.hovered() && layer.editing.is_none() && !ui.ctx().egui_wants_keyboard_input() {
        if ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            delete_selected(layer);
        } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            layer.selected = None;
        }
    }

    consumed
}
