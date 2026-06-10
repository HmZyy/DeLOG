//! DeLOG parsers: format sniffing and the ULog / ArduPilot BIN / tlog
//! parsers.
//!
//! Dependency rule (PLAN.md §3.2): parsers never see GPU or UI. Their only
//! output is `ParsedBatch` + diagnostics into an `IngestSink`.

pub mod parser;

pub use parser::{
    Candidate, Detection, LogParser, ParseError, ParserRegistry, ReadSeek, SNIFF_CONFIDENCE,
    SNIFF_HEAD_LEN, Sniff,
};
