#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

impl SplitDirection {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "horizontal" => Some(Self::Horizontal),
            "vertical" => Some(Self::Vertical),
            _ => None,
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
