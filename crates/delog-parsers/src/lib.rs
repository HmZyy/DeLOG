//! DeLOG parsers: format sniffing and the ULog / ArduPilot BIN / tlog
//! parsers.
//!
//! Dependency rule (PLAN.md §3.2): parsers never see GPU or UI. Their only
//! output is `ParsedBatch` + diagnostics into an `IngestSink`.
