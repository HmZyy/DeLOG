//! DeLOG parsers.
//!
//! Dependency rule: parsers never depend on GPU or UI; their only output is
//! `ParsedBatch` + diagnostics into an `IngestSink`.

pub mod ardupilot;
pub mod mavlink;
pub mod parquet;
pub mod parser;
pub mod tlog;
pub mod ulog;

pub use ardupilot::ArduPilotParser;
pub use parquet::ParquetParser;
pub use parser::{
    Candidate, Detection, LogParser, ParseError, ParserRegistry, ReadSeek, SNIFF_CONFIDENCE,
    SNIFF_HEAD_LEN, Sniff,
};
pub use tlog::TlogParser;
pub use ulog::ULogParser;
