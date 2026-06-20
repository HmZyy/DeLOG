//! The script worker thread: owns the CPython interpreter for the app lifetime
//! and serves REPL evals over channels (SCR-02..05).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSender, IngestSink, ParsedBatch, SourceKind};
use delog_core::metrics::MetricsRegistry;
use delog_core::snapshot::DataStore;
use numpy::IntoPyArray;
use pyo3::exceptions::{PyKeyboardInterrupt, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::api::Delog;
use crate::custom_parser::{
    ParserOutput, emit_parser_output, parse_python_result, read_float32_file,
};
use crate::live::{
    LiveBatchPy, LiveTransformBatch, LiveTransformSpec, parse_transform_result, result_to_batch,
};

/// Bound on the app→engine live-batch queue. Full = drop (non-blocking submit).
pub const LIVE_TRANSFORM_QUEUE_CAP: usize = 128;

/// Consecutive errors a live transform may raise before it is disabled.
const LIVE_TRANSFORM_ERROR_LIMIT: u8 = 3;

/// One registered live transform the worker executes on matching live batches.
struct ActiveTransform {
    spec: LiveTransformSpec,
    callable: Py<PyAny>,
    source: SourceId,
    consecutive_errors: u8,
    disabled: bool,
}

/// Format a Python error with its traceback (if any) for an Error event.
fn format_pyerr(py: Python<'_>, err: &PyErr) -> String {
    let tb = err
        .traceback(py)
        .and_then(|t| t.format().ok())
        .unwrap_or_default();
    format!("{tb}{err}")
}

/// A command sent from the UI thread to the worker.
pub enum ScriptCommand {
    /// Evaluate one REPL line in the persistent namespace.
    Eval(String),
    /// Run a full script buffer; on success emits one `script:<name>` source.
    RunScript { name: String, source: String },
    /// Unregister the live transforms registered under `name` and remove their
    /// appendable `script:<name>` live-derived source.
    UnregisterLive { name: String },
    /// Compile a custom parser without executing it.
    ValidateParser { name: String, source: String },
    /// Read and parse one raw float file using a custom parser.
    ParseFile {
        parser_name: String,
        source: String,
        path: PathBuf,
    },
    /// Stop the worker (sender dropped also stops it).
    Shutdown,
}

/// Lifecycle events specific to custom file parsers.
#[derive(Debug, Clone, PartialEq)]
pub enum ParserEvent {
    SyntaxValid {
        name: String,
    },
    Reading {
        parser_name: String,
        path: PathBuf,
    },
    Running {
        parser_name: String,
        path: PathBuf,
    },
    Succeeded {
        parser_name: String,
        path: PathBuf,
        topics: u64,
        rows: u64,
    },
    Failed {
        parser_name: String,
        path: Option<PathBuf>,
        message: String,
    },
}

/// An event streamed from the worker back to the UI thread.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptEvent {
    /// Captured stdout/stderr text.
    Output(String),
    /// The repr of a REPL expression value.
    Result(String),
    /// A Python error / traceback.
    Error(String),
    /// A command finished.
    Done,
    /// One mirrored live batch was processed by the registered transforms.
    /// Carries no command-completion semantics; the UI ignores it.
    LiveBatchProcessed,
    /// A custom parser lifecycle event; never implies ordinary script completion.
    Parser(ParserEvent),
}

/// Handle to the worker thread. Dropping it shuts the worker down.
pub struct ScriptEngine {
    tx: Sender<ScriptCommand>,
    /// Bounded, non-blocking queue of raw live batches to run transforms on.
    live_tx: SyncSender<ParsedBatch>,
    events: Receiver<ScriptEvent>,
    handle: Option<JoinHandle<()>>,
    /// The live transforms currently active, keyed by registering script name
    /// (mirrors the worker's `active_transforms`). Shared with the worker so it
    /// stays current as scripts register/unregister; the UI reads it through
    /// [`ScriptEngine::has_live_transform`] to enable the Unregister button.
    active_live: Arc<Mutex<HashMap<String, Vec<LiveTransformSpec>>>>,
}

impl ScriptEngine {
    /// Spawn the interpreter worker. The interpreter and its global namespace
    /// live for the worker's lifetime, so REPL state persists across evals.
    pub fn spawn(
        store: Arc<DataStore>,
        sender: IngestSender,
        metrics: Arc<MetricsRegistry>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = channel::<ScriptCommand>();
        let (evt_tx, evt_rx) = channel::<ScriptEvent>();
        let (live_tx, live_rx) = sync_channel::<ParsedBatch>(LIVE_TRANSFORM_QUEUE_CAP);
        let active_live: Arc<Mutex<HashMap<String, Vec<LiveTransformSpec>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let active_live_worker = Arc::clone(&active_live);
        let handle = std::thread::Builder::new()
            .name("delog-script".into())
            .spawn(move || {
                worker_loop(
                    store,
                    sender,
                    metrics,
                    cmd_rx,
                    live_rx,
                    evt_tx,
                    active_live_worker,
                )
            })
            .expect("spawn script thread");
        Self {
            tx: cmd_tx,
            live_tx,
            events: evt_rx,
            handle: Some(handle),
            active_live,
        }
    }

    pub fn send(&self, cmd: ScriptCommand) -> Result<(), String> {
        self.tx
            .send(cmd)
            .map_err(|_| "script worker command channel disconnected".to_owned())
    }

