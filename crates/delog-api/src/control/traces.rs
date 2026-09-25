use delog_core::identity::FieldId;

use super::ResourceOwner;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceMode {
    Line,
    Scatter,
    Step,
}

impl TraceMode {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "line" => Ok(Self::Line),
            "scatter" => Ok(Self::Scatter),
            "step" => Ok(Self::Step),
            _ => Err(Error::invalid_input(format!(
                "trace mode must be 'line', 'scatter', or 'step', got {name:?}"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Scatter => "scatter",
            Self::Step => "step",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceInfo {
    pub instance_id: u64,
    pub index: usize,
    pub field_id: FieldId,
    pub field: String,
    pub color: [f32; 4],
    pub width_px: f32,
    pub mode: TraceMode,
    pub visible: bool,
    pub owner: Option<ResourceOwner>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TraceRequest {
    List {
        window: u64,
        tile: u64,
    },
    Add {
        window: u64,
        tile: u64,
        field_id: FieldId,
        field: String,
        color: Option<[f32; 4]>,
        width_px: Option<f32>,
        mode: TraceMode,
        owner: Option<ResourceOwner>,
    },
    AddReturning {
        window: u64,
        tile: u64,
        field_id: FieldId,
        field: String,
        color: Option<[f32; 4]>,
        width_px: Option<f32>,
        mode: TraceMode,
        owner: Option<ResourceOwner>,
    },
    Remove {
        window: u64,
        tile: u64,
        index: Option<usize>,
        field_id: Option<FieldId>,
        field: Option<String>,
    },
    Clear {
        window: u64,
        tile: u64,
    },
    ClearOwned {
        window: u64,
        tile: u64,
        owner: String,
    },
    Set {
        window: u64,
        tile: u64,
        index: usize,
        field_id: FieldId,
        color: Option<[f32; 4]>,
        width_px: Option<f32>,
        mode: Option<TraceMode>,
        visible: Option<bool>,
    },
}

impl TraceRequest {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::List { window: _, tile: _ } | Self::Clear { window: _, tile: _ } => Ok(()),
            Self::ClearOwned {
                window: _,
                tile: _,
                owner,
            } => {
                if owner.is_empty() {
                    Err(Error::invalid_input("trace owner must not be empty"))
                } else {
                    Ok(())
                }
            }
            Self::Add {
                window: _,
                tile: _,
                field_id: _,
                field,
                color,
                width_px,
                mode: _,
                owner,
            }
            | Self::AddReturning {
                window: _,
                tile: _,
                field_id: _,
                field,
                color,
                width_px,
                mode: _,
                owner,
            } => {
                validate_field(field)?;
                validate_color(*color)?;
                validate_width(*width_px)?;
                if let Some(owner) = owner {
                    owner.validate()?;
                }
                Ok(())
            }
            Self::Remove {
                window: _,
                tile: _,
                index,
                field_id,
                field,
            } => {
                if index.is_some() == field_id.is_some() || field_id.is_some() != field.is_some() {
                    return Err(Error::invalid_input(
                        "remove() needs exactly one of a position or field=",
                    ));
                }
                if let Some(field) = field {
                    validate_field(field)?;
                }
                Ok(())
            }
            Self::Set {
                window: _,
                tile: _,
                index: _,
                field_id: _,
                color,
                width_px,
                mode: _,
                visible: _,
            } => {
                validate_color(*color)?;
                validate_width(*width_px)
            }
        }
    }

    pub fn remove(
        window: u64,
        tile: u64,
        index: Option<usize>,
        field: Option<(FieldId, String)>,
    ) -> Result<Self> {
        if index.is_some() == field.is_some() {
            return Err(Error::invalid_input(
                "remove() needs exactly one of a position or field=",
            ));
        }
        let (field_id, field) = match field {
            Some((id, path)) => (Some(id), Some(path)),
            None => (None, None),
        };
        Ok(Self::Remove {
            window,
            tile,
            index,
            field_id,
            field,
        })
    }
}

fn validate_field(field: &str) -> Result<()> {
    if field.is_empty() {
        Err(Error::invalid_input("trace field path must not be empty"))
    } else {
        Ok(())
    }
}

fn validate_color(color: Option<[f32; 4]>) -> Result<()> {
    if color.is_some_and(|color| {
        !color
            .iter()
            .all(|component| component.is_finite() && (0.0..=1.0).contains(component))
    }) {
        Err(Error::invalid_input(
            "trace color components must be finite and between 0 and 1",
        ))
    } else {
        Ok(())
    }
}

fn validate_width(width: Option<f32>) -> Result<()> {
    if width.is_some_and(|width| !width.is_finite() || width <= 0.0) {
        Err(Error::invalid_input(
            "trace width_px must be finite and > 0",
        ))
    } else {
        Ok(())
    }
}
