pub mod draw;
pub mod edit;
pub mod hit;
pub mod interact;

use crate::plotting::gpu::PaneView;

const DEFAULT_SPAN_FRACTION: f64 = 0.12;
const DEFAULT_Y_FRACTION: f64 = 0.15;
const DEFAULT_STROKE_PX: f32 = 1.5;
const DEFAULT_FONT_PX: f32 = 11.0;
const FALLBACK_SPAN_US: i64 = 1_000_000;
const FALLBACK_Y_SPAN: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DataPos {
    pub t_us: i64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Segment,
    Rect,
    Ellipse,
    HLine,
}

impl Kind {
    pub const ALL: [Self; 5] = [
        Self::Text,
        Self::Segment,
        Self::Rect,
        Self::Ellipse,
        Self::HLine,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Segment => "Segment",
            Self::Rect => "Rectangle",
            Self::Ellipse => "Circle",
            Self::HLine => "Limit line",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Geometry {
    Text { at: DataPos },
    Segment { from: DataPos, to: DataPos },
    Rect { a: DataPos, b: DataPos },
    Ellipse { a: DataPos, b: DataPos },
    HLine { y: f64 },
}

impl Geometry {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Text { .. } => Kind::Text,
            Self::Segment { .. } => Kind::Segment,
            Self::Rect { .. } => Kind::Rect,
            Self::Ellipse { .. } => Kind::Ellipse,
            Self::HLine { .. } => Kind::HLine,
        }
    }

    pub fn translated(self, dt_us: i64, dy: f64) -> Self {
        let shift = |p: DataPos| DataPos {
            t_us: p.t_us.saturating_add(dt_us),
            y: p.y + dy,
        };
        match self {
            Self::Text { at } => Self::Text { at: shift(at) },
            Self::Segment { from, to } => Self::Segment {
                from: shift(from),
                to: shift(to),
            },
            Self::Rect { a, b } => Self::Rect {
                a: shift(a),
                b: shift(b),
            },
            Self::Ellipse { a, b } => Self::Ellipse {
                a: shift(a),
                b: shift(b),
            },
            Self::HLine { y } => Self::HLine { y: y + dy },
        }
    }

    pub fn handle_positions(&self) -> Vec<DataPos> {
        match *self {
            Self::Text { .. } | Self::HLine { .. } => Vec::new(),
            Self::Segment { from, to } => vec![from, to],
            Self::Rect { a, b } | Self::Ellipse { a, b } => vec![
                a,
                DataPos { t_us: b.t_us, y: a.y },
                b,
                DataPos { t_us: a.t_us, y: b.y },
            ],
        }
    }

