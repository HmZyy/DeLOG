//! The script worker thread: owns the CPython interpreter for the app lifetime
//! and serves REPL evals over channels (SCR-02..05).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// A command sent from the UI thread to the worker.
pub enum ScriptCommand {
    /// Evaluate one REPL line in the persistent namespace.
    Eval(String),
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
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = channel::<ScriptCommand>();
        let (evt_tx, evt_rx) = channel::<ScriptEvent>();
        let handle = std::thread::Builder::new()
            .name("delog-script".into())
            .spawn(move || worker_loop(cmd_rx, evt_tx))
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

fn worker_loop(cmd_rx: Receiver<ScriptCommand>, evt_tx: Sender<ScriptEvent>) {
    // One persistent globals dict for the whole session.
    let globals: Py<PyDict> = Python::attach(|py| PyDict::new(py).unbind());

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            ScriptCommand::Shutdown => break,
            ScriptCommand::Eval(src) => {
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

    #[test]
    fn eval_returns_a_result_event() {
        let engine = ScriptEngine::spawn();
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
}
