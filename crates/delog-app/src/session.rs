//! App data session. [`Session`] owns the one ingest thread (the store's only
//! writer); the UI thread only ever does `snapshot()` + draw.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use delog_core::diagnostics::{Diag, DiagRecord, DiagnosticHub, Severity};
use delog_core::identity::SourceId;
use delog_core::ingest::{ingest_channel, IngestSender, IngestSink, ParseSummary, SourceKind};
use delog_core::ingestor::{IngestObserver, Ingestor};
use delog_core::metrics::MetricsRegistry;
use delog_core::parse_ctl::{CancelToken, ParseCtl};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_parsers::{
    ArduPilotParser, Detection, ParquetParser, ParseError, ParserRegistry, SNIFF_HEAD_LEN,
    TimestampSelectionProvider, TlogParser, ULogParser,
};

#[cfg(feature = "scripting")]
type LiveScriptSink = Arc<Mutex<Option<std::sync::mpsc::Sender<delog_script::LiveBatchInput>>>>;

#[derive(Debug, Clone, Copy, Default)]
struct LoadState {
    progress: f32,
    done: bool,
}

type Loads = Arc<Mutex<HashMap<SourceId, LoadState>>>;

/// Lives on the ingest thread.
struct AppObserver {
    loads: Loads,
    diagnostics: Arc<DiagnosticHub>,
    ctx: egui::Context,
    #[cfg(feature = "scripting")]
    live_scripts: LiveScriptSink,
}

impl IngestObserver for AppObserver {
    fn on_progress(&mut self, source: SourceId, frac: f32) {
        let mut loads = self.loads.lock().unwrap();
        let state = loads.entry(source).or_default();
        if !state.done {
            state.progress = frac;
        }
        self.ctx.request_repaint();
    }

    fn on_close(&mut self, source: SourceId, _summary: ParseSummary) {
        let mut loads = self.loads.lock().unwrap();
        let state = loads.entry(source).or_default();
        state.done = true;
        state.progress = 1.0;
        self.ctx.request_repaint();
    }

    fn on_remove(&mut self, source: SourceId) {
        let mut loads = self.loads.lock().unwrap();
        let state = loads.entry(source).or_default();
        state.done = true;
        state.progress = 1.0;
        self.ctx.request_repaint();
    }

    fn on_diagnostic(&mut self, diag: Diag) {
        self.diagnostics.emit(diag);
        self.ctx.request_repaint();
    }

    #[cfg(feature = "scripting")]
    fn on_batch(
        &mut self,
        kind: SourceKind,
        source_label: &str,
        batch: &delog_core::ingest::ParsedBatch,
    ) {
        if kind != SourceKind::Live {
            return;
        }
        let Some(tx) = self.live_scripts.lock().unwrap().clone() else {
            return;
        };
        match tx.send(delog_script::LiveBatchInput::new(
            source_label,
            batch.clone(),
        )) {
            Ok(()) => {}
            Err(std::sync::mpsc::SendError(_)) => {
                *self.live_scripts.lock().unwrap() = None;
            }
        }
    }
}

struct ActiveLoad {
    label: String,
    cancel: CancelToken,
    handle: Option<JoinHandle<()>>,
}

pub struct Session {
    store: Arc<DataStore>,
    sender: IngestSender,
    metrics: Arc<MetricsRegistry>,
    registry: Arc<ParserRegistry>,
    loads: Loads,
    diagnostics: Arc<DiagnosticHub>,
    active: Vec<ActiveLoad>,
    live_links: Vec<delog_stream::LiveLink>,
    /// Sources already swept by the post-load quality scan.
    scanned: HashSet<SourceId>,
    ctx: egui::Context,
    #[cfg(feature = "scripting")]
    live_scripts: LiveScriptSink,
    _ingest: JoinHandle<()>,
}

