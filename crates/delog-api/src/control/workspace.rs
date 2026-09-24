use crate::{Error, Result};

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
    AddPlot {
        direction: SplitDirection,
    },
    Split {
        window: u64,
        tile: u64,
        direction: SplitDirection,
    },
    Close {
        window: u64,
        tile: u64,
    },
    Equalize,
    ShowScene {
        visible: bool,
    },
    OpenWindow {
        title: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackRequest {
    Set {
        speed: Option<f64>,
        follow_live: Option<bool>,
    },
}

impl PlaybackRequest {
    pub fn set(speed: Option<f64>, follow_live: Option<bool>) -> Result<Self> {
        if let Some(speed) = speed
            && !speed.is_finite()
        {
            return Err(Error::invalid_input(format!(
                "playback speed must be finite, got {speed}"
            )));
        }
        Ok(Self::Set { speed, follow_live })
    }
}