    /// A cloneable sender for raw live batches (the app's live-decoder side
    /// forwards each batch here so registered transforms can run on it).
    pub fn live_batch_sender(&self) -> SyncSender<ParsedBatch> {
        self.live_tx.clone()
    }

    /// Non-blocking submit of one raw live batch. Returns the batch back if the
    /// queue is full or the worker is gone — live data is droppable (§5).
    // The Err variant hands the (large) batch back so the caller can drop or
    // count it; boxing would defeat the give-back-without-copy purpose.
    #[allow(clippy::result_large_err)]
    pub fn try_send_live_batch(&self, batch: ParsedBatch) -> Result<(), ParsedBatch> {
        match self.live_tx.try_send(batch) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(batch)) => Err(batch),
            Err(std::sync::mpsc::TrySendError::Disconnected(batch)) => Err(batch),
        }
    }

    /// Drain any ready events without blocking (UI thread calls this per frame).
    pub fn drain_events(&self) -> Vec<ScriptEvent> {
        self.events.try_iter().collect()
    }

    /// Ask the running script to stop (raises KeyboardInterrupt at the next
    /// bytecode boundary). Pure C calls (e.g. a large numpy op) cannot be
    /// interrupted mid-call — documented limitation (SCR-05).
    pub fn request_interrupt(&self) {
        // We use both mechanisms for maximum coverage:
        // 1. PyErr_SetInterrupt sets the pending-SIGINT trip flag (works when a
        //    SIGINT handler is installed on the main thread, i.e. in the real app).
        // 2. Py_AddPendingCall injects a callback into CPython's eval-loop pending
        //    call queue, which is checked at every bytecode boundary in any thread
        //    regardless of SIGINT handler state — this is what actually fires in
        //    the embedded/worker-thread test environment.
        //
        // Safety: both functions are documented as safe to call from any thread
        // without holding the GIL.
        extern "C" fn raise_keyboard_interrupt(_arg: *mut std::ffi::c_void) -> std::ffi::c_int {
            // Set the exception on the current thread state, then return -1 so
            // the eval loop propagates it immediately.
            // Safety: called from within a CPython eval-loop pending-call
            // callback; PyExc_KeyboardInterrupt is a valid, initialized static.
            unsafe {
                pyo3::ffi::PyErr_SetNone(pyo3::ffi::PyExc_KeyboardInterrupt);
            }
            -1
        }
        unsafe {
            pyo3::ffi::PyErr_SetInterrupt();
            pyo3::ffi::Py_AddPendingCall(Some(raise_keyboard_interrupt), std::ptr::null_mut());
        }
    }

    /// Whether the script named `name` currently has a registered live
    /// transform. The UI uses this to enable/disable the Unregister button.
    pub fn has_live_transform(&self, name: &str) -> bool {
        self.active_live.lock().unwrap().contains_key(name)
    }

    #[cfg(test)]
    pub fn recv_blocking(&self) -> ScriptEvent {
        self.events.recv().expect("worker alive")
    }

    #[cfg(test)]
    pub fn transform_specs(&self) -> Vec<crate::live::LiveTransformSpec> {
        self.active_live
            .lock()
            .unwrap()
            .values()
            .flatten()
            .cloned()
            .collect()
    }
}

