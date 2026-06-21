//! Render cache layer: per-trace f32 caches, min/max pyramid, cache manager.

pub mod manager;
pub mod pyramid;
pub mod trace;

pub use manager::{CacheManager, DEFAULT_BUDGET_BYTES};
pub use pyramid::{BRANCH, MinMax, MinMaxPyramid};
pub use trace::TraceCache;
