//! DeLOG live streaming: MAVLink link backends (UDP/TCP/serial), the link
//! state machine, message→field extraction and the raw-frame recorder.
//!
//! Dependency rule (PLAN.md §3.2): like parsers, this crate never sees GPU
//! or UI; live batches feed the same `IngestSink` path as files.