    pub fn set_handle(&mut self, index: usize, p: DataPos) {
        match self {
            Self::Text { .. } | Self::HLine { .. } => {}
            Self::Segment { from, to } => match index {
                0 => *from = p,
                1 => *to = p,
                _ => {}
            },
            Self::Rect { a, b } | Self::Ellipse { a, b } => match index {
                0 => *a = p,
                1 => {
                    b.t_us = p.t_us;
                    a.y = p.y;
                }
                2 => *b = p,
                3 => {
                    a.t_us = p.t_us;
                    b.y = p.y;
                }
                _ => {}
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub color: [f32; 4],
    pub stroke_px: f32,
    pub fill_opacity: f32,
    pub font_px: f32,
    pub arrow: bool,
}

impl Style {
    pub fn color32(&self) -> egui::Color32 {
        let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        egui::Color32::from_rgba_unmultiplied(
            u(self.color[0]),
            u(self.color[1]),
            u(self.color[2]),
            u(self.color[3]),
        )
    }

    pub fn fill32(&self) -> egui::Color32 {
        self.color32().gamma_multiply(self.fill_opacity.clamp(0.0, 1.0))
    }
}

pub fn default_style(id: u64, trace_count: usize) -> Style {
    Style {
        color: delog_render::palette::trace_color(id as usize + trace_count).to_srgb_f32(),
        stroke_px: DEFAULT_STROKE_PX,
        fill_opacity: 0.0,
        font_px: DEFAULT_FONT_PX,
        arrow: false,
    }
}

fn corner_box(at: DataPos, dt: i64, dy: f64) -> (DataPos, DataPos) {
    (
        DataPos {
            t_us: at.t_us.saturating_sub(dt / 2),
            y: at.y - dy / 2.0,
        },
        DataPos {
            t_us: at.t_us.saturating_add(dt / 2),
            y: at.y + dy / 2.0,
        },
    )
}

pub fn default_geometry(kind: Kind, at: DataPos, span_us: i64, y_span: f64) -> Geometry {
    let span_us = if span_us > 0 { span_us } else { FALLBACK_SPAN_US };
    let y_span = if y_span.is_finite() && y_span.abs() > 0.0 {
        y_span.abs()
    } else {
        FALLBACK_Y_SPAN
    };
    let dt = ((span_us as f64 * DEFAULT_SPAN_FRACTION) as i64).max(1);
    let dy = y_span * DEFAULT_Y_FRACTION;
    match kind {
        Kind::Text => Geometry::Text { at },
        Kind::Segment => Geometry::Segment {
            from: at,
            to: DataPos {
                t_us: at.t_us.saturating_add(dt),
                y: at.y + dy,
            },
        },
        Kind::Rect => {
            let (a, b) = corner_box(at, dt, dy);
            Geometry::Rect { a, b }
        }
        Kind::Ellipse => {
            let (a, b) = corner_box(at, dt, dy);
            Geometry::Ellipse { a, b }
        }
        Kind::HLine => Geometry::HLine { y: at.y },
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub id: u64,
    pub geom: Geometry,
    pub label: String,
    pub style: Style,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Grab {
    Body {
        id: u64,
        origin: Geometry,
        from: DataPos,
    },
    Handle {
        id: u64,
        index: usize,
    },
}

impl Grab {
    pub fn id(self) -> u64 {
        match self {
            Self::Body { id, .. } | Self::Handle { id, .. } => id,
        }
    }
}

#[derive(Debug, Default)]
pub struct AnnotationLayer {
    items: Vec<Annotation>,
    next_id: u64,
    pub selected: Option<u64>,
    pub grab: Option<Grab>,
    pub editing: Option<u64>,
    pub last_cursor: Option<DataPos>,
}

impl AnnotationLayer {
    pub fn items(&self) -> &[Annotation] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, id: u64) -> Option<&Annotation> {
        self.items.iter().find(|a| a.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Annotation> {
        self.items.iter_mut().find(|a| a.id == id)
    }

    pub fn add(&mut self, kind: Kind, at: DataPos, span_us: i64, y_span: f64, trace_count: usize) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Annotation {
            id,
            geom: default_geometry(kind, at, span_us, y_span),
            label: String::new(),
            style: default_style(id, trace_count),
        });
        id
    }

    pub fn remove(&mut self, id: u64) {
        self.items.retain(|a| a.id != id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        if self.editing == Some(id) {
            self.editing = None;
        }
        if self.grab.map(Grab::id) == Some(id) {
            self.grab = None;
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PlotTransform {
    rect: egui::Rect,
    origin_us: i64,
    x0: f64,
    x_span: f64,
    y0: f64,
    y_span: f64,
}

impl PlotTransform {
    pub fn new(view: PaneView, origin_us: i64) -> Self {
        let (x0, x1) = view.x_range;
        let (y0, y1) = view.y_range;
        let x_span = if x1 > x0 { (x1 - x0) as f64 } else { 1.0 };
        let y_span = if y1 > y0 { y1 - y0 } else { 1.0 };
        Self {
            rect: view.rect,
            origin_us,
            x0: x0 as f64,
            x_span,
            y0,
            y_span,
        }
    }

    pub fn rect(&self) -> egui::Rect {
        self.rect
    }

    pub fn x_of(&self, t_us: i64) -> f32 {
        let t_sec = (t_us as i128 - self.origin_us as i128) as f64 * 1e-6;
        let frac = (t_sec - self.x0) / self.x_span;
        self.rect.left() + (frac * self.rect.width() as f64) as f32
    }

    pub fn y_of(&self, y: f64) -> f32 {
        let frac = (y - self.y0) / self.y_span;
        self.rect.bottom() - (frac * self.rect.height() as f64) as f32
    }

    pub fn to_screen(self, p: DataPos) -> egui::Pos2 {
        egui::pos2(self.x_of(p.t_us), self.y_of(p.y))
    }

    pub fn to_data(self, pos: egui::Pos2) -> DataPos {
        let x_frac = (pos.x - self.rect.left()) as f64 / self.rect.width().max(1.0) as f64;
        let t_sec = self.x0 + x_frac * self.x_span;
        let y_frac = (self.rect.bottom() - pos.y) as f64 / self.rect.height().max(1.0) as f64;
        DataPos {
            t_us: self.origin_us.saturating_add((t_sec * 1e6).round() as i64),
            y: self.y0 + y_frac * self.y_span,
        }
    }
}

#[cfg(test)]
mod tests;