impl Drop for ScriptEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(ScriptCommand::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// A Python file-like object that forwards `write` to the event channel.
#[pyclass]
struct OutputCapture {
    tx: Sender<ScriptEvent>,
}

#[pymethods]
impl OutputCapture {
    fn write(&self, data: &str) -> usize {
        if !data.is_empty() {
            let _ = self.tx.send(ScriptEvent::Output(data.to_string()));
        }
        data.len()
    }
    fn flush(&self) {}
}

fn worker_loop(
    store: Arc<DataStore>,
    sender: IngestSender,
    metrics: Arc<MetricsRegistry>,
    cmd_rx: Receiver<ScriptCommand>,
    live_rx: Receiver<ParsedBatch>,
    evt_tx: Sender<ScriptEvent>,
    active_live: Arc<Mutex<HashMap<String, Vec<LiveTransformSpec>>>>,
) {
    // One persistent globals dict for the whole session.
    let globals: Py<PyDict> = Python::attach(|py| PyDict::new(py).unbind());
    // Per-script-name snapshot-emit source from the previous run, for
    // replace-on-rerun (the `emit_topics` Derived source). Kept separate from
    // `live_sources` because one script name can drive BOTH a snapshot-emit
    // source and a live-derived source in the same run; sharing one map would
    // clobber.
    let mut prev_sources: HashMap<String, SourceId> = HashMap::new();
    // Per-script-name live-derived source from the previous run (the appendable
    // `LiveDerived` source registered transforms write into). A re-run removes
    // the prior generation's source so only the latest stays active.
    let mut live_sources: HashMap<String, SourceId> = HashMap::new();
    // Live transforms currently active, keyed by the registering script name. A
    // re-run replaces that script's set.
    let mut active_transforms: HashMap<String, Vec<ActiveTransform>> = HashMap::new();
    // Monotonic counter for generation tracking across script runs.
    let mut run_counter: u64 = 0;

    // Install stdout/stderr capture once, before the command loop.
    Python::attach(|py| {
        let sys = py.import("sys").expect("import sys");
        let cap =
            Bound::new(py, OutputCapture { tx: evt_tx.clone() }).expect("build OutputCapture");
        sys.setattr("stdout", &cap).expect("set sys.stdout");
        sys.setattr("stderr", &cap).expect("set sys.stderr");
    });

    // Poll both the command and live-batch channels. Commands take priority;
    // when neither has work, sleep briefly to avoid a busy spin. The loop ends
    // when both senders are gone (or an explicit Shutdown).
    loop {
        match cmd_rx.try_recv() {
            Ok(cmd) => {
                if handle_command(
                    cmd,
                    &store,
                    &sender,
                    &metrics,
                    &globals,
                    &evt_tx,
                    &active_live,
                    &mut prev_sources,
                    &mut live_sources,
                    &mut active_transforms,
                    &mut run_counter,
                ) {
                    break; // Shutdown
                }
                continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }

        match live_rx.try_recv() {
            Ok(batch) => {
                run_live_transforms(&sender, &evt_tx, &mut active_transforms, batch);
                let _ = evt_tx.send(ScriptEvent::LiveBatchProcessed);
                continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // Command channel still open (checked above); keep serving it.
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }

        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Run every active, matching live transform against one raw batch, emitting a
/// derived batch per success. Transforms disable themselves after
/// [`LIVE_TRANSFORM_ERROR_LIMIT`] consecutive errors (one Error event emitted).
fn run_live_transforms(
    sender: &IngestSender,
    evt_tx: &Sender<ScriptEvent>,
    active_transforms: &mut HashMap<String, Vec<ActiveTransform>>,
    batch: ParsedBatch,
) {
    for transforms in active_transforms.values_mut() {
        for transform in transforms.iter_mut() {
            if transform.disabled || !transform.spec.matches(&batch) {
                continue;
            }
            match run_one_transform(transform, &batch) {
                Ok(derived) => {
                    transform.consecutive_errors = 0;
                    let mut sink = sender.file_sink();
                    sink.submit(derived);
                }
                Err(msg) => {
                    transform.consecutive_errors += 1;
                    if transform.consecutive_errors >= LIVE_TRANSFORM_ERROR_LIMIT {
                        transform.disabled = true;
                        let _ = evt_tx.send(ScriptEvent::Error(format!(
                            "live transform '{}' disabled after {} consecutive errors; last: {msg}",
                            transform.spec.output_topic, LIVE_TRANSFORM_ERROR_LIMIT
                        )));
                    }
                }
            }
        }
    }
}

/// Execute one live transform end to end: materialize, call the Python
/// callable, parse + validate the result, and build the derived `ParsedBatch`.
fn run_one_transform(
    transform: &ActiveTransform,
    batch: &ParsedBatch,
) -> Result<ParsedBatch, String> {
    let materialized = LiveTransformBatch::from_parsed(&transform.spec, batch)?;
    let input_times = materialized.times.clone();
    let result = Python::attach(|py| -> Result<crate::live::LiveTransformResult, String> {
        let py_batch = Bound::new(py, LiveBatchPy::from_materialized(py, materialized))
            .map_err(|e| format_pyerr(py, &e))?;
        let ret = transform
            .callable
            .bind(py)
            .call1((py_batch,))
            .map_err(|e| format_pyerr(py, &e))?;
        parse_transform_result(py, &transform.spec, &input_times, &ret)
            .map_err(|e| format_pyerr(py, &e))
    })?;
    result_to_batch(transform.source, result)
}

/// Handle one command. Returns `true` if the worker should shut down.
#[allow(clippy::too_many_arguments)]
fn handle_command(
    cmd: ScriptCommand,
    store: &Arc<DataStore>,
    sender: &IngestSender,
    metrics: &MetricsRegistry,
    globals: &Py<PyDict>,
    evt_tx: &Sender<ScriptEvent>,
    active_live: &Arc<Mutex<HashMap<String, Vec<LiveTransformSpec>>>>,
    prev_sources: &mut HashMap<String, SourceId>,
    live_sources: &mut HashMap<String, SourceId>,
    active_transforms: &mut HashMap<String, Vec<ActiveTransform>>,
    run_counter: &mut u64,
) -> bool {
    {
        match cmd {
            ScriptCommand::Shutdown => return true,
            ScriptCommand::RunScript { name, source } => {
                // A NUL byte can't go through CString; fail before running so a
                // bad buffer never emits a (partial or empty) source.
                if source.contains('\0') {
                    let _ = evt_tx.send(ScriptEvent::Error("source contains a NUL byte".into()));
                    let _ = evt_tx.send(ScriptEvent::Done);
                    return false;
                }
                let snapshot = store.load();
                let emit: crate::api::EmitBuffer = std::rc::Rc::default();
                let live: crate::api::LiveTransformBuffer = std::rc::Rc::default();
                let generation = *run_counter;
                *run_counter += 1;
                let run_result: Result<(), String> = Python::attach(|py| {
                    let g = globals.bind(py);
                    let delog = Bound::new(
                        py,
                        crate::api::Delog::new(
                            snapshot.clone(),
                            std::rc::Rc::clone(&emit),
                            std::rc::Rc::clone(&live),
                            name.clone(),
                            generation,
                        ),
                    )
                    .unwrap();
                    g.set_item("delog", delog).unwrap();
                    let code = std::ffi::CString::new(source.as_str()).expect("NUL checked above");
                    py.run(&code, Some(g), None)
                        .map_err(|e| format_pyerr(py, &e))
                });
                match run_result {
                    Ok(()) => {
                        // Register this run's live transforms against a single
                        // live-derived source, replacing this script-name's
                        // prior generation. Live-derived sources are appendable
                        // and never `close_source`d; the prior generation is
                        // torn down with `remove_source` (BRW-06 semantics:
                        // tombstones the source in the published snapshot,
                        // keeping its slot/rows valid for any reader still
                        // viewing it). Order: remove old -> open new -> record
                        // the new id (the new source has a fresh id, so removing
                        // first is safe).
                        let pending = live.borrow();
                        if pending.is_empty() {
                            active_transforms.remove(&name);
                            active_live.lock().unwrap().remove(&name);
                            // A script that previously registered a live
                            // transform and now registers none must not leave
                            // its old live-derived source active.
                            if let Some(prev) = live_sources.remove(&name) {
                                sender.remove_source(prev);
                            }
                        } else {
                            if let Some(prev) = live_sources.remove(&name) {
                                sender.remove_source(prev);
                            }
                            let derived_source = {
                                let mut sink = sender.file_sink();
                                sink.open_source(&format!("script:{name}"), SourceKind::LiveDerived)
                            };
                            live_sources.insert(name.clone(), derived_source);
                            let transforms = Python::attach(|py| {
                                pending
                                    .iter()
                                    .map(|p| ActiveTransform {
                                        spec: p.spec.clone(),
                                        callable: p.callable.clone_ref(py),
                                        source: derived_source,
                                        consecutive_errors: 0,
                                        disabled: false,
                                    })
                                    .collect::<Vec<_>>()
                            });
                            active_transforms.insert(name.clone(), transforms);
                            // Mirror the active specs to the shared map so the UI
                            // can tell this script has a live transform.
                            let specs: Vec<LiveTransformSpec> =
                                pending.iter().map(|p| p.spec.clone()).collect();
                            active_live.lock().unwrap().insert(name.clone(), specs);
                        }
                        drop(pending);

                        let topics = emit.borrow();
                        // A pure live-transform script emits no snapshot topics;
                        // publishing an empty Derived source would create a
                        // phantom `script:<name>` entry in the browser. Skip the
                        // open, but still tear down this name's prior emit source
                        // (a script that used to emit and now doesn't must clean
                        // up).
                        if topics.is_empty() {
                            if let Some(prev) = prev_sources.remove(&name) {
                                sender.remove_source(prev);
                            }
                        } else {
                            let mut sink = sender.file_sink();
                            match crate::emit::emit_topics(&mut sink, &name, &topics) {
                                Ok(new_id) => {
                                    if let Some(prev) = prev_sources.insert(name.clone(), new_id) {
                                        sender.remove_source(prev);
                                    }
                                }
                                Err(e) => {
                                    let _ = evt_tx.send(ScriptEvent::Error(e));
                                }
                            }
                        }
                    }
                    Err(msg) => {
                        // No partial source on failure.
                        let _ = evt_tx.send(ScriptEvent::Error(msg));
                    }
                }
                let _ = evt_tx.send(ScriptEvent::Done);
            }
            ScriptCommand::ValidateParser { name, source } => {
                let event = Python::attach(|py| {
                    let compiled = py
                        .import("builtins")
                        .and_then(|builtins| builtins.getattr("compile"))
                        .and_then(|compile| compile.call1((source, name.clone(), "exec")));
                    match compiled {
                        Ok(_) => ParserEvent::SyntaxValid { name },
                        Err(error) => {
                            metrics.add("python_parser_failures", 1);
                            ParserEvent::Failed {
                                parser_name: name,
                                path: None,
                                message: format_pyerr(py, &error),
                            }
                        }
                    }
                });
                let _ = evt_tx.send(ScriptEvent::Parser(event));
            }
            ScriptCommand::ParseFile {
                parser_name,
                source,
                path,
            } => run_parser_file(sender, metrics, evt_tx, parser_name, source, path),
            ScriptCommand::UnregisterLive { name } => {
                // Same teardown as the empty-rerun path: stop running the
                // callbacks and tombstone the appendable live-derived source.
                active_transforms.remove(&name);
                active_live.lock().unwrap().remove(&name);
                if let Some(prev) = live_sources.remove(&name) {
                    sender.remove_source(prev);
                }
                // Console confirmation; carries no command-completion semantics.
                let _ = evt_tx.send(ScriptEvent::Output(format!(
                    "# unregistered live transform '{name}'\n"
                )));
            }
            ScriptCommand::Eval(src) => {
                let snapshot = store.load();
                let emit: crate::api::EmitBuffer = std::rc::Rc::default();
                let live: crate::api::LiveTransformBuffer = std::rc::Rc::default();
                let generation = *run_counter;
                *run_counter += 1;
                Python::attach(|py| {
                    let g = globals.bind(py);
                    let delog = Bound::new(
                        py,
                        Delog::new(
                            snapshot.clone(),
                            std::rc::Rc::clone(&emit),
                            std::rc::Rc::clone(&live),
                            String::new(),
                            generation,
                        ),
                    )
                    .unwrap();
                    g.set_item("delog", delog).unwrap();
                });
                eval_line(globals, &src, evt_tx);
                let _ = evt_tx.send(ScriptEvent::Done);
            }
        }
    }
    false
}

enum ParserRunError {
    Python(PyErr),
    Other(String),
}

fn run_parser_file(
    sender: &IngestSender,
    metrics: &MetricsRegistry,
    evt_tx: &Sender<ScriptEvent>,
    parser_name: String,
    source: String,
    path: PathBuf,
) {
    let total_timer = metrics.scope("python_parser_total");
    let _ = evt_tx.send(ScriptEvent::Parser(ParserEvent::Reading {
        parser_name: parser_name.clone(),
        path: path.clone(),
    }));
    let raw_result = {
        let _read_timer = metrics.scope("python_parser_read");
        read_float32_file(&path)
    };
    let raw = match raw_result {
        Ok(raw) => raw,
        Err(error) => {
            drop(total_timer);
            finish_parser_failure(
                metrics,
                evt_tx,
                parser_name,
                Some(path),
                ParserRunError::Other(error.to_string()),
            );
            return;
        }
    };
    metrics.record("python_parser_input_bytes", raw.input_bytes as f32);
    let _ = evt_tx.send(ScriptEvent::Parser(ParserEvent::Running {
        parser_name: parser_name.clone(),
        path: path.clone(),
    }));

    let parsed = Python::attach(|py| -> Result<ParserOutput, ParserRunError> {
        let namespace = PyDict::new(py);
        let code = std::ffi::CString::new(source).map_err(|_| {
            ParserRunError::Python(PyValueError::new_err("source contains a NUL byte"))
        })?;
        let execution = {
            let _python_timer = metrics.scope("python_parser_python");
            (|| -> PyResult<Bound<'_, PyAny>> {
                py.run(&code, Some(&namespace), None)?;
                let parse = namespace.get_item("Parse")?.ok_or_else(|| {
                    PyValueError::new_err("parser must define callable Parse(raw_data)")
                })?;
                if !parse.is_callable() {
                    return Err(PyValueError::new_err(
                        "parser attribute Parse must be callable",
                    ));
                }
                let values = raw.values.into_pyarray(py);
                parse.call1((values,))
            })()
        };
        let result = execution.map_err(ParserRunError::Python)?;
        let _convert_timer = metrics.scope("python_parser_convert");
        parse_python_result(py, &result).map_err(ParserRunError::Python)
    });

    let output = match parsed {
        Ok(output) => output,
        Err(error) => {
            drop(total_timer);
            finish_parser_failure(metrics, evt_tx, parser_name, Some(path), error);
            return;
        }
    };
    metrics.record("python_parser_output_bytes", output.arrow_bytes as f32);
    let source_label = path.file_stem().or_else(|| path.file_name()).map_or_else(
        || path.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut sink = sender.file_sink();
    match emit_parser_output(&mut sink, &source_label, output) {
        Ok(summary) => {
            drop(total_timer);
            let _ = evt_tx.send(ScriptEvent::Parser(ParserEvent::Succeeded {
                parser_name,
                path,
                topics: summary.topic_count,
                rows: summary.row_count,
            }));
        }
        Err(message) => {
            drop(total_timer);
            finish_parser_failure(
                metrics,
                evt_tx,
                parser_name,
                Some(path),
                ParserRunError::Other(message),
            )
        }
    }
}

fn finish_parser_failure(
    metrics: &MetricsRegistry,
    evt_tx: &Sender<ScriptEvent>,
    parser_name: String,
    path: Option<PathBuf>,
    error: ParserRunError,
) {
    let message = match error {
        ParserRunError::Python(error) => Python::attach(|py| {
            if error.is_instance_of::<PyKeyboardInterrupt>(py) {
                metrics.add("python_parser_cancelled", 1);
            } else {
                metrics.add("python_parser_failures", 1);
            }
            format_pyerr(py, &error)
        }),
        ParserRunError::Other(message) => {
            metrics.add("python_parser_failures", 1);
            message
        }
    };
    let _ = evt_tx.send(ScriptEvent::Parser(ParserEvent::Failed {
        parser_name,
        path,
        message,
    }));
}

/// Evaluate one line: try as an expression (to report its repr), else exec it.
fn eval_line(globals: &Py<PyDict>, src: &str, evt_tx: &Sender<ScriptEvent>) {
    Python::attach(|py| {
        let g = globals.bind(py);
        let code = match std::ffi::CString::new(src) {
            Ok(c) => c,
            Err(_) => {
                let _ = evt_tx.send(ScriptEvent::Error("source contains a NUL byte".into()));
                return;
            }
        };
        match py.eval(&code, Some(g), None) {
            Ok(value) => {
                if !value.is_none() {
                    let repr = value.repr().map(|r| r.to_string()).unwrap_or_default();
                    let _ = evt_tx.send(ScriptEvent::Result(repr));
                }
            }
            Err(_) => {
                // Not an expression — run as a statement.
                if let Err(err) = py.run(&code, Some(g), None) {
                    let _ = evt_tx.send(ScriptEvent::Error(format_pyerr(py, &err)));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    /// Serializes tests that spawn a `ScriptEngine`: they share one process-global
    /// embedded CPython interpreter (and its global `sys.stdout`), so they must not
    /// run concurrently. `unwrap_or_else(into_inner)` ignores poisoning from a
    /// panicking test so one failure doesn't cascade.
    static ENGINE_LOCK: Mutex<()> = Mutex::new(());

    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::metrics::MetricsRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;
    use std::path::PathBuf;

    fn test_metrics() -> Arc<MetricsRegistry> {
        Arc::new(MetricsRegistry::new())
    }

    fn dummy_sender() -> delog_core::ingest::IngestSender {
        delog_core::ingest::ingest_channel().0
    }

    fn test_store_with_baro_alt() -> Arc<DataStore> {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("flight");
        let topic = id.add_topic(src, "BARO").unwrap();
        id.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![1.0]))];
        let chunk = Arc::new(Chunk::try_new(Int64Array::from(vec![0]), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snap = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();
        Arc::new(DataStore::from_snapshot(snap))
    }

    #[test]
    fn print_is_captured_as_output_events() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine =
            ScriptEngine::spawn(Arc::new(DataStore::new()), dummy_sender(), test_metrics());
        let _ = engine.send(ScriptCommand::Eval("print('hello')".into()));
        let mut text = String::new();
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Output(s) => text.push_str(&s),
                ScriptEvent::Done => break,
                ScriptEvent::Error(e) => panic!("{e}"),
                ScriptEvent::Result(_)
                | ScriptEvent::LiveBatchProcessed
                | ScriptEvent::Parser(_) => {}
            }
        }
        assert!(text.contains("hello"), "captured: {text:?}");
    }

    #[test]
    fn eval_returns_a_result_event() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine =
            ScriptEngine::spawn(Arc::new(DataStore::new()), dummy_sender(), test_metrics());
        let _ = engine.send(ScriptCommand::Eval("1 + 1".into()));
        let mut got_result = None;
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Result(r) => got_result = Some(r),
                ScriptEvent::Done => break,
                ScriptEvent::Error(e) => panic!("unexpected error: {e}"),
                ScriptEvent::Output(_)
                | ScriptEvent::LiveBatchProcessed
                | ScriptEvent::Parser(_) => {}
            }
        }
        assert_eq!(got_result.as_deref(), Some("2"));
    }

    #[test]
    fn python_can_read_a_field_via_delog() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let store = test_store_with_baro_alt();
        let engine = ScriptEngine::spawn(store, dummy_sender(), test_metrics());
        let _ = engine.send(ScriptCommand::Eval(
            "float(delog.field('flight/BARO/Alt').v[0])".into(),
        ));
        let mut result = None;
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Result(r) => result = Some(r),
                ScriptEvent::Done => break,
                ScriptEvent::Error(e) => panic!("{e}"),
                ScriptEvent::Output(_)
                | ScriptEvent::LiveBatchProcessed
                | ScriptEvent::Parser(_) => {}
            }
        }
        assert_eq!(result.as_deref(), Some("1.0"));
    }

    #[test]
    fn running_a_script_emits_a_derived_source() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use delog_core::ingest::ingest_channel;
        use delog_core::ingestor::{Ingestor, NullObserver};

        // Write side: a real ingestor thread we emit into.
        let ingestor = Ingestor::new(NullObserver);
        let write_store = ingestor.store();
        let (sender, receiver) = ingest_channel();
        let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

        // Read side: empty store (this script builds its own arrays, reads no fields).
        let engine = ScriptEngine::spawn(Arc::new(DataStore::new()), sender, test_metrics());
        let script = r#"
import numpy as np
t = np.array([0, 100, 200], dtype=np.int64)
out = delog.output(t, "Mag")
out.add_field("v", np.array([1.0, 2.0, 3.0]), unit="m")
"#;
        let _ = engine.send(ScriptCommand::RunScript {
            name: "test".into(),
            source: script.into(),
        });
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Done => break,
                ScriptEvent::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        drop(engine); // worker drops its sender clone -> ingest thread ends

        // Poll for the derived source (emission flows async through the channel).
        let mut found = false;
        for _ in 0..100 {
            let snap = write_store.load();
            if snap
                .sources
                .iter()
                .any(|s| s.entry.label == "script:test" && !s.entry.removed)
            {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(found, "derived source script:test not published");
        let _ = ingest_thread;
    }

    #[test]
    fn live_transform_decorator_registers_a_transform() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use delog_core::ingest::ingest_channel;
        use delog_core::ingestor::{Ingestor, NullObserver};

        // A running ingestor is required so that file_sink().open_source() can
        // get its reply (otherwise the worker blocks indefinitely on RunScript).
        let ingestor = Ingestor::new(NullObserver);
        let store = ingestor.store();
        let (sender, receiver) = ingest_channel();
        let _ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

        let engine = ScriptEngine::spawn(store, sender, test_metrics());

        let script = r#"
@delog.live_transform(
    topic="NAV_CONTROLLER_OUTPUT",
    fields=["nav_roll", "nav_pitch"],
    output_topic="NAV_RAD",
)
def convert(batch):
    return {}
"#;
        let _ = engine.send(ScriptCommand::RunScript {
            name: "nav_rad".into(),
            source: script.into(),
        });

        loop {
            match engine.recv_blocking() {
                ScriptEvent::Done => break,
                ScriptEvent::Error(e) => panic!("unexpected error: {e}"),
                _ => {}
            }
        }
        assert_eq!(engine.transform_specs().len(), 1);
        assert_eq!(engine.transform_specs()[0].topic, "NAV_CONTROLLER_OUTPUT");
        assert_eq!(engine.transform_specs()[0].output_topic, "NAV_RAD");
    }

    #[test]
    fn unregister_live_removes_the_transform_and_tombstones_its_source() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use delog_core::ingest::ingest_channel;
        use delog_core::ingestor::{Ingestor, NullObserver};

        let ingestor = Ingestor::new(NullObserver);
        let write_store = ingestor.store();
        let (sender, receiver) = ingest_channel();
        let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));
        let engine =
            ScriptEngine::spawn(Arc::new(DataStore::new()), sender.clone(), test_metrics());

        let script = r#"
