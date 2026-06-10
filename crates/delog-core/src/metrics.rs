//! Performance metrics registry (PLAN.md §16).
//!
//! Hot paths record through cheap atomics — a metric write is one
//! `fetch_add` plus one relaxed store — so instrumentation can stay on
//! permanently. The perf dock merely *reads*, computing ring statistics at
//! its own 4 Hz refresh rate.
//!
//! Usage:
//! ```
//! use delog_core::metrics::MetricsRegistry;
//!
//! let metrics = MetricsRegistry::new();
//! {
//!     let _t = metrics.scope("yquery"); // RAII: records elapsed ms on drop
//! }
//! metrics.record("upload_bytes", 4096.0); // gauge sample
//! metrics.add("ingest_dropped_batches", 1); // monotonic counter
//! assert_eq!(metrics.stats("yquery").unwrap().n, 1);
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// Number of samples each metric ring retains.
pub const RING_LEN: usize = 256;

/// Statistics over the last [`RING_LEN`] recorded samples of one metric.
///
/// Timers record **milliseconds**; gauges record whatever unit their call
/// site documents. `n` is the total number of samples ever recorded (not
/// capped at the ring length). `counter` is the metric's monotonic counter
/// value (0 unless [`MetricsRegistry::add`] was used).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricStats {
    pub last: f32,
    pub min: f32,
    pub max: f32,
    pub avg: f32,
    pub p99: f32,
    pub n: u64,
    pub counter: u64,
}

/// A registry of named metrics: RAII scope timers, gauges and counters.
///
/// Cloneable handle semantics are provided by wrapping in `Arc` at the
/// call site; the registry itself is `Send + Sync`.
pub struct MetricsRegistry {
    metrics: RwLock<HashMap<&'static str, Arc<Metric>>>,
}

struct Metric {
    /// Ring of f32 sample bit patterns; index = `count % RING_LEN`.
    ring: [AtomicU32; RING_LEN],
    /// Total samples ever recorded; the write cursor.
    count: AtomicU64,
    /// Monotonic counter (for drop/overflow style metrics).
    counter: AtomicU64,
}

/// RAII timer returned by [`MetricsRegistry::scope`]; records elapsed
/// milliseconds into the metric's ring when dropped.
pub struct ScopeTimer {
    metric: Arc<Metric>,
    start: Instant,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self {
            metrics: RwLock::new(HashMap::new()),
        }
    }

    /// Start an RAII timer; elapsed time is recorded in **milliseconds**
    /// when the returned guard drops.
    #[must_use = "the timer records on drop — binding to `_` discards it immediately"]
    pub fn scope(&self, name: &'static str) -> ScopeTimer {
        ScopeTimer {
            metric: self.get_or_register(name),
            start: Instant::now(),
        }
    }

    /// Record one gauge sample.
    pub fn record(&self, name: &'static str, value: f32) {
        self.get_or_register(name).record(value);
    }

    /// Increment the metric's monotonic counter.
    pub fn add(&self, name: &'static str, delta: u64) {
        self.get_or_register(name)
            .counter
            .fetch_add(delta, Ordering::Relaxed);
    }

    /// Current monotonic counter value, or `None` for unknown metrics.
    pub fn counter(&self, name: &'static str) -> Option<u64> {
        let metrics = self.metrics.read().expect("metrics map poisoned");
        Some(metrics.get(name)?.counter.load(Ordering::Relaxed))
    }

    /// Ring statistics for one metric, or `None` for unknown metrics.
    pub fn stats(&self, name: &'static str) -> Option<MetricStats> {
        let metrics = self.metrics.read().expect("metrics map poisoned");
        Some(metrics.get(name)?.stats())
    }

    /// `(name, stats)` for every registered metric, sorted by name —
    /// what the perf dock iterates at 4 Hz.
    pub fn snapshot(&self) -> Vec<(&'static str, MetricStats)> {
        let metrics = self.metrics.read().expect("metrics map poisoned");
        let mut out: Vec<_> = metrics.iter().map(|(&n, m)| (n, m.stats())).collect();
        out.sort_by_key(|&(n, _)| n);
        out
    }

    fn get_or_register(&self, name: &'static str) -> Arc<Metric> {
        if let Some(m) = self.metrics.read().expect("metrics map poisoned").get(name) {
            return Arc::clone(m);
        }
        let mut metrics = self.metrics.write().expect("metrics map poisoned");
        Arc::clone(
            metrics
                .entry(name)
                .or_insert_with(|| Arc::new(Metric::new())),
        )
    }
}

impl Metric {
    fn new() -> Self {
        Self {
            ring: std::array::from_fn(|_| AtomicU32::new(0)),
            count: AtomicU64::new(0),
            counter: AtomicU64::new(0),
        }
    }

    /// One `fetch_add` + one relaxed store — safe on any hot path.
    /// Two concurrent writers may claim slots that alias the same ring
    /// cell after wrap; the stats are advisory, so the lost sample is
    /// acceptable by design.
    fn record(&self, value: f32) {
        let idx = self.count.fetch_add(1, Ordering::Relaxed) as usize % RING_LEN;
        self.ring[idx].store(value.to_bits(), Ordering::Relaxed);
    }

