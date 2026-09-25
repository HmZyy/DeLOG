use super::ResourceOwner;

#[derive(Debug, Clone, PartialEq)]
pub struct PlotInfo {
    pub window: u64,
    pub tile: u64,
    pub instance_id: u64,
    pub index: usize,
    pub label: String,
    pub owner: Option<ResourceOwner>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlotRequest {
    List { window: Option<u64> },
    Focused,
}
