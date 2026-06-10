//! DeLOG core: IDs & keys, time model, columnar store, snapshots, chunk
//! stats, diagnostics types and the metrics registry.
//!
//! Dependency rule (PLAN.md §3.2): this crate depends on `arrow` and std
//! only. Nothing here may know about parsers, GPU, or UI.

pub mod identity;
pub mod metrics;
pub mod time;
