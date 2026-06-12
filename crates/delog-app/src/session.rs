//! App data session: the never-block load path (PLAN.md §19.6, UIX-11).
//!
//! [`Session`] owns the one ingest thread (the store's only writer) and the
//! parser registry. Opening a path sniffs the format and runs the parser on a
//! worker thread, so the UI thread only ever does `snapshot()` + draw. Progress
//! and diagnostics flow back through an [`IngestObserver`] into shared state the
//! UI reads; an epoch listener requests a repaint whenever data lands (ING-06).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use delog_core::diagnostics::{Diag, Severity};
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSender, IngestSink, ParseSummary, SourceKind, ingest_channel};
use delog_core::ingestor::{IngestObserver, Ingestor};
use delog_core::metrics::MetricsRegistry;
use delog_core::parse_ctl::{CancelToken, ParseCtl};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_parsers::{
    ArduPilotParser, Detection, ParserRegistry, SNIFF_HEAD_LEN, TlogParser, ULogParser,
};

/// Most recent diagnostics retained for the UI (full hub is DIA-01, M9).
const MAX_DIAGNOSTICS: usize = 1000;

/// Per-source load progress, shared between the ingest thread and the UI.
#[derive(Debug, Clone, Copy, Default)]
struct LoadState {
    progress: f32,
    done: bool,
}

type Loads = Arc<Mutex<HashMap<SourceId, LoadState>>>;
type Diags = Arc<Mutex<Vec<Diag>>>;

/// Observer that funnels ingest-side events into shared UI state and requests
/// repaints. Lives on the ingest thread.
struct AppObserver {
    loads: Loads,
    diagnostics: Diags,
    ctx: egui::Context,
}

impl IngestObserver for AppObserver {
    fn on_progress(&mut self, source: SourceId, frac: f32) {
        self.loads
            .lock()
            .unwrap()
            .entry(source)
            .or_default()
            .progress = frac;
        self.ctx.request_repaint();
    }

    fn on_close(&mut self, source: SourceId, _summary: ParseSummary) {
        let mut loads = self.loads.lock().unwrap();
        let state = loads.entry(source).or_default();
        state.done = true;
        state.progress = 1.0;
        self.ctx.request_repaint();
    }

    fn on_diagnostic(&mut self, diag: Diag) {
        push_diag(&self.diagnostics, diag);
        self.ctx.request_repaint();
    }
}

/// Append to the retained diagnostics, dropping the oldest over the cap.
fn push_diag(diags: &Diags, diag: Diag) {
    let mut diags = diags.lock().unwrap();
    if diags.len() >= MAX_DIAGNOSTICS {
        diags.remove(0);
    }
    diags.push(diag);
}

/// A parse running on a worker thread.
struct ActiveLoad {
    label: String,
    cancel: CancelToken,
    handle: Option<JoinHandle<()>>,
}

/// The owner of the store, ingest thread and parser registry.
pub struct Session {
    store: Arc<DataStore>,
    sender: IngestSender,
    metrics: Arc<MetricsRegistry>,
    registry: Arc<ParserRegistry>,
    loads: Loads,
    diagnostics: Diags,
    active: Vec<ActiveLoad>,
    live_links: Vec<delog_stream::LiveLink>,
    /// Sources already swept by the post-load quality scan (DIA-05).
    scanned: HashSet<SourceId>,
    ctx: egui::Context,
    _ingest: JoinHandle<()>,
}

impl Session {
    /// Spawn the ingest thread and register the built-in parsers.
    pub fn new(ctx: egui::Context) -> Self {
        let loads: Loads = Arc::default();
        let diagnostics: Diags = Arc::default();
        let observer = AppObserver {
            loads: Arc::clone(&loads),
            diagnostics: Arc::clone(&diagnostics),
            ctx: ctx.clone(),
        };

        let ingestor = Ingestor::new(observer);
        let store = ingestor.store();
        store.on_epoch({
            let ctx = ctx.clone();
            move |_epoch| ctx.request_repaint()
        });

        let (sender, receiver) = ingest_channel();
        let metrics = Arc::new(MetricsRegistry::new());
        let ingest = std::thread::Builder::new()
            .name("delog-ingest".into())
            .spawn(move || ingestor.run(receiver))
            .expect("spawn ingest thread");

        let mut registry = ParserRegistry::new();
        registry.register(Arc::new(ArduPilotParser));
        registry.register(Arc::new(ULogParser));
        registry.register(Arc::new(TlogParser));

        Self {
            store,
            sender,
            metrics,
            registry: Arc::new(registry),
            loads,
            diagnostics,
            active: Vec::new(),
            live_links: Vec::new(),
            scanned: HashSet::new(),
            ctx,
            _ingest: ingest,
        }
    }

    /// The current coherent view of the store (UI calls this once per frame).
    pub fn snapshot(&self) -> Arc<StoreSnapshot> {
        self.store.load()
    }

    /// The shared metrics registry (PLAN.md §16) — instrumentation reads/writes
    /// through this; the perf dock (PRF-*) snapshots it at 4 Hz.
    pub fn metrics(&self) -> &Arc<MetricsRegistry> {
        &self.metrics
    }

