#[derive(Debug, Clone, PartialEq)]
pub struct PlotInfo {
    pub window: u64,
    pub tile: u64,
    pub index: usize,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlotRequest {
    List { window: Option<u64> },
    Focused,
}
