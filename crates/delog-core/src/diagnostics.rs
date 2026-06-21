//! Diagnostic records and hub emitted across the pipeline.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};

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
/// `"timestamp-regression"`) used for burst dedup; `message` is the
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

/// A retained diagnostic row. Repeated burst-equivalent diagnostics collapse
/// into one row by incrementing `count`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagRecord {
    pub seq: u64,
    pub diag: Diag,
    pub count: u64,
}

/// Central diagnostics hub: mpsc ingress, fixed-size ring retention and
/// last-row burst deduplication.
#[derive(Debug)]
pub struct DiagnosticHub {
    tx: Sender<Diag>,
    rx: Mutex<Receiver<Diag>>,
    state: Mutex<HubState>,
}

#[derive(Debug)]
struct HubState {
    ring: VecDeque<DiagRecord>,
    cap: usize,
    next_seq: u64,
}

impl DiagnosticHub {
    pub const DEFAULT_CAPACITY: usize = 10_000;

    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "diagnostic hub capacity must be non-zero");
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx: Mutex::new(rx),
            state: Mutex::new(HubState {
                ring: VecDeque::with_capacity(capacity.min(1024)),
                cap: capacity,
                next_seq: 0,
            }),
        }
    }

    /// Sender for worker threads that should not lock the ring directly.
    pub fn sender(&self) -> Sender<Diag> {
        self.tx.clone()
    }

    /// Best-effort enqueue through the mpsc ingress.
    pub fn emit(&self, diag: Diag) {
        let _ = self.tx.send(diag);
    }

    /// Drain queued diagnostics into the retained ring and return how many
    /// incoming events were consumed.
    pub fn drain(&self) -> usize {
        let mut drained = 0;
        let rx = self.rx.lock().unwrap();
        while let Ok(diag) = rx.try_recv() {
            self.push_locked(diag);
            drained += 1;
        }
        drained
    }

    /// Snapshot retained diagnostics after draining queued events.
    pub fn snapshot(&self) -> Vec<DiagRecord> {
        self.drain();
        self.state.lock().unwrap().ring.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.drain();
        self.state.lock().unwrap().ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.drain();
        self.state.lock().unwrap().ring.clear();
    }

    fn push_locked(&self, diag: Diag) {
        let mut state = self.state.lock().unwrap();
        if let Some(last) = state.ring.back_mut()
            && same_burst(&last.diag, &diag)
        {
            last.count = last.count.saturating_add(1);
            return;
        }

        let seq = state.next_seq;
        state.next_seq = state.next_seq.saturating_add(1);
        if state.ring.len() == state.cap {
            state.ring.pop_front();
        }
        state.ring.push_back(DiagRecord {
            seq,
            diag,
            count: 1,
        });
    }
}

impl Default for DiagnosticHub {
    fn default() -> Self {
        Self::new()
    }
}

fn same_burst(a: &Diag, b: &Diag) -> bool {
    a.severity == b.severity
        && a.code == b.code
        && a.source == b.source
        && a.time_us == b.time_us
        && a.byte_offset == b.byte_offset
        && a.message == b.message
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

    #[test]
    fn hub_dedups_adjacent_bursts_by_count() {
        let hub = DiagnosticHub::with_capacity(8);
        for _ in 0..3 {
            hub.emit(Diag::warning("drop", "live batch dropped").with_source(SourceId(1)));
        }
        hub.emit(Diag::warning("other", "different"));

        let records = hub.snapshot();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].count, 3);
        assert_eq!(records[0].diag.code, "drop");
        assert_eq!(records[1].count, 1);
    }

    #[test]
    fn hub_ring_drops_oldest_records_over_capacity() {
        let hub = DiagnosticHub::with_capacity(2);
        hub.emit(Diag::info("a", "a"));
        hub.emit(Diag::info("b", "b"));
        hub.emit(Diag::info("c", "c"));

        let records = hub.snapshot();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].diag.code, "b");
        assert_eq!(records[1].diag.code, "c");
    }

    #[test]
    fn hub_clear_removes_retained_records() {
        let hub = DiagnosticHub::with_capacity(2);
        hub.emit(Diag::error("x", "x"));
        assert!(!hub.snapshot().is_empty());
        hub.clear();
        assert!(hub.snapshot().is_empty());
    }
}