    pub fn diagnostics(&self) -> Vec<Diag> {
        self.diagnostics.lock().unwrap().clone()
    }

    /// Push a diagnostic raised outside the ingest path (e.g. wgpu error
    /// scopes, GPU-12) into the same retained list.
    pub fn push_diagnostic(&self, diag: Diag) {
        push_diag(&self.diagnostics, diag);
    }

    /// Start one live MAVLink link. Multiple calls create independent links
    /// (LIV-10); each link demuxes sysids into live sources downstream.
    pub fn start_live(
        &mut self,
        endpoint: delog_stream::Endpoint,
        recording: Option<PathBuf>,
    ) -> Result<(), String> {
        match delog_stream::LiveLink::spawn(
            endpoint.clone(),
            self.sender.clone(),
            Arc::clone(&self.metrics),
            recording,
        ) {
            Ok(link) => {
                self.live_links.push(link);
                self.ctx.request_repaint();
                Ok(())
            }
            Err(err) => Err(format!("{endpoint}: {err}")),
        }
    }

    pub fn live_statuses(&self) -> Vec<delog_stream::LiveLinkStatus> {
        self.live_links.iter().map(|link| link.status()).collect()
    }

    /// Disconnect the live link at `index` (matching `live_statuses` order).
    /// Dropping the `LiveLink` stops its reader and ingest threads; samples
    /// already ingested stay in the store.
    pub fn stop_live(&mut self, index: usize) {
        if index < self.live_links.len() {
            self.live_links.remove(index);
            self.ctx.request_repaint();
        }
    }

    pub fn has_live_links(&self) -> bool {
        !self.live_links.is_empty()
    }

    pub fn has_connected_live(&self) -> bool {
        self.live_links
            .iter()
            .any(|link| link.status().state == delog_stream::LinkState::Connected)
    }

    /// Request a per-source time-offset change (§4.2, BRW-07). Applied by the
    /// ingest thread (the single registry writer), which publishes a new
    /// epoch; stale render caches rebuild off that.
    pub fn set_source_offset(&self, source: SourceId, offset_us: i64) {
        self.sender.set_source_offset(source, offset_us);
    }

    /// Combined progress of in-flight loads (the least-advanced one), or `None`
    /// when nothing is loading.
    pub fn overall_progress(&self) -> Option<f32> {
        self.loads
            .lock()
            .unwrap()
            .values()
            .filter(|s| !s.done)
            .map(|s| s.progress)
            .min_by(|a, b| a.total_cmp(b))
    }

    /// Labels + cancel handles of loads still running.
    pub fn active_labels(&self) -> Vec<String> {
        self.active
            .iter()
            .filter(|a| a.handle.as_ref().is_some_and(|h| !h.is_finished()))
            .map(|a| a.label.clone())
            .collect()
    }

    pub fn has_active_loads(&self) -> bool {
        self.active
            .iter()
            .any(|a| a.handle.as_ref().is_some_and(|h| !h.is_finished()))
    }

    /// Start parsing `path` on a worker thread. Returns immediately (§19.6).
    pub fn open_path(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        let label = source_label(&path);
        let cancel = CancelToken::new();
        let sender = self.sender.clone();
        let registry = Arc::clone(&self.registry);
        let ctx = self.ctx.clone();
        let worker_cancel = cancel.clone();
        let worker_label = label.clone();

        let handle = std::thread::Builder::new()
            .name("delog-parse".into())
            .spawn(move || {
                run_parse(&path, &worker_label, &registry, &sender, worker_cancel);
                ctx.request_repaint();
            })
            .expect("spawn parse thread");

        self.active.push(ActiveLoad {
            label,
            cancel,
            handle: Some(handle),
        });
    }

    /// Request cancellation of every running load (UIX-02 cancel).
    pub fn cancel_all(&self) {
        for load in &self.active {
            load.cancel.cancel();
        }
    }

