//! DeLOG core data model. Dependency rule: nothing here may depend on parsers,
//! GPU, or UI.

pub mod align;
pub mod analysis;
pub mod chunk;
pub mod derived;
pub mod diagnostics;
pub mod export;
pub mod field_view;
pub mod identity;
pub mod ingest;
pub mod ingestor;
pub mod mem;
pub mod metrics;
pub mod parse_ctl;
pub mod quality;
pub mod schema;
pub mod snapshot;
pub mod store;
pub mod time;