impl Session {
    pub fn new(ctx: egui::Context, parquet_selection: Arc<dyn TimestampSelectionProvider>) -> Self {
        let loads: Loads = Arc::default();
        let diagnostics = Arc::new(DiagnosticHub::new());
        #[cfg(feature = "scripting")]
        let live_scripts: LiveScriptSink = Arc::new(Mutex::new(None));
        let observer = AppObserver {
            loads: Arc::clone(&loads),
            diagnostics: Arc::clone(&diagnostics),
            ctx: ctx.clone(),
            #[cfg(feature = "scripting")]
            live_scripts: Arc::clone(&live_scripts),
        };

        let metrics = Arc::new(MetricsRegistry::new());
        let ingestor = Ingestor::new(observer).with_metrics(Arc::clone(&metrics));
        let store = ingestor.store();
        store.on_epoch({
            let ctx = ctx.clone();
            move |_epoch| ctx.request_repaint()
        });

        let (sender, receiver) = ingest_channel();
        let ingest = std::thread::Builder::new()
            .name("delog-ingest".into())
            .spawn(move || ingestor.run(receiver))
            .expect("spawn ingest thread");

        let mut registry = ParserRegistry::new();
        registry.register(Arc::new(ArduPilotParser));
        registry.register(Arc::new(ULogParser));
        registry.register(Arc::new(TlogParser));
        registry.register(Arc::new(ParquetParser::new(parquet_selection)));

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
            #[cfg(feature = "scripting")]
            live_scripts,
            _ingest: ingest,
        }
    }

    /// The current coherent view of the store (UI calls this once per frame).
    pub fn snapshot(&self) -> Arc<StoreSnapshot> {
        self.store.load()
    }

    /// The published store, for background engines that load fresh snapshots.
    /// Consumed by the scripts engine; dead without that feature.
    #[cfg_attr(not(feature = "scripting"), allow(dead_code))]
    pub fn store(&self) -> Arc<DataStore> {
        Arc::clone(&self.store)
    }

    /// A clone of the ingest sender, for engines that emit derived sources.
    /// Consumed by the scripts engine; dead without that feature.
    #[cfg_attr(not(feature = "scripting"), allow(dead_code))]
    pub fn ingest_sender(&self) -> IngestSender {
        self.sender.clone()
    }

    /// Wire (or unwire) the live-batch mirror into the script engine's queue.
    /// Called once per frame from `app.rs` to keep the sink current.
    #[cfg(feature = "scripting")]
    pub fn set_live_script_sink(
        &self,
        sink: Option<std::sync::mpsc::Sender<delog_script::LiveBatchInput>>,
    ) {
        *self.live_scripts.lock().unwrap() = sink;
    }

    /// The shared metrics registry — instrumentation reads/writes
    /// through this; the perf dock snapshots it at 4 Hz.
    pub fn metrics(&self) -> &Arc<MetricsRegistry> {
        &self.metrics
    }

    pub fn diagnostic_records(&self) -> Vec<DiagRecord> {
        self.diagnostics.snapshot()
    }

    pub fn clear_diagnostics(&self) {
        self.diagnostics.clear();
    }

    /// Push a diagnostic raised outside the ingest path (e.g. wgpu error
    /// scopes) into the same retained list.
    pub fn push_diagnostic(&self, diag: Diag) {
        self.diagnostics.emit(diag);
        self.ctx.request_repaint();
    }

    /// Start one live MAVLink link. Multiple calls create independent links;
    /// each link demuxes sysids into live sources downstream.
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

    /// Request a per-source time-offset change. Applied by the
    /// ingest thread (the single registry writer), which publishes a new
    /// epoch; stale render caches rebuild off that.
    pub fn set_source_offset(&self, source: SourceId, offset_us: i64) {
        self.sender.set_source_offset(source, offset_us);
    }

    pub fn set_source_offsets(&self, offsets: Vec<(SourceId, i64)>) -> Result<(), ()> {
        self.sender.set_source_offsets(offsets)
    }

    /// Request removal of a source. Applied by the ingest thread
    /// (the single registry writer): it tombstones the source and its
    /// topics/fields, drops their stores, and publishes a new epoch off which
    /// the cache manager GCs the orphaned render caches.
    pub fn remove_source(&self, source: SourceId) {
        self.sender.remove_source(source);
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

    /// Start parsing `path` on a worker thread. Returns immediately.
    pub fn open_path(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        let label = source_label(&path);
        let cancel = CancelToken::new();
        let sender = self.sender.clone();
        let registry = Arc::clone(&self.registry);
        let metrics = Arc::clone(&self.metrics);
        let ctx = self.ctx.clone();
        let worker_cancel = cancel.clone();
        let worker_label = label.clone();

        let handle = std::thread::Builder::new()
            .name("delog-parse".into())
            .spawn(move || {
                {
                    // Wall-clock parse time for this file (`parse_total`).
                    let _t = metrics.scope("parse_total");
                    run_parse(&path, &worker_label, &registry, &sender, worker_cancel);
                }
                ctx.request_repaint();
            })
            .expect("spawn parse thread");

        self.active.push(ActiveLoad {
            label,
            cancel,
            handle: Some(handle),
        });
    }

    pub fn cancel_all(&self) {
        for load in &self.active {
            load.cancel.cancel();
        }
    }

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
    /// summarized diagnostics. The snapshot is an immutable Arc, so
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
                    diagnostics.emit(diag);
                }
                ctx.request_repaint();
            })
            .expect("spawn quality scan thread");
    }

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
            // Auto-pick the best until the manual picker exists.
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
    let ctl = ParseCtl::new(cancel, source, total).with_label(label);
    match parser.parse(Box::new(file), &mut sink, &ctl) {
        Ok(_) => {}
        Err(ParseError::Setup { detail }) => {
            sender.remove_source(source);
            sink.diagnostic(Diag::error("parse-setup", format!("{label}: {detail}")).at_byte(0));
        }
        Err(ParseError::SetupCancelled) => sender.remove_source(source),
        Err(err) => {
            sink.diagnostic(Diag::warning("parse-ended", format!("{label}: {err}")).at_byte(0));
        }
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

    use arrow::array::{ArrayRef, Float32Array, Float64Array, Int64Array};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use delog_parsers::{
        LogParser, ParseError, ReadSeek, Sniff, TimestampSelection, TimestampSelectionError,
        TimestampSelectionProvider, TimestampSelectionRequest, TimestampUnit,
    };
    use parquet::arrow::ArrowWriter;

    use super::*;

    struct FixedSelectionProvider {
        response: Result<TimestampSelection, TimestampSelectionError>,
    }

    impl TimestampSelectionProvider for FixedSelectionProvider {
        fn select(
            &self,
            _request: TimestampSelectionRequest,
            _ctl: &ParseCtl,
        ) -> Result<TimestampSelection, TimestampSelectionError> {
            self.response
        }
    }

    fn fixed_selection_provider(
        response: Result<TimestampSelection, TimestampSelectionError>,
    ) -> Arc<dyn TimestampSelectionProvider> {
        Arc::new(FixedSelectionProvider { response })
    }

    fn cancelling_selection_provider() -> Arc<dyn TimestampSelectionProvider> {
        fixed_selection_provider(Err(TimestampSelectionError::Cancelled))
    }

    #[cfg(feature = "scripting")]
    #[test]
    fn app_observer_mirrors_only_live_batches_without_blocking() {
        use std::sync::mpsc::channel;

        use arrow::array::{ArrayRef, Float64Array, Int64Array};
        use arrow::datatypes::DataType;
        use delog_core::ingest::{ParsedBatch, SourceKind};
        use delog_core::schema::{FieldSchema, TopicSchema};

        let (tx, rx) = channel();
        let live_scripts = Arc::new(Mutex::new(Some(tx)));
        let mut observer = AppObserver {
            loads: Arc::default(),
            diagnostics: Arc::default(),
            ctx: egui::Context::default(),
            live_scripts,
        };
        let schema = Arc::new(
            TopicSchema::new(
                "A",
                [FieldSchema::new("v", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let batch = ParsedBatch::new(
            SourceId(0),
            schema,
            Int64Array::from(vec![1]),
            vec![Arc::new(Float64Array::from(vec![2.0])) as ArrayRef],
        );

        observer.on_batch(SourceKind::File, "flight", &batch);
        assert!(rx.try_recv().is_err(), "file batches are not mirrored");

        observer.on_batch(SourceKind::Live, "live", &batch);
        let mirrored = rx.try_recv().expect("live batches are mirrored");
        assert_eq!(mirrored.source_label, "live");
        assert_eq!(mirrored.batch.source, batch.source);
    }

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

    fn temp_parquet_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("delog-session-test-{name}.parquet"));
        path
    }

    fn write_generic_parquet(path: &Path) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("time_ms", DataType::Int64, false),
            Field::new("value", DataType::Float32, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1, 2])) as ArrayRef,
                Arc::new(Float32Array::from(vec![1.5, 2.5])) as ArrayRef,
            ],
        )
        .unwrap();
        let file = File::create(path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    fn write_all_invalid_parquet(path: &Path) {
        let rows = delog_parsers::parquet::PARQUET_BATCH_ROWS * 32;
        let schema = Arc::new(Schema::new(vec![
            Field::new("time", DataType::Float64, false),
            Field::new("value", DataType::Float32, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Float64Array::from(vec![f64::NAN; rows])) as ArrayRef,
                Arc::new(Float32Array::from(vec![1.0; rows])) as ArrayRef,
            ],
        )
        .unwrap();
        let file = File::create(path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    #[test]
    fn open_path_loads_a_bin_into_the_store() {
        let path = temp_path("load");
        File::create(&path).unwrap().write_all(&tiny_bin()).unwrap();

        let mut session =
            Session::new(egui::Context::default(), cancelling_selection_provider());
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
        assert!(snap.sources.iter().any(|s| s.entry.label.contains("load")));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_path_loads_a_generic_parquet_into_the_store() {
        let path = temp_parquet_path("generic-load");
        write_generic_parquet(&path);
        let provider = fixed_selection_provider(Ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Milliseconds,
        }));

        let mut session = Session::new(egui::Context::default(), provider);
        session.open_path(path.clone());
        session.join_workers();
        session.wait_until(|session| {
            session
                .snapshot()
                .topics
                .iter()
                .filter_map(|topic| topic.store.as_ref())
                .any(|store| store.rows == 2)
        });

        let snapshot = session.snapshot();
        let store = snapshot
            .topics
            .iter()
            .find_map(|topic| topic.store.as_ref())
            .expect("generic Parquet creates a topic store");
        assert_eq!(store.rows, 2);
        assert_eq!(store.schema.name(), source_label(path.as_path()));
        assert_eq!(store.chunks[0].t.values(), &[1_000, 2_000]);
        assert_eq!(store.schema.fields()[0].dtype, DataType::Float32);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cancelled_parquet_selection_tombstones_the_provisional_source() {
        let path = temp_parquet_path("cancelled");
        write_generic_parquet(&path);

        let mut session =
            Session::new(egui::Context::default(), cancelling_selection_provider());
        session.open_path(path.clone());
        session.join_workers();
        session.wait_until(|session| {
            session.snapshot().sources.iter().any(|source| {
                source.entry.label == source_label(path.as_path()) && source.entry.removed
            })
        });

        let snapshot = session.snapshot();
        let source = snapshot
            .sources
            .iter()
            .find(|source| source.entry.label == source_label(path.as_path()))
            .expect("Parquet parser opens a provisional source");
        assert!(source.entry.removed);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pre_submit_cancellation_after_progress_does_not_contaminate_the_next_load() {
        let invalid_path = temp_parquet_path("invalid-progress-cancel");
        write_all_invalid_parquet(&invalid_path);
        let next_path = temp_path("after-invalid-cancel");
        File::create(&next_path)
            .unwrap()
            .write_all(&tiny_bin())
            .unwrap();
        let provider = fixed_selection_provider(Ok(TimestampSelection {
            column_index: 0,
            unit: TimestampUnit::Seconds,
        }));
        let mut session = Session::new(egui::Context::default(), provider);

        session.open_path(invalid_path.clone());
        let cancel = session.active[0].cancel.clone();
        let loads = Arc::clone(&session.loads);
        let cancel_after_progress = std::thread::spawn(move || {
            for _ in 0..2_000 {
                if !loads.lock().unwrap().is_empty() {
                    cancel.cancel();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            panic!("all-invalid Parquet parse did not report progress");
        });
        session.join_workers();
        cancel_after_progress.join().unwrap();
        session.wait_until(|session| {
            session.snapshot().sources.iter().any(|source| {
                source.entry.label == source_label(invalid_path.as_path()) && source.entry.removed
            })
        });

        let invalid_source = session
            .snapshot()
            .sources
            .iter()
            .find(|source| source.entry.label == source_label(invalid_path.as_path()))
            .expect("all-invalid Parquet opens a provisional source")
            .entry
            .id;

        let mut late_sink = session.sender.file_sink();
        late_sink.progress(invalid_source, 0.25);
        late_sink.diagnostic(Diag::info(
            "late-progress-sentinel",
            "late progress was processed",
        ));
        session.wait_until(|session| {
            session
                .diagnostic_records()
                .iter()
                .any(|record| record.diag.code == "late-progress-sentinel")
        });

        session.open_path(next_path.clone());
        session.join_workers();
        session.wait_until(|session| {
            let snapshot = session.snapshot();
            let Some(source) = snapshot
                .sources
                .iter()
                .find(|source| source.entry.label == source_label(next_path.as_path()))
            else {
                return false;
            };
            session
                .loads
                .lock()
                .unwrap()
                .get(&source.entry.id)
                .is_some_and(|state| state.done)
        });
        let next_snapshot = session.snapshot();
        assert!(
            next_snapshot
                .topics
                .iter()
                .find(|topic| topic.entry.name == "TEST")
                .and_then(|topic| next_snapshot.topic_store(topic.entry.id))
                .is_some_and(|store| store.rows == 2)
        );

        let invalid_load = session.loads.lock().unwrap()[&invalid_source];
        assert!(invalid_load.done);
        assert_eq!(invalid_load.progress, 1.0);
        assert_eq!(
            session.overall_progress(),
            None,
            "removed-source progress must not contaminate later imports"
        );
        assert!(session.diagnostic_records().iter().all(|record| {
            record.diag.code != "parse-setup" && record.diag.code != "parse-ended"
        }));

        let _ = std::fs::remove_file(&invalid_path);
        let _ = std::fs::remove_file(&next_path);
    }

    #[test]
    fn unknown_format_reports_a_diagnostic_and_no_topics() {
        let path = temp_path("garbage");
        File::create(&path)
            .unwrap()
            .write_all(b"this is not a flight log")
            .unwrap();

        let mut session =
            Session::new(egui::Context::default(), cancelling_selection_provider());
        session.open_path(path.clone());
        session.join_workers();
        session.wait_until(|s| {
            s.diagnostic_records()
                .iter()
                .any(|record| record.diag.code == "format-unknown")
        });

        let snap = session.snapshot();
        assert!(snap.topics.iter().all(|t| t.store.is_none()));

        let _ = std::fs::remove_file(&path);
    }

    struct StubSetupParser;

    impl LogParser for StubSetupParser {
        fn name(&self) -> &'static str {
            "setup-stub"
        }

        fn sniff(&self, _head: &[u8]) -> Sniff {
            Sniff::new(100, "test")
        }

        fn parse(
            &self,
            _src: Box<dyn ReadSeek>,
            _sink: &mut dyn IngestSink,
            _ctl: &ParseCtl,
        ) -> Result<ParseSummary, ParseError> {
            Err(ParseError::Setup {
                detail: "bad schema".into(),
            })
        }
    }

    #[test]
    fn setup_failure_removes_the_provisional_source() {
        let path = temp_path("setup-failure");
        File::create(&path).unwrap().write_all(b"setup").unwrap();

        let session = Session::new(egui::Context::default(), cancelling_selection_provider());
        let mut registry = ParserRegistry::new();
        registry.register(Arc::new(StubSetupParser));
        run_parse(
            &path,
            "setup-failure",
            &registry,
            &session.sender,
            CancelToken::new(),
        );
        session.wait_until(|s| {
            let snapshot = s.snapshot();
            snapshot
                .sources
                .iter()
                .any(|source| source.entry.label == "setup-failure" && source.entry.removed)
                && s.diagnostic_records()
                    .iter()
                    .any(|record| record.diag.code == "parse-setup")
        });

        let snapshot = session.snapshot();
        let source = snapshot
            .sources
            .iter()
            .find(|source| source.entry.label == "setup-failure")
            .expect("setup parser opens a provisional source");
        assert!(source.entry.removed);
        assert!(session.diagnostic_records().iter().any(|record| {
            record.diag.code == "parse-setup" && record.diag.message == "setup-failure: bad schema"
        }));

        let _ = std::fs::remove_file(&path);
    }
}