    /// Drop bookkeeping for finished loads and kick the post-load data-quality
    /// scan for newly finished sources (§15, DIA-05).
    pub fn prune_finished(&mut self) {
        self.active
            .retain(|a| a.handle.as_ref().is_some_and(|h| !h.is_finished()));

        let done: Vec<SourceId> = self
            .loads
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, state)| state.done && !self.scanned.contains(id))
            .map(|(&id, _)| id)
            .collect();
        for source in done {
            self.scanned.insert(source);
            self.spawn_quality_scan(source);
        }
    }

    /// Scan `source` for data-quality findings off the UI thread and push the
    /// summarized diagnostics (DIA-05). The snapshot is an immutable Arc, so
    /// the scan races nothing.
    fn spawn_quality_scan(&self, source: SourceId) {
        let snapshot = self.store.load();
        let diagnostics = Arc::clone(&self.diagnostics);
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("delog-quality-scan".into())
            .spawn(move || {
                let findings = delog_core::quality::scan_source(&snapshot, source);
                if findings.is_empty() {
                    return;
                }
                for diag in findings {
                    push_diag(&diagnostics, diag);
                }
                ctx.request_repaint();
            })
            .expect("spawn quality scan thread");
    }

    /// Test helper: join every worker so all messages have been sent.
    #[cfg(test)]
    fn join_workers(&mut self) {
        for load in &mut self.active {
            if let Some(handle) = load.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// Test helper: poll `pred` until true (bounded so a bug can't hang).
    #[cfg(test)]
    fn wait_until(&self, mut pred: impl FnMut(&Self) -> bool) {
        for _ in 0..2_000 {
            if pred(self) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("condition not met in time");
    }
}

/// Sniff, pick a parser, and run it into the ingest sink. Diagnostics are
/// emitted through the sink so they reach the same hub as parser diagnostics.
fn run_parse(
    path: &Path,
    label: &str,
    registry: &ParserRegistry,
    sender: &IngestSender,
    cancel: CancelToken,
) {
    let mut sink = sender.file_sink();
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(err) => {
            sink.diagnostic(Diag::new(
                Severity::Error,
                "open-failed",
                format!("{}: {err}", path.display()),
            ));
            return;
        }
    };
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);

    let mut head = vec![0u8; SNIFF_HEAD_LEN];
    let read = read_head(&mut file, &mut head);
    head.truncate(read);
    if file.seek(SeekFrom::Start(0)).is_err() {
        sink.diagnostic(Diag::error(
            "seek-failed",
            format!("{label}: cannot rewind"),
        ));
        return;
    }

    let parser = match registry.detect(&head) {
        Detection::Auto(parser) => parser,
        Detection::Ambiguous(candidates) => {
            // Auto-pick the best until the manual picker exists (UIX-10).
            let best = &candidates[0];
            sink.diagnostic(Diag::warning(
                "format-ambiguous",
                format!(
                    "{label}: ambiguous format, assuming `{}` (score {})",
                    best.parser.name(),
                    best.sniff.score
                ),
            ));
            Arc::clone(&best.parser)
        }
        Detection::Unknown => {
            sink.diagnostic(Diag::error(
                "format-unknown",
                format!("{label}: no parser recognised this file"),
            ));
            return;
        }
    };

    let source = sink.open_source(label, SourceKind::File);
    let ctl = ParseCtl::new(cancel, source, total);
    if let Err(err) = parser.parse(Box::new(file), &mut sink, &ctl) {
        sink.diagnostic(Diag::warning("parse-ended", format!("{label}: {err}")).at_byte(0));
    }
}

fn read_head(file: &mut File, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    filled
}

fn source_label(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("source")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Minimal self-describing DataFlash log: one TEST message, two rows.
    fn tiny_bin() -> Vec<u8> {
        const HEAD1: u8 = 0xA3;
        const HEAD2: u8 = 0x95;
        const FMT: u8 = 0x80;
        let mut buf = Vec::new();
        // FMT for TEST(200): format "Qf" (TimeUS, A), length = 3 + 8 + 4 = 15.
        buf.extend([HEAD1, HEAD2, FMT, 200, 15]);
        let field = |s: &str, w: usize, b: &mut Vec<u8>| {
            b.extend(s.as_bytes());
            b.extend(std::iter::repeat_n(0u8, w - s.len()));
        };
        field("TEST", 4, &mut buf);
        field("Qf", 16, &mut buf);
        field("TimeUS,A", 64, &mut buf);
        for (t, a) in [(1_000u64, 1.5f32), (2_000, 2.5)] {
            buf.extend([HEAD1, HEAD2, 200]);
            buf.extend(t.to_le_bytes());
            buf.extend(a.to_le_bytes());
        }
        buf
    }

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("delog-session-test-{name}.BIN"));
        p
    }

    #[test]
    fn open_path_loads_a_bin_into_the_store() {
        let path = temp_path("load");
        File::create(&path).unwrap().write_all(&tiny_bin()).unwrap();

        let mut session = Session::new(egui::Context::default());
        session.open_path(path.clone());
        session.join_workers();
        session.wait_until(|s| {
            let snap = s.snapshot();
            snap.topics
                .iter()
                .find(|t| t.entry.name == "TEST")
                .and_then(|t| snap.topic_store(t.entry.id))
                .is_some_and(|store| store.rows == 2)
        });

        let snap = session.snapshot();
        // The source was labelled from the file stem.
        assert!(snap.sources.iter().any(|s| s.entry.label.contains("load")));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_format_reports_a_diagnostic_and_no_topics() {
        let path = temp_path("garbage");
        File::create(&path)
            .unwrap()
            .write_all(b"this is not a flight log")
            .unwrap();

        let mut session = Session::new(egui::Context::default());
        session.open_path(path.clone());
        session.join_workers();
        session.wait_until(|s| s.diagnostics().iter().any(|d| d.code == "format-unknown"));

        let snap = session.snapshot();
        assert!(snap.topics.iter().all(|t| t.store.is_none()));

        let _ = std::fs::remove_file(&path);
    }
}
