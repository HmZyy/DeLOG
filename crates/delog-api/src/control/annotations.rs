use super::ResourceOwner;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationKind {
    Text,
    Segment,
    Rect,
    Ellipse,
    HLine,
}

impl AnnotationKind {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "text" => Ok(Self::Text),
            "segment" => Ok(Self::Segment),
            "rect" => Ok(Self::Rect),
            "ellipse" => Ok(Self::Ellipse),
            "hline" => Ok(Self::HLine),
            _ => Err(Error::invalid_input(format!(
                "annotation kind must be 'text', 'segment', 'rect', 'ellipse', or 'hline', got {name:?}"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Segment => "segment",
            Self::Rect => "rect",
            Self::Ellipse => "ellipse",
            Self::HLine => "hline",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationGeometry {
    Text { at: (i64, f64) },
    Segment { from: (i64, f64), to: (i64, f64) },
    Rect { a: (i64, f64), b: (i64, f64) },
    Ellipse { a: (i64, f64), b: (i64, f64) },
    HLine { y: f64 },
}

impl AnnotationGeometry {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Text { at } => validate_point(*at, "at"),
            Self::Segment { from, to } => {
                validate_point(*from, "from")?;
                validate_point(*to, "to")
            }
            Self::Rect { a, b } | Self::Ellipse { a, b } => {
                validate_point(*a, "a")?;
                validate_point(*b, "b")
            }
            Self::HLine { y } => validate_f64(*y, "y"),
        }
    }

    pub fn text(at: (i64, f64)) -> Result<Self> {
        validate_point(at, "at")?;
        Ok(Self::Text { at })
    }

    pub fn segment(from: (i64, f64), to: (i64, f64)) -> Result<Self> {
        validate_point(from, "from")?;
        validate_point(to, "to")?;
        Ok(Self::Segment { from, to })
    }

    pub fn rect(a: (i64, f64), b: (i64, f64)) -> Result<Self> {
        validate_point(a, "a")?;
        validate_point(b, "b")?;
        Ok(Self::Rect { a, b })
    }

    pub fn ellipse(a: (i64, f64), b: (i64, f64)) -> Result<Self> {
        validate_point(a, "a")?;
        validate_point(b, "b")?;
        Ok(Self::Ellipse { a, b })
    }

    pub fn hline(y: f64) -> Result<Self> {
        validate_f64(y, "y")?;
        Ok(Self::HLine { y })
    }

    pub fn kind(&self) -> AnnotationKind {
        match self {
            Self::Text { .. } => AnnotationKind::Text,
            Self::Segment { .. } => AnnotationKind::Segment,
            Self::Rect { .. } => AnnotationKind::Rect,
            Self::Ellipse { .. } => AnnotationKind::Ellipse,
            Self::HLine { .. } => AnnotationKind::HLine,
        }
    }

    pub fn moved_to(self, point: (i64, f64)) -> Result<Self> {
        validate_point(point, "point")?;
        let delta = |anchor: (i64, f64)| (point.0 - anchor.0, point.1 - anchor.1);
        let shift =
            |position: (i64, f64), delta: (i64, f64)| (position.0 + delta.0, position.1 + delta.1);
        Ok(match self {
            Self::Text { .. } => Self::Text { at: point },
            Self::Segment { from, to } => {
                let delta = delta(from);
                Self::Segment {
                    from: point,
                    to: shift(to, delta),
                }
            }
            Self::Rect { a, b } => {
                let delta = delta(a);
                Self::Rect {
                    a: point,
                    b: shift(b, delta),
                }
            }
            Self::Ellipse { a, b } => {
                let delta = delta(a);
                Self::Ellipse {
                    a: point,
                    b: shift(b, delta),
                }
            }
            Self::HLine { y } => Self::HLine { y },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AnnotationStylePatch {
    pub color: Option<[f32; 4]>,
    pub stroke_px: Option<f32>,
    pub fill_opacity: Option<f32>,
    pub font_px: Option<f32>,
    pub arrow: Option<bool>,
}

impl AnnotationStylePatch {
    pub fn new(
        color: Option<[f32; 4]>,
        stroke_px: Option<f32>,
        fill_opacity: Option<f32>,
        font_px: Option<f32>,
        arrow: Option<bool>,
    ) -> Result<Self> {
        let patch = Self {
            color,
            stroke_px,
            fill_opacity,
            font_px,
            arrow,
        };
        patch.validate()?;
        Ok(patch)
    }

    pub fn validate(&self) -> Result<()> {
        let Self {
            color,
            stroke_px,
            fill_opacity,
            font_px,
            arrow: _,
        } = self;
        if let Some(color) = color
            && !color
                .iter()
                .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
        {
            return Err(Error::invalid_input(
                "annotation color components must be finite and between 0 and 1",
            ));
        }
        validate_f32(*stroke_px, "stroke_px")?;
        validate_f32(*fill_opacity, "fill_opacity")?;
        validate_f32(*font_px, "font_px")?;
        if stroke_px.is_some_and(|value| value <= 0.0) {
            return Err(Error::invalid_input("stroke_px must be > 0"));
        }
        if fill_opacity.is_some_and(|value| !(0.0..=1.0).contains(&value)) {
            return Err(Error::invalid_input("fill_opacity must be between 0 and 1"));
        }
        if font_px.is_some_and(|value| value <= 0.0) {
            return Err(Error::invalid_input("font_px must be > 0"));
        }
        Ok(())
    }
}

fn validate_point(point: (i64, f64), name: &str) -> Result<()> {
    validate_f64(point.1, &format!("{name}.y"))
}

fn validate_f64(value: f64, name: &str) -> Result<()> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(Error::invalid_input(format!("{name} must be finite")))
    }
}

fn validate_f32(value: Option<f32>, name: &str) -> Result<()> {
    match value {
        Some(value) if !value.is_finite() => {
            Err(Error::invalid_input(format!("{name} must be finite")))
        }
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationFilter {
    Index(usize),
    Id(u64),
    Kind(AnnotationKind),
    Label(String),
    Owner(String),
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationInfo {
    pub plot_instance_id: u64,
    pub window: u64,
    pub tile: u64,
    pub id: u64,
    pub index: usize,
    pub kind: AnnotationKind,
    pub geometry: AnnotationGeometry,
    pub label: String,
    pub color: [f32; 4],
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationRequest {
    Add {
        window: u64,
        tile: u64,
        geometry: AnnotationGeometry,
        label: String,
        style: AnnotationStylePatch,
        owner: Option<ResourceOwner>,
    },
    List {
        target: Option<(u64, u64)>,
    },
    Remove {
        target: Option<(u64, u64)>,
        filter: AnnotationFilter,
    },
    Set {
        window: u64,
        tile: u64,
        id: u64,
        label: Option<String>,
        geometry: Option<AnnotationGeometry>,
        style: AnnotationStylePatch,
    },
}

impl AnnotationRequest {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Add {
                window: _,
                tile: _,
                geometry,
                label: _,
                style,
                owner,
            } => {
                geometry.validate()?;
                style.validate()?;
                if let Some(owner) = owner {
                    owner.validate()?;
                }
                Ok(())
            }
            Self::List { target: _ } => Ok(()),
            Self::Remove { target: _, filter } => match filter {
                AnnotationFilter::Owner(owner) if owner.is_empty() => {
                    Err(Error::invalid_input("annotation owner must not be empty"))
                }
                AnnotationFilter::Index(_)
                | AnnotationFilter::Id(_)
                | AnnotationFilter::Kind(_)
                | AnnotationFilter::Label(_)
                | AnnotationFilter::Owner(_)
                | AnnotationFilter::All => Ok(()),
            },
            Self::Set {
                window: _,
                tile: _,
                id: _,
                label: _,
                geometry,
                style,
            } => {
                if let Some(geometry) = geometry {
                    geometry.validate()?;
                }
                style.validate()
            }
        }
    }
}
