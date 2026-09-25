use super::ResourceOwner;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    pub owner: Option<ResourceOwner>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceInfo {
    pub scene_visible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackInfo {
    pub speed: f64,
    pub follow_live: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

impl SplitDirection {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "horizontal" => Ok(Self::Horizontal),
            "vertical" => Ok(Self::Vertical),
            _ => Err(Error::invalid_input(format!(
                "split direction must be 'horizontal' or 'vertical', got {name:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceRequest {
    ListWindows,
    GetState,
    AddPlot {
        window: Option<u64>,
        direction: SplitDirection,
        owner: Option<ResourceOwner>,
    },
    Split {
        window: u64,
        tile: u64,
        direction: SplitDirection,
        owner: Option<ResourceOwner>,
    },
    Close {
        window: u64,
        tile: u64,
    },
    Equalize {
        window: Option<u64>,
    },
    ShowScene {
        visible: bool,
    },
    OpenWindow {
        title: Option<String>,
        owner: Option<ResourceOwner>,
    },
}

impl WorkspaceRequest {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::OpenWindow { title, owner } => {
                if title.as_ref().is_some_and(|title| title.trim().is_empty()) {
                    return Err(Error::invalid_input("window title must not be empty"));
                }
                if let Some(owner) = owner {
                    owner.validate()?;
                }
                Ok(())
            }
            Self::AddPlot {
                window: _,
                direction: _,
                owner,
            }
            | Self::Split {
                window: _,
                tile: _,
                direction: _,
                owner,
            } => {
                if let Some(owner) = owner {
                    owner.validate()?;
                }
                Ok(())
            }
            Self::Close { window: _, tile: _ }
            | Self::Equalize { window: _ }
            | Self::ShowScene { visible: _ }
            | Self::ListWindows
            | Self::GetState => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackRequest {
    Get,
    Set {
        speed: Option<f64>,
        follow_live: Option<bool>,
    },
}

impl PlaybackRequest {
    pub fn set(speed: Option<f64>, follow_live: Option<bool>) -> Result<Self> {
        let request = Self::Set { speed, follow_live };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<()> {
        let speed = match self {
            Self::Get => return Ok(()),
            Self::Set { speed, .. } => speed,
        };
        if let Some(speed) = speed
            && !speed.is_finite()
        {
            return Err(Error::invalid_input(format!(
                "playback speed must be finite, got {speed}"
            )));
        }
        Ok(())
    }
}
