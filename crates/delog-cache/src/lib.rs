//! DeLOG render cache layer: per-trace f32 caches (the One Copy, ZC-3),
//! the branching-64 min/max pyramid, and the cache manager with LRU
//! eviction.

pub mod pyramid;

pub use pyramid::{BRANCH, MinMax, MinMaxPyramid};
