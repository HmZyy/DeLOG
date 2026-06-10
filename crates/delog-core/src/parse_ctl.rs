//! Per-parse control: cancellation and byte-based progress (PLAN.md §5, ING-04).
//!
//! A parser receives a `&ParseCtl` alongside its [`IngestSink`] (§6.1). It
//! polls cancellation cheaply — the `Arc<AtomicBool>` is read at most once every
//! [`CANCEL_POLL_INTERVAL`] records, so the common path is a counter compare,
//! not an atomic load — and reports progress as a byte fraction, throttled so a
//! multi-GB parse emits ~100 events, not millions.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::identity::SourceId;
use crate::ingest::IngestSink;

/// Records between cancellation polls (§5). Coarse enough to be free per record,
/// fine enough to stay responsive on a fast parse.
pub const CANCEL_POLL_INTERVAL: u64 = 4096;

/// Smallest progress advance worth an event (1% of the file).
const PROGRESS_EPSILON: f32 = 0.01;

/// A cancellation flag shared between the UI (which sets it) and a parser thread
/// (which polls it). Cloning shares the same underlying flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. The parser observes this on its next poll boundary.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

/// Cancellation + byte-progress for one parse run.
///
/// The progress throttle uses interior mutability so the whole control can be
/// passed as `&ParseCtl` (matching the parser trait, §6.1); a `ParseCtl` belongs
/// to a single parser thread and is not shared, so a `Cell` is sufficient.
#[derive(Debug)]
pub struct ParseCtl {
    cancel: CancelToken,
    /// Total source bytes, or 0 when unknown (e.g. a live stream).
    total_bytes: u64,
    last_reported: Cell<f32>,
}

impl ParseCtl {
    pub fn new(cancel: CancelToken, total_bytes: u64) -> Self {
        Self {
            cancel,
            total_bytes,
            last_reported: Cell::new(0.0),
        }
    }

    /// Force-read the cancellation flag. Prefer [`cancelled_at`](Self::cancelled_at)
    /// inside hot record loops.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Cheap per-record cancellation check: only touches the atomic on a
    /// [`CANCEL_POLL_INTERVAL`] boundary, returning `false` otherwise.
    pub fn cancelled_at(&self, record_index: u64) -> bool {
        record_index.is_multiple_of(CANCEL_POLL_INTERVAL) && self.cancel.is_cancelled()
    }

    /// Progress fraction in `0.0..=1.0` for `bytes_read`, or `0.0` if the total
    /// is unknown.
    pub fn fraction(&self, bytes_read: u64) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (bytes_read as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
    }

    /// Emit a progress event to `sink` only if the fraction advanced at least
    /// [`PROGRESS_EPSILON`] since the last report (or first reached completion),
    /// keeping the event stream sparse.
    pub fn report_progress(&self, sink: &mut dyn IngestSink, source: SourceId, bytes_read: u64) {
        if self.total_bytes == 0 {
            return;
        }
        let frac = self.fraction(bytes_read);
        let last = self.last_reported.get();
        let completed = frac >= 1.0 && last < 1.0;
        if completed || frac - last >= PROGRESS_EPSILON {
            self.last_reported.set(frac);
            sink.progress(source, frac);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{ParseSummary, ParsedBatch, SourceKind};

    #[derive(Default)]
    struct ProgressSink {
        events: Vec<f32>,
    }
    impl IngestSink for ProgressSink {
        fn open_source(&mut self, _key: &str, _kind: SourceKind) -> SourceId {
            SourceId(0)
        }
        fn submit(&mut self, _batch: ParsedBatch) {}
        fn diagnostic(&mut self, _diag: crate::diagnostics::Diag) {}
        fn progress(&mut self, _source: SourceId, frac: f32) {
            self.events.push(frac);
        }
        fn close_source(&mut self, _source: SourceId, _summary: ParseSummary) {}
    }

    #[test]
    fn cancel_token_is_shared_across_clones() {
        let token = CancelToken::new();
        let remote = token.clone();
        assert!(!token.is_cancelled());
        remote.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancellation_is_only_observed_on_poll_boundaries() {
        let token = CancelToken::new();
        let ctl = ParseCtl::new(token.clone(), 1_000);
        token.cancel();

        // Off-boundary indices ignore the flag; the boundary observes it.
        assert!(!ctl.cancelled_at(1));
        assert!(!ctl.cancelled_at(CANCEL_POLL_INTERVAL - 1));
        assert!(ctl.cancelled_at(CANCEL_POLL_INTERVAL));
        assert!(ctl.cancelled_at(0));
        // The unconditional check always sees it.
        assert!(ctl.is_cancelled());
    }

    #[test]
    fn fraction_clamps_and_handles_unknown_total() {
        let ctl = ParseCtl::new(CancelToken::new(), 200);
        assert_eq!(ctl.fraction(0), 0.0);
        assert_eq!(ctl.fraction(100), 0.5);
        assert_eq!(ctl.fraction(500), 1.0);

        let unknown = ParseCtl::new(CancelToken::new(), 0);
        assert_eq!(unknown.fraction(123), 0.0);
    }

    #[test]
    fn progress_is_throttled_to_meaningful_advances() {
        let ctl = ParseCtl::new(CancelToken::new(), 10_000);
        let mut sink = ProgressSink::default();

        // 0.5% advances are swallowed; the 1% step and completion fire.
        ctl.report_progress(&mut sink, SourceId(0), 50); // 0.5% — no event
        ctl.report_progress(&mut sink, SourceId(0), 100); // 1.0% — event
        ctl.report_progress(&mut sink, SourceId(0), 150); // 1.5% — no event
        ctl.report_progress(&mut sink, SourceId(0), 10_000); // 100% — event

        assert_eq!(sink.events, vec![0.01, 1.0]);
    }

    #[test]
    fn unknown_total_emits_no_progress() {
        let ctl = ParseCtl::new(CancelToken::new(), 0);
        let mut sink = ProgressSink::default();
        ctl.report_progress(&mut sink, SourceId(0), 999);
        assert!(sink.events.is_empty());
    }
}