@delog.live_transform(topic="A", fields=["v"], output_topic="B")
def f(batch):
    return {"x": batch.v}
"#;
        let _ = engine.send(ScriptCommand::RunScript {
            name: "live".into(),
            source: script.into(),
        });
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Done => break,
                ScriptEvent::Error(e) => panic!("unexpected error: {e}"),
                _ => {}
            }
        }
        assert!(
            engine.has_live_transform("live"),
            "the transform is registered after running the script"
        );

        // Unregister it. The worker emits an Output confirmation when done.
        let _ = engine.send(ScriptCommand::UnregisterLive {
            name: "live".into(),
        });
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Output(s) if s.contains("unregistered") => break,
                ScriptEvent::Error(e) => panic!("unexpected error: {e}"),
                _ => {}
            }
        }
        assert!(
            !engine.has_live_transform("live"),
            "the transform is gone after UnregisterLive"
        );

        // The live-derived source is tombstoned (`remove_source` publishes it).
        let removed = |snap: &StoreSnapshot| -> bool {
            snap.sources
                .iter()
                .find(|s| s.entry.label == "script:live")
                .is_some_and(|s| s.entry.removed)
        };
        let mut settled = false;
        for _ in 0..100 {
            if removed(&write_store.load()) {
                settled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(settled, "the script:live source is removed on unregister");

        drop(engine);
        drop(sender);
        let _ = ingest_thread.join();
    }

    #[test]
    #[ignore = "Py_AddPendingCall uses a global queue; when other tests run concurrently \
                the pending callback is consumed by another worker's eval loop before the \
                while-True loop processes it, causing a hang. Passes when run in isolation \
                (cargo test ... interrupt_stops). request_interrupt is retained for the real app \
                where a single interpreter runs on the worker thread."]
    fn interrupt_stops_a_long_loop_with_keyboardinterrupt() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine =
            ScriptEngine::spawn(Arc::new(DataStore::new()), dummy_sender(), test_metrics());
        let _ = engine.send(ScriptCommand::Eval("while True:\n    pass".into()));
        // Let the interpreter enter the loop, then interrupt.
        std::thread::sleep(std::time::Duration::from_millis(200));
        engine.request_interrupt();
        let mut err = None;
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Error(e) => err = Some(e),
                ScriptEvent::Done => break,
                _ => {}
            }
        }
        assert!(err.unwrap_or_default().contains("KeyboardInterrupt"));
    }

    #[test]
    fn rerunning_live_transform_removes_previous_generation() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use delog_core::ingest::ingest_channel;
        use delog_core::ingestor::{Ingestor, NullObserver};

        let ingestor = Ingestor::new(NullObserver);
        let write_store = ingestor.store();
        let (sender, receiver) = ingest_channel();
        let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

        let engine =
            ScriptEngine::spawn(Arc::new(DataStore::new()), sender.clone(), test_metrics());

        let script = r#"
