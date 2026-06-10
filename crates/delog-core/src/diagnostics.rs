//! Diagnostic records emitted across the pipeline (PLAN.md §15).
//!
//! The full diagnostics hub (ring buffer, dedup, dock UI) is M9 (DIA-01); this
//! is the minimal record type that parser/ingest emitters need from M2 onward
//! (DIA-04 notes the emitters land earlier than the hub).

use crate::identity::SourceId;
use crate::time::TimestampUs;

/// Diagnostic severity, ordered so filters can compare (`>= Warning`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// One diagnostic event. `code` is a stable machine-readable tag (e.g.
/// `"timestamp-regression"`) used for burst dedup (§15); `message` is the
/// human-facing text. Time/byte-offset locate the event when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub severity: Severity,
    pub code: &'static str,
    pub source: Option<SourceId>,
    pub time_us: Option<TimestampUs>,
    pub byte_offset: Option<u64>,
    pub message: String,
}

impl Diag {
    pub fn new(severity: Severity, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            source: None,
            time_us: None,
            byte_offset: None,
            message: message.into(),
        }
    }

    pub fn info(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Severity::Info, code, message)
    }

    pub fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, message)
    }

    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, message)
    }

    pub fn with_source(mut self, source: SourceId) -> Self {
        self.source = Some(source);
        self
    }

    pub fn at_time(mut self, time_us: TimestampUs) -> Self {
        self.time_us = Some(time_us);
        self
    }

    pub fn at_byte(mut self, byte_offset: u64) -> Self {
        self.byte_offset = Some(byte_offset);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_info_below_warning_below_error() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
    }

    #[test]
    fn builders_attach_location_metadata() {
        let diag = Diag::warning("timestamp-regression", "t went backwards")
            .with_source(SourceId(3))
            .at_time(1_000)
            .at_byte(42);
        assert_eq!(diag.severity, Severity::Warning);
        assert_eq!(diag.code, "timestamp-regression");
        assert_eq!(diag.source, Some(SourceId(3)));
        assert_eq!(diag.time_us, Some(1_000));
        assert_eq!(diag.byte_offset, Some(42));
    }
}
