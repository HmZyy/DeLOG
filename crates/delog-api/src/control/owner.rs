use std::sync::Arc;

use delog_core::snapshot::StoreSnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptOwner {
    pub name: String,
    pub generation: u64,
}

#[derive(Clone)]
pub struct PlotContext {
    pub owner: Option<ScriptOwner>,
    pub snapshot: Arc<StoreSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenerationRequest {
    Commit { owner: String, generation: u64 },
    Rollback { owner: String, generation: u64 },
}
