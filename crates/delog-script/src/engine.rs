//! The script worker thread: owns the CPython interpreter for the app lifetime
//! and serves REPL evals over channels (SCR-02..05).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use delog_core::identity::SourceId;
use delog_core::ingest::IngestSender;
use delog_core::snapshot::DataStore;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::api::Delog;

/// A command sent from the UI thread to the worker.
pub enum ScriptCommand {
    /// Evaluate one REPL line in the persistent namespace.
    Eval(String),
    /// Run a full script buffer; on success emits one `script:<name>` source.
    RunScript { name: String, source: String },
    /// Stop the worker (sender dropped also stops it).
    Shutdown,
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
}

/// Handle to the worker thread. Dropping it shuts the worker down.
pub struct ScriptEngine {
    tx: Sender<ScriptCommand>,
    events: Receiver<ScriptEvent>,
    handle: Option<JoinHandle<()>>,
}

impl ScriptEngine {
    /// Spawn the interpreter worker. The interpreter and its global namespace
    /// live for the worker's lifetime, so REPL state persists across evals.
    pub fn spawn(store: Arc<DataStore>, sender: IngestSender) -> Self {
        let (cmd_tx, cmd_rx) = channel::<ScriptCommand>();
        let (evt_tx, evt_rx) = channel::<ScriptEvent>();
        let handle = std::thread::Builder::new()
            .name("delog-script".into())
            .spawn(move || worker_loop(store, sender, cmd_rx, evt_tx))
            .expect("spawn script thread");
        Self {
            tx: cmd_tx,
            events: evt_rx,
            handle: Some(handle),
        }
    }

    pub fn send(&self, cmd: ScriptCommand) {
        let _ = self.tx.send(cmd);
    }

    /// Drain any ready events without blocking (UI thread calls this per frame).
    pub fn drain_events(&self) -> Vec<ScriptEvent> {
        self.events.try_iter().collect()
    }

    #[cfg(test)]
    pub fn recv_blocking(&self) -> ScriptEvent {
        self.events.recv().expect("worker alive")
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
    cmd_rx: Receiver<ScriptCommand>,
    evt_tx: Sender<ScriptEvent>,
) {
    // One persistent globals dict for the whole session.
    let globals: Py<PyDict> = Python::attach(|py| PyDict::new(py).unbind());
    // Per-script-name source from the previous run, for replace-on-rerun.
    let mut prev_sources: HashMap<String, SourceId> = HashMap::new();

    // Install stdout/stderr capture once, before the command loop.
    Python::attach(|py| {
        let sys = py.import("sys").expect("import sys");
        let cap =
            Bound::new(py, OutputCapture { tx: evt_tx.clone() }).expect("build OutputCapture");
        sys.setattr("stdout", &cap).expect("set sys.stdout");
        sys.setattr("stderr", &cap).expect("set sys.stderr");
    });

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            ScriptCommand::Shutdown => break,
            ScriptCommand::RunScript { name, source } => {
                // A NUL byte can't go through CString; fail before running so a
                // bad buffer never emits a (partial or empty) source.
                if source.contains('\0') {
                    let _ = evt_tx.send(ScriptEvent::Error("source contains a NUL byte".into()));
                    let _ = evt_tx.send(ScriptEvent::Done);
                    continue;
                }
                let snapshot = store.load();
                let emit: crate::api::EmitBuffer = std::rc::Rc::default();
                let run_result = Python::attach(|py| {
                    let g = globals.bind(py);
                    let delog = Bound::new(
                        py,
                        crate::api::Delog::new(snapshot.clone(), std::rc::Rc::clone(&emit)),
                    )
                    .unwrap();
                    g.set_item("delog", delog).unwrap();
                    let code = std::ffi::CString::new(source.as_str()).expect("NUL checked above");
                    py.run(&code, Some(g), None)
                });
                match run_result {
                    Ok(()) => {
                        let topics = emit.borrow();
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
                    Err(err) => {
                        // No partial source on failure.
                        let _ = evt_tx.send(ScriptEvent::Error(format!("{err}")));
                    }
                }
                let _ = evt_tx.send(ScriptEvent::Done);
            }
            ScriptCommand::Eval(src) => {
                let snapshot = store.load();
                let emit: crate::api::EmitBuffer = std::rc::Rc::default();
                Python::attach(|py| {
                    let g = globals.bind(py);
                    let delog =
                        Bound::new(py, Delog::new(snapshot.clone(), std::rc::Rc::clone(&emit)))
                            .unwrap();
                    g.set_item("delog", delog).unwrap();
                });
                eval_line(&globals, &src, &evt_tx);
                let _ = evt_tx.send(ScriptEvent::Done);
            }
        }
    }
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
                    let _ = evt_tx.send(ScriptEvent::Error(format!("{err}")));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;

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
        let engine = ScriptEngine::spawn(Arc::new(DataStore::new()), dummy_sender());
        engine.send(ScriptCommand::Eval("print('hello')".into()));
        let mut text = String::new();
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Output(s) => text.push_str(&s),
                ScriptEvent::Done => break,
                ScriptEvent::Error(e) => panic!("{e}"),
                ScriptEvent::Result(_) => {}
            }
        }
        assert!(text.contains("hello"), "captured: {text:?}");
    }

    #[test]
    fn eval_returns_a_result_event() {
        let engine = ScriptEngine::spawn(Arc::new(DataStore::new()), dummy_sender());
        engine.send(ScriptCommand::Eval("1 + 1".into()));
        let mut got_result = None;
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Result(r) => got_result = Some(r),
                ScriptEvent::Done => break,
                ScriptEvent::Error(e) => panic!("unexpected error: {e}"),
                ScriptEvent::Output(_) => {}
            }
        }
        assert_eq!(got_result.as_deref(), Some("2"));
    }

    #[test]
    fn python_can_read_a_field_via_delog() {
        let store = test_store_with_baro_alt();
        let engine = ScriptEngine::spawn(store, dummy_sender());
        engine.send(ScriptCommand::Eval(
            "float(delog.field('flight/BARO/Alt').v[0])".into(),
        ));
        let mut result = None;
        loop {
            match engine.recv_blocking() {
                ScriptEvent::Result(r) => result = Some(r),
                ScriptEvent::Done => break,
                ScriptEvent::Error(e) => panic!("{e}"),
                ScriptEvent::Output(_) => {}
            }
        }
        assert_eq!(result.as_deref(), Some("1.0"));
    }

    #[test]
    fn running_a_script_emits_a_derived_source() {
        use delog_core::ingest::ingest_channel;
        use delog_core::ingestor::{Ingestor, NullObserver};

        // Write side: a real ingestor thread we emit into.
        let ingestor = Ingestor::new(NullObserver);
        let write_store = ingestor.store();
        let (sender, receiver) = ingest_channel();
        let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

        // Read side: empty store (this script builds its own arrays, reads no fields).
        let engine = ScriptEngine::spawn(Arc::new(DataStore::new()), sender);
        let script = r#"
import numpy as np
t = np.array([0, 100, 200], dtype=np.int64)
out = delog.output(t, "Mag")
out.add_field("v", np.array([1.0, 2.0, 3.0]), unit="m")
"#;
        engine.send(ScriptCommand::RunScript {
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
}
