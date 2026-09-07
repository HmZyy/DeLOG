use super::{Annotation, Geometry, PlotTransform};

pub const TOLERANCE_PX: f32 = 5.0;
pub const HANDLE_PX: f32 = 4.0;

pub fn handles(geom: &Geometry, tf: &PlotTransform) -> Vec<egui::Pos2> {
    geom.handle_positions()
        .into_iter()
        .map(|p| tf.to_screen(p))
        .collect()
}

pub fn handle_at(
    items: &[Annotation],
    tf: &PlotTransform,
    pos: egui::Pos2,
) -> Option<(u64, usize)> {
    let grab = HANDLE_PX + TOLERANCE_PX;
    items.iter().rev().find_map(|a| {
        handles(&a.geom, tf)
            .into_iter()
            .position(|h| (h - pos).length() <= grab)
            .map(|index| (a.id, index))
    })
}

pub fn topmost(items: &[Annotation], tf: &PlotTransform, pos: egui::Pos2) -> Option<u64> {
    items
        .iter()
        .rev()
        .find(|a| contains(a, tf, pos))
        .map(|a| a.id)
}

pub fn text_rect(annot: &Annotation, tf: &PlotTransform) -> egui::Rect {
    let Geometry::Text { at } = annot.geom else {
        return egui::Rect::NOTHING;
    };
    if annot.label.is_empty() {
        return egui::Rect::NOTHING;
    }
    let anchor = tf.to_screen(at);
    let width = annot.label.chars().count() as f32 * annot.style.font_px * 0.5;
    let height = annot.style.font_px * 1.2;
    egui::Rect::from_min_max(
        egui::pos2(anchor.x, anchor.y - height),
        egui::pos2(anchor.x + width, anchor.y),
    )
}

pub fn screen_rect(geom: &Geometry, tf: &PlotTransform) -> egui::Rect {
    match *geom {
        Geometry::Text { at } => egui::Rect::from_center_size(tf.to_screen(at), egui::Vec2::ZERO),
        Geometry::Segment { from, to } => {
            egui::Rect::from_two_pos(tf.to_screen(from), tf.to_screen(to))
        }
        Geometry::Rect { a, b } | Geometry::Ellipse { a, b } => {
            egui::Rect::from_two_pos(tf.to_screen(a), tf.to_screen(b))
        }
        Geometry::HLine { y } => {
            let line_y = tf.y_of(y);
            egui::Rect::from_min_max(
                egui::pos2(tf.rect().left(), line_y),
                egui::pos2(tf.rect().right(), line_y),
            )
        }
    }
}

pub fn contains(annot: &Annotation, tf: &PlotTransform, pos: egui::Pos2) -> bool {
    let filled = annot.style.fill_opacity > 0.0;
    match annot.geom {
        Geometry::Text { .. } => text_rect(annot, tf).expand(TOLERANCE_PX).contains(pos),
        Geometry::Segment { from, to } => {
            distance_to_segment(pos, tf.to_screen(from), tf.to_screen(to)) <= TOLERANCE_PX
        }
        Geometry::Rect { .. } => near_rect(screen_rect(&annot.geom, tf), pos, filled),
        Geometry::Ellipse { .. } => near_ellipse(screen_rect(&annot.geom, tf), pos, filled),
        Geometry::HLine { y } => {
            let rect = tf.rect();
            (pos.y - tf.y_of(y)).abs() <= TOLERANCE_PX
                && pos.x >= rect.left() - TOLERANCE_PX
                && pos.x <= rect.right() + TOLERANCE_PX
        }
    }
}

fn near_rect(rect: egui::Rect, pos: egui::Pos2, filled: bool) -> bool {
    if !rect.expand(TOLERANCE_PX).contains(pos) {
        return false;
    }
    filled || !rect.shrink(TOLERANCE_PX).contains(pos)
}

fn near_ellipse(rect: egui::Rect, pos: egui::Pos2, filled: bool) -> bool {
    let center = rect.center();
    let rx = (rect.width() / 2.0).max(0.5);
    let ry = (rect.height() / 2.0).max(0.5);
    let dx = (pos.x - center.x) / rx;
    let dy = (pos.y - center.y) / ry;
    let normalized = (dx * dx + dy * dy).sqrt();
    if filled && normalized <= 1.0 {
        return true;
    }
    (normalized - 1.0).abs() * rx.min(ry) <= TOLERANCE_PX
}

fn distance_to_segment(pos: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= f32::EPSILON {
        return (pos - a).length();
    }
    let t = (((pos - a).dot(ab)) / len_sq).clamp(0.0, 1.0);
    (pos - (a + t * ab)).length()
}
