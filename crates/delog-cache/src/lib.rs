//! DeLOG render cache layer: per-trace f32 caches (the One Copy, ZC-3),
//! the branching-64 min/max pyramid, and the cache manager with LRU
//! eviction.

pub mod manager;
pub mod pyramid;
pub mod trace;

pub use manager::{CacheManager, DEFAULT_BUDGET_BYTES};
pub use pyramid::{BRANCH, MinMax, MinMaxPyramid};
pub use trace::TraceCache;