    fn stats(&self) -> MetricStats {
        let n = self.count.load(Ordering::Relaxed);
        let counter = self.counter.load(Ordering::Relaxed);
        let len = (n as usize).min(RING_LEN);
        if len == 0 {
            return MetricStats {
                last: 0.0,
                min: 0.0,
                max: 0.0,
                avg: 0.0,
                p99: 0.0,
                n,
                counter,
            };
        }

        let mut window: Vec<f32> = self.ring[..len]
            .iter()
            .map(|s| f32::from_bits(s.load(Ordering::Relaxed)))
            .collect();
        let last = window[(n as usize - 1) % RING_LEN];

        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        let mut sum = 0.0_f64;
        for &v in &window {
            min = min.min(v);
            max = max.max(v);
            sum += f64::from(v);
        }
        let avg = (sum / len as f64) as f32;

        window.sort_by(f32::total_cmp);
        let p99 = window[((len - 1) as f64 * 0.99).round() as usize];

        MetricStats {
            last,
            min,
            max,
            avg,
            p99,
            n,
            counter,
        }
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopeTimer {
    fn drop(&mut self) {
        let elapsed_ms = self.start.elapsed().as_secs_f64() * 1e3;
        self.metric.record(elapsed_ms as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn gauge_records_basic_stats() {
        let m = MetricsRegistry::new();
        for v in [1.0_f32, 2.0, 3.0, 4.0, 5.0] {
            m.record("g", v);
        }
        let s = m.stats("g").unwrap();
        assert_eq!(s.last, 5.0);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.max, 5.0);
        assert_eq!(s.avg, 3.0);
        assert_eq!(s.n, 5);
    }

    #[test]
    fn unknown_metric_has_no_stats() {
        let m = MetricsRegistry::new();
        assert!(m.stats("nope").is_none());
        assert!(m.counter("nope").is_none());
    }

    #[test]
    fn ring_wraps_and_keeps_only_recent_samples() {
        let m = MetricsRegistry::new();
        // 300 samples: 0..300. The ring keeps the last 256 (44..300).
        for v in 0..300 {
            m.record("g", v as f32);
        }
        let s = m.stats("g").unwrap();
        assert_eq!(s.n, 300);
        assert_eq!(s.last, 299.0);
        assert_eq!(s.min, 44.0);
        assert_eq!(s.max, 299.0);
    }

    #[test]
    fn p99_is_near_the_high_end() {
        let m = MetricsRegistry::new();
        for v in 1..=100 {
            m.record("g", v as f32);
        }
        let s = m.stats("g").unwrap();
        assert!(
            (99.0..=100.0).contains(&s.p99),
            "p99 = {} not in [99, 100]",
            s.p99
        );
    }

    #[test]
    fn scope_timer_records_elapsed_milliseconds() {
        let m = MetricsRegistry::new();
        {
            let _t = m.scope("timed");
            thread::sleep(Duration::from_millis(10));
        }
        let s = m.stats("timed").unwrap();
        assert_eq!(s.n, 1);
        // Sleep is >= 10 ms; allow generous headroom for a loaded machine.
        assert!(s.last >= 10.0, "elapsed {} ms < 10 ms", s.last);
        assert!(s.last < 1000.0, "elapsed {} ms implausibly large", s.last);
    }

    #[test]
    fn counters_accumulate() {
        let m = MetricsRegistry::new();
        m.add("drops", 2);
        m.add("drops", 3);
        assert_eq!(m.counter("drops"), Some(5));
        assert_eq!(m.stats("drops").unwrap().counter, 5);
    }

    #[test]
    fn snapshot_lists_all_metrics_sorted() {
        let m = MetricsRegistry::new();
        m.record("zzz", 1.0);
        m.record("aaa", 2.0);
        m.add("mmm", 1);
        let names: Vec<_> = m.snapshot().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["aaa", "mmm", "zzz"]);
    }

    #[test]
    fn empty_metric_stats_are_zeroed() {
        let m = MetricsRegistry::new();
        m.add("only_counted", 7); // counter-only: no ring samples yet
        let s = m.stats("only_counted").unwrap();
        assert_eq!(s.n, 0);
        assert_eq!(s.last, 0.0);
        assert_eq!(s.min, 0.0);
        assert_eq!(s.max, 0.0);
        assert_eq!(s.avg, 0.0);
        assert_eq!(s.p99, 0.0);
        assert_eq!(s.counter, 7);
    }

    #[test]
    fn concurrent_recording_loses_nothing_in_the_total() {
        let m = Arc::new(MetricsRegistry::new());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    for v in 0..1000 {
                        m.record("hot", v as f32);
                        m.add("hot", 1);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let s = m.stats("hot").unwrap();
        assert_eq!(s.n, 8000);
        assert_eq!(s.counter, 8000);
    }
}
