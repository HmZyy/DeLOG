//! DeLOG core: IDs & keys, time model, columnar store, snapshots, chunk
//! stats, diagnostics types and the metrics registry.
//!
//! Dependency rule (PLAN.md §3.2): nothing here may know about parsers, GPU,
//! or UI.

pub mod chunk;
pub mod diagnostics;
pub mod field_view;
pub mod identity;
pub mod ingest;
pub mod ingestor;
pub mod mem;
pub mod metrics;
pub mod parse_ctl;
pub mod schema;
pub mod snapshot;
pub mod store;
pub mod time;