@delog.live_transform(topic="A", fields=["v"], output_topic="B")
def f(batch):
    return {"x": batch.v}
"#;
        // Run the same live script twice. Each run opens a live-derived source
        // labelled exactly "script:live". A bare `open_source` does not publish
        // a snapshot (only a batch/close/remove does), so the live-derived
        // source becomes visible only once it is removed. The script emits no
        // snapshot topics (pure live transform), so NO Derived emit source is
        // published either (it would be a phantom). The second run must REMOVE
        // the first-generation live source.
        for _ in 0..2 {
            let _ = engine.send(ScriptCommand::RunScript {
                name: "live".into(),
                source: script.into(),
            });
            loop {
                match engine.recv_blocking() {
                    ScriptEvent::Done => break,
                    ScriptEvent::Error(e) => panic!("unexpected error: {e}"),
                    _ => {}
                }
            }
        }

        // Emission flows async through the ingest channel; poll until the store
        // settles. The first generation's live-derived source (label exactly
        // "script:live") must be tombstoned (`removed`) after the re-run.
        let first_live_removed = |snap: &StoreSnapshot| -> bool {
            snap.sources
                .iter()
                .find(|s| s.entry.label == "script:live")
                .is_some_and(|s| s.entry.removed)
        };
        // No active "script:live"-prefixed source must remain. A pure live
        // transform publishes no Derived emit source (the phantom-source bug),
        // and the second-generation live-derived source is not published until a
        // batch flows into it (bare open_source doesn't publish). So once the
        // first generation is tombstoned, zero active script:live sources exist.
        let active_script_sources = |snap: &StoreSnapshot| -> usize {
            snap.sources
                .iter()
                .filter(|s| s.entry.label.starts_with("script:live") && !s.entry.removed)
                .count()
        };
        let mut settled = false;
        for _ in 0..100 {
            let snap = write_store.load();
            if first_live_removed(&snap) && active_script_sources(&snap) == 0 {
                settled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            settled,
            "first-generation live source must be removed, leaving no active phantom emit source"
        );

        drop(engine);
        drop(sender);
        let _ = ingest_thread.join();
    }

    #[test]
    fn failing_script_emits_error_and_no_source() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use delog_core::ingest::ingest_channel;
        use delog_core::ingestor::{Ingestor, NullObserver};

        let ingestor = Ingestor::new(NullObserver);
        let write_store = ingestor.store();
        let (sender, receiver) = ingest_channel();
        let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

        let engine = ScriptEngine::spawn(Arc::new(DataStore::new()), sender, test_metrics());
        let script = "import numpy as np\nout = delog.output(np.array([0],dtype=np.int64),'X')\nout.add_field('v', np.array([1.0]))\nraise ValueError('boom')\n";
        let _ = engine.send(ScriptCommand::RunScript {
            name: "bad".into(),
            source: script.into(),
        });
        let mut err = None;
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Error(e) => err = Some(e),
                ScriptEvent::Done => break,
                _ => {}
            }
        }
        drop(engine);
        assert!(err.expect("error event").contains("boom"));
        // Failure emits nothing — give the ingest thread a moment, then confirm absence.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let snap = write_store.load();
        assert!(
            !snap
                .sources
                .iter()
                .any(|s| s.entry.label == "script:bad" && !s.entry.removed)
        );
        let _ = ingest_thread;
    }

    #[test]
    fn validate_parser_compiles_without_executing_source() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine =
            ScriptEngine::spawn(Arc::new(DataStore::new()), dummy_sender(), test_metrics());
        let _ = engine.send(ScriptCommand::ValidateParser {
            name: "side_effect.py".into(),
            source: "raise RuntimeError('must not run')\ndef Parse(raw_data):\n    return []\n"
                .into(),
        });
        assert_eq!(
            engine.recv_blocking(),
            ScriptEvent::Parser(ParserEvent::SyntaxValid {
                name: "side_effect.py".into()
            })
        );
    }

    #[test]
    fn validate_parser_reports_syntax_and_nul_errors_as_parser_failures() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let metrics = test_metrics();
        let engine = ScriptEngine::spawn(
            Arc::new(DataStore::new()),
            dummy_sender(),
            Arc::clone(&metrics),
        );
        for source in ["def Parse(:\n    pass", "x = '\0'"] {
            let _ = engine.send(ScriptCommand::ValidateParser {
                name: "bad.py".into(),
                source: source.into(),
            });
            match engine.recv_blocking() {
                ScriptEvent::Parser(ParserEvent::Failed {
                    parser_name,
                    path,
                    message,
                }) => {
                    assert_eq!(parser_name, "bad.py");
                    assert_eq!(path, None);
                    assert!(!message.is_empty());
                }
                event => panic!("unexpected event: {event:?}"),
            }
        }
        assert_eq!(metrics.counter("python_parser_failures"), Some(2));
    }

    #[test]
    fn parse_file_failure_is_one_parser_event_without_done() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let metrics = test_metrics();
        let engine = ScriptEngine::spawn(
            Arc::new(DataStore::new()),
            dummy_sender(),
            Arc::clone(&metrics),
        );
        let path = PathBuf::from("/definitely/missing/custom-parser.bin");
        let _ = engine.send(ScriptCommand::ParseFile {
            parser_name: "bad.py".into(),
            source: "def Parse(raw_data): return []".into(),
            path: path.clone(),
        });
        assert_eq!(
            engine.recv_blocking(),
            ScriptEvent::Parser(ParserEvent::Reading {
                parser_name: "bad.py".into(),
                path: path.clone(),
            })
        );
        match engine.recv_blocking() {
            ScriptEvent::Parser(ParserEvent::Failed {
                parser_name,
                path: failed_path,
                ..
            }) => {
                assert_eq!(parser_name, "bad.py");
                assert_eq!(failed_path, Some(path));
            }
            event => panic!("unexpected event: {event:?}"),
        }
        assert_eq!(metrics.counter("python_parser_failures"), Some(1));
        assert!(engine.drain_events().is_empty());
    }

    #[test]
    fn parser_keyboard_interrupt_counts_cancellation_not_failure() {
        let _guard = ENGINE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path =
            std::env::temp_dir().join(format!("delog-parser-cancel-{}.bin", std::process::id()));
        std::fs::write(&path, 1_f32.to_ne_bytes()).unwrap();
        let metrics = test_metrics();
        let engine = ScriptEngine::spawn(
            Arc::new(DataStore::new()),
            dummy_sender(),
            Arc::clone(&metrics),
        );
        let _ = engine.send(ScriptCommand::ParseFile {
            parser_name: "cancel.py".into(),
            source: "def Parse(raw_data):\n    raise KeyboardInterrupt()\n".into(),
            path: path.clone(),
        });
        loop {
            if matches!(
                engine.recv_blocking(),
                ScriptEvent::Parser(ParserEvent::Failed { .. })
            ) {
                break;
            }
        }
        assert_eq!(metrics.counter("python_parser_cancelled"), Some(1));
        assert_eq!(metrics.counter("python_parser_failures"), None);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn send_reports_a_disconnected_worker() {
        let (tx, rx) = channel();
        drop(rx);
        let (live_tx, _live_rx) = sync_channel(LIVE_TRANSFORM_QUEUE_CAP);
        let (_event_tx, events) = channel();
        let engine = ScriptEngine {
            tx,
            live_tx,
            events,
            handle: None,
            active_live: Arc::new(Mutex::new(HashMap::new())),
        };

        assert!(engine.send(ScriptCommand::Eval("1".into())).is_err());
    }
}
