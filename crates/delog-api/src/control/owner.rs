use std::sync::Arc;

use crate::{Error, Result};
use delog_core::snapshot::StoreSnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceOwner {
    pub name: String,
    pub generation: u64,
}

pub type ScriptOwner = ResourceOwner;

impl ResourceOwner {
    pub fn validate(&self) -> Result<()> {
        let Self {
            name,
            generation: _,
        } = self;
        if name.is_empty() {
            Err(Error::invalid_input("resource owner must not be empty"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub struct PlotContext {
    pub owner: Option<ResourceOwner>,
    pub snapshot: Arc<StoreSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenerationRequest {
    Commit { owner: String, generation: u64 },
    Rollback { owner: String, generation: u64 },
    RemoveOwned { owner: String },
}

impl GenerationRequest {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Commit {
                owner,
                generation: _,
            }
            | Self::Rollback {
                owner,
                generation: _,
            }
            | Self::RemoveOwned { owner } => {
                if owner.is_empty() {
                    Err(Error::invalid_input("generation owner must not be empty"))
                } else {
                    Ok(())
                }
            }
        }
    }
}
