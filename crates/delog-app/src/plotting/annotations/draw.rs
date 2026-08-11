use super::hit;
use super::{Annotation, AnnotationLayer, Geometry, PlotTransform};
use crate::plotting::gpu::PaneView;

const ARROW_HEAD_PX: f32 = 8.0;
const SELECTED_STROKE_BOOST: f32 = 1.0;
const LABEL_PAD_PX: f32 = 3.0;

pub fn is_visible(annot: &Annotation, tf: &PlotTransform) -> bool {
    let bb = match annot.geom {
        Geometry::Text { .. } => hit::text_rect(annot, tf),
        _ => hit::screen_rect(&annot.geom, tf),
    };
    if !bb.is_finite() {
        return false;
    }
    match annot.geom {
        Geometry::HLine { .. } => {
            let y = bb.top();
            y >= tf.rect().top() && y <= tf.rect().bottom()
        }
        _ => tf.rect().intersects(bb.expand(1.0)),
    }
}

pub fn label_anchor(geom: &Geometry, tf: &PlotTransform) -> (egui::Pos2, egui::Align2) {
    match *geom {
        Geometry::Text { at } => (tf.to_screen(at), egui::Align2::LEFT_BOTTOM),
        Geometry::Segment { to, .. } => (
            tf.to_screen(to) + egui::vec2(LABEL_PAD_PX, -LABEL_PAD_PX),
            egui::Align2::LEFT_BOTTOM,
        ),
        Geometry::Rect { .. } | Geometry::Ellipse { .. } => (
            hit::screen_rect(geom, tf).left_top(),
            egui::Align2::LEFT_BOTTOM,
        ),
        Geometry::HLine { y } => (
            egui::pos2(tf.rect().right() - LABEL_PAD_PX, tf.y_of(y) - LABEL_PAD_PX),
            egui::Align2::RIGHT_BOTTOM,
        ),
    }
}

pub fn draw(
    ui: &egui::Ui,
    view: PaneView,
    origin_us: i64,
    layer: &AnnotationLayer,
    preview: Option<Geometry>,
) {
    if layer.is_empty() && preview.is_none() {
        return;
    }
    let tf = PlotTransform::new(view, origin_us);
    let painter = ui.painter().with_clip_rect(tf.rect());
    if let Some(geom) = preview {
        let ghost = Annotation {
            id: u64::MAX,
            geom,
            label: String::new(),
            style: super::default_style(0),
        };
        if is_visible(&ghost, &tf) {
            paint_geometry(&painter, &ghost, &tf, false);
        }
    }
    for annot in layer.items() {
        if !is_visible(annot, &tf) {
            continue;
        }
        let selected = layer.selected == Some(annot.id);
        paint_geometry(&painter, annot, &tf, selected);
        paint_label(&painter, annot, &tf);
        if selected {
            paint_handles(&painter, annot, &tf);
        }
    }
}

fn paint_geometry(painter: &egui::Painter, annot: &Annotation, tf: &PlotTransform, selected: bool) {
    let width = if selected {
        annot.style.stroke_px + SELECTED_STROKE_BOOST
    } else {
        annot.style.stroke_px
    };
    let stroke = egui::Stroke::new(width, annot.style.color32());
    let fill = annot.style.fill32();
    match annot.geom {
        Geometry::Text { .. } => {}
        Geometry::Segment { from, to } => {
            let a = tf.to_screen(from);
            let b = tf.to_screen(to);
            painter.line_segment([a, b], stroke);
            if annot.style.arrow {
                paint_arrow_head(painter, a, b, stroke);
            }
        }
        Geometry::Rect { .. } => {
            let rect = hit::screen_rect(&annot.geom, tf);
            if annot.style.fill_opacity > 0.0 {
                painter.rect_filled(rect, 0.0, fill);
            }
            painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Middle);
        }
        Geometry::Ellipse { .. } => {
            let rect = hit::screen_rect(&annot.geom, tf);
            let clip = tf.rect();
            match ellipse_paint(rect, clip, annot.style.fill_opacity) {
                EllipsePaint::Shape(bounded) => {
                    painter.add(egui::epaint::EllipseShape {
                        center: bounded.center(),
                        radius: bounded.size() / 2.0,
                        fill: if annot.style.fill_opacity > 0.0 {
                            fill
                        } else {
                            egui::Color32::TRANSPARENT
                        },
                        stroke,
                        angle: 0.0,
                    });
                }
                EllipsePaint::FillOnly => {
                    painter.rect_filled(clip, 0.0, fill);
                }
                EllipsePaint::Skip => {}
            }
        }
        Geometry::HLine { y } => {
            painter.hline(tf.rect().x_range(), tf.y_of(y), stroke);
        }
    }
}

fn ellipse_fully_encloses(rect: egui::Rect, clip: egui::Rect) -> bool {
    let center = rect.center();
    let rx = (rect.width() / 2.0).max(f32::EPSILON);
    let ry = (rect.height() / 2.0).max(f32::EPSILON);
    let inside = |p: egui::Pos2| {
        let dx = (p.x - center.x) / rx;
        let dy = (p.y - center.y) / ry;
        dx * dx + dy * dy < 1.0
    };
    [
        clip.left_top(),
        clip.right_top(),
        clip.left_bottom(),
        clip.right_bottom(),
    ]
    .into_iter()
    .all(inside)
}

pub fn clamped_ellipse(rect: egui::Rect, clip: egui::Rect) -> Option<egui::Rect> {
    if ellipse_fully_encloses(rect, clip) {
        return None;
    }
    let bound = clip.expand2(clip.size() * 4.0);
    Some(egui::Rect::from_min_max(
        rect.min.clamp(bound.min, bound.max),
        rect.max.clamp(bound.min, bound.max),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EllipsePaint {
    Shape(egui::Rect),
    FillOnly,
    Skip,
}

pub fn ellipse_paint(rect: egui::Rect, clip: egui::Rect, fill_opacity: f32) -> EllipsePaint {
    match clamped_ellipse(rect, clip) {
        Some(bounded) => EllipsePaint::Shape(bounded),
        None if fill_opacity > 0.0 => EllipsePaint::FillOnly,
        None => EllipsePaint::Skip,
    }
}

fn paint_arrow_head(
    painter: &egui::Painter,
    from: egui::Pos2,
    to: egui::Pos2,
    stroke: egui::Stroke,
) {
    let dir = (to - from).normalized();
    if !dir.x.is_finite() || !dir.y.is_finite() {
        return;
    }
    let rot = egui::emath::Rot2::from_angle(std::f32::consts::TAU / 10.0);
    painter.line_segment([to, to - ARROW_HEAD_PX * (rot * dir)], stroke);
    painter.line_segment([to, to - ARROW_HEAD_PX * (rot.inverse() * dir)], stroke);
}

fn paint_label(painter: &egui::Painter, annot: &Annotation, tf: &PlotTransform) {
    if annot.label.is_empty() {
        return;
    }
    let (pos, align) = label_anchor(&annot.geom, tf);
    painter.text(
        pos,
        align,
        &annot.label,
        egui::FontId::proportional(annot.style.font_px),
        annot.style.color32(),
    );
}

fn paint_handles(painter: &egui::Painter, annot: &Annotation, tf: &PlotTransform) {
    let color = annot.style.color32();
    for handle in hit::handles(&annot.geom, tf) {
        painter.rect_filled(
            egui::Rect::from_center_size(handle, egui::Vec2::splat(hit::HANDLE_PX * 2.0)),
            0.0,
            color,
        );
    }
}
