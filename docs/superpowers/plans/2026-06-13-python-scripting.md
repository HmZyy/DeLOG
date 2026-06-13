# Python Scripting for Derived Fields — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an optional, build-gated Python (embedded CPython) scripting capability that reads the loaded dataset and emits new derived fields/topics through the existing ingestion path, with an in-app interactive REPL + code editor and a reusable global library of saved scripts.

**Architecture:** A new `delog-script` crate (`delog-app → delog-script → delog-core`) owns a long-lived worker thread holding the CPython interpreter. It reads via `StoreSnapshot` and writes by submitting `ParsedBatch`es through the existing `IngestSink` as a `SourceKind::Derived` source, so derived data inherits chunking/stats/caching/GPU/layout for free. Python weight lives behind a `python` feature on `delog-script` and a `scripting` feature on `delog-app`, both OFF by default — `cargo build --workspace` needs no Python.

**Tech Stack:** Rust (edition 2024), `pyo3` (embedded CPython), `numpy` (rust-numpy), `arrow`, `egui`/`eframe`, `egui_code_editor`.

**Spec:** `docs/superpowers/specs/2026-06-13-python-scripting-design.md`

**Conventions for every task:** clippy must be clean (`cargo clippy --workspace -- -D warnings`, and for python-gated code `cargo clippy -p delog-script --features python -- -D warnings`); `cargo fmt --all` before each commit; update the PLAN.md §22 checklist entry **in the same commit** as the feature (per PLAN §0). pyo3 code targets **pyo3 0.24 / numpy 0.24** idioms — if the build flags an API drift, adapt to the installed version (the build/test step is the source of truth).

---

## Task 1: `SourceKind::Derived` in delog-core (`SCR-01a`)

**Files:**
- Modify: `crates/delog-core/src/ingest.rs` (the `SourceKind` enum, ~line 36)
- Modify: `crates/delog-core/src/ingestor.rs` (`open_source` seal-rows match, ~line 160)
- Test: `crates/delog-core/src/ingestor.rs` (tests module)

- [ ] **Step 1: Write the failing test**

In `crates/delog-core/src/ingestor.rs` tests module (after `live_source_seals_at_the_row_threshold_and_publishes`), add:

```rust
#[test]
fn derived_source_seals_at_the_file_threshold() {
    let mut ing = Ingestor::new(NullObserver);
    let source = open(&mut ing, "script:test", SourceKind::Derived);

    // One batch below the file threshold does not seal.
    ing.process(IngestMsg::Batch(batch(source, "DERIVED", &[1, 2, 3])));
    assert_eq!(ing.chunks_sealed(), 0);

    // Closing flushes the tail.
    ing.process(IngestMsg::CloseSource {
        source,
        summary: ParseSummary::default(),
    });
    assert_eq!(ing.chunks_sealed(), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p delog-core derived_source_seals_at_the_file_threshold`
Expected: FAIL — `no variant named Derived found for enum SourceKind`.

- [ ] **Step 3: Add the variant**

In `crates/delog-core/src/ingest.rs`, extend the enum:

```rust
/// What kind of source produced a stream of batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A log file parsed start-to-finish; may block on backpressure (§5).
    File,
    /// A live link; must never block — full channel drops the batch (ING-03).
    Live,
    /// A programmatically-generated source (derived fields, scripts): bounded
    /// like a file, sealed at the file threshold, but flagged distinctly so the
    /// UI can present it apart from opened files (PLAN.md §17.3, SCR-01).
    Derived,
}
```

- [ ] **Step 4: Give it file-like seal semantics**

In `crates/delog-core/src/ingestor.rs`, `open_source`, update the match:

```rust
let seal_rows = match kind {
    SourceKind::File | SourceKind::Derived => FILE_CHUNK_ROWS,
    SourceKind::Live => LIVE_CHUNK_ROWS,
};
```

Then build to surface any other non-exhaustive `match` on `SourceKind` (e.g. `flush_aged_live` filters `== SourceKind::Live`, which stays correct):

Run: `cargo build -p delog-core`
Expected: compiles, or points you at any exhaustive match to extend.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p delog-core derived_source_seals_at_the_file_threshold`
Expected: PASS.

- [ ] **Step 6: Mark the checklist and commit**

Add to PLAN.md §22 a new `## Scripting (SCR)` section (place it after the Analysis `ANA-*` block) with:
```
- [~] **SCR-01** — Derived-source substrate: `SourceKind::Derived` (file-like seal) + `IngestMsg::RemoveSource`. Shared with ANA-04 and §4.6.
```

```bash
cargo fmt --all
git add crates/delog-core/src/ingest.rs crates/delog-core/src/ingestor.rs PLAN.md
git commit -m "core: add SourceKind::Derived with file-like seal semantics (SCR-01)"
```

---

## Task 2: `IngestMsg::RemoveSource` wiring (`SCR-01b`)

**Files:**
- Modify: `crates/delog-core/src/ingest.rs` (`IngestMsg` enum ~line 95; `IngestSink` trait + impls; `IngestSender`)
- Modify: `crates/delog-core/src/ingestor.rs` (`process` dispatch; new `remove_source` handler)
- Test: `crates/delog-core/src/ingestor.rs`

- [ ] **Step 1: Write the failing test**

In `crates/delog-core/src/ingestor.rs` tests module:

```rust
#[test]
fn remove_source_drops_its_stores_and_republishes() {
    let mut ing = Ingestor::new(NullObserver);
    let store = ing.store();
    let source = open(&mut ing, "script:test", SourceKind::Derived);
    ing.process(IngestMsg::Batch(batch(source, "DERIVED", &[1, 2, 3])));
    ing.process(IngestMsg::CloseSource {
        source,
        summary: ParseSummary::default(),
    });
    let before = store.load();
    let topic = before
        .topics
        .iter()
        .find(|t| t.entry.name == "DERIVED")
        .unwrap()
        .entry
        .id;
    assert!(before.topic_store(topic).is_some());

    ing.process(IngestMsg::RemoveSource { source });

    let after = store.load();
    assert!(after.epoch > before.epoch, "removal publishes a new epoch");
    assert!(!after.is_source_live(source));
    assert!(after.topic_store(topic).is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p delog-core remove_source_drops_its_stores_and_republishes`
Expected: FAIL — `no variant named RemoveSource`.

- [ ] **Step 3: Add the message variant**

In `crates/delog-core/src/ingest.rs`, add to `IngestMsg`:

```rust
    /// Remove a source: tombstone it and its topics/fields in the registry,
    /// drop their stores, and republish a snapshot without them (§4.6, SCR-01).
    RemoveSource {
        source: SourceId,
    },
```

- [ ] **Step 4: Add a sink method to request removal**

In `crates/delog-core/src/ingest.rs`, on `impl IngestSender` (next to `set_source_offset`):

```rust
    /// Request removal of a source (§4.6, SCR-01). Blocking like a file sink;
    /// a no-op once the ingest thread is gone.
    pub fn remove_source(&self, source: SourceId) {
        let _ = self.tx.send(IngestMsg::RemoveSource { source });
    }
```

- [ ] **Step 5: Handle it in the ingestor**

In `crates/delog-core/src/ingestor.rs`, `process`, add an arm:

```rust
            IngestMsg::RemoveSource { source } => self.remove_source(source),
```

Then add the method on `impl<O: IngestObserver> Ingestor<O>` (near `flush_source`):

```rust
    /// Tombstone a source and drop every store it owned, then republish.
    fn remove_source(&mut self, source: SourceId) {
        // Flush any pending rows first so the seal path does not race the drop.
        self.flush_source(source);
        let Some(removed) = self.identity.remove_source(source) else {
            return;
        };
        for topic in &removed.topics {
            self.stores.remove(topic);
            self.topic_max_ts.remove(topic);
        }
        self.sources.remove(&source);
        self.publish();
    }
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p delog-core remove_source_drops_its_stores_and_republishes`
Expected: PASS.

- [ ] **Step 7: Verify clippy + commit**

Run: `cargo clippy -p delog-core -- -D warnings` → clean.

```bash
cargo fmt --all
git add crates/delog-core/src/ingest.rs crates/delog-core/src/ingestor.rs
git commit -m "core: wire IngestMsg::RemoveSource (tombstone + drop stores + republish) (SCR-01)"
```

---

## Task 3: Workspace deps + `delog-script` crate scaffold (`SCR-02a`)

This task creates the crate so it compiles in `cargo build --workspace` **without** Python (the `python` feature is off by default).

**Files:**
- Modify: `Cargo.toml` (workspace members + `[workspace.dependencies]`)
- Create: `crates/delog-script/Cargo.toml`
- Create: `crates/delog-script/src/lib.rs`

- [ ] **Step 1: Add pinned dependencies to the workspace table**

In root `Cargo.toml`, add `"crates/delog-script",` to `[workspace] members`, and under `[workspace.dependencies]` add (after the intra-workspace block):

```toml
delog-script = { path = "crates/delog-script" }
```

and in the third-party section:

```toml
# scripting (optional; behind feature flags — PLAN.md §17.3, SCR-*)
pyo3 = { version = "0.24", features = ["auto-initialize"] }
numpy = "0.24"
egui_code_editor = "0.2"
```

> If `cargo` later reports `egui_code_editor 0.2` is incompatible with `egui 0.34`, pin the highest `egui_code_editor` version whose `egui` dependency matches `0.34`, then `cargo update -p egui_code_editor`.

- [ ] **Step 2: Create the crate manifest**

Create `crates/delog-script/Cargo.toml`:

```toml
[package]
name = "delog-script"
description = "DeLOG — optional embedded-Python scripting for derived fields"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[features]
default = []
# Pulls in the embedded CPython interpreter + numpy. OFF by default so
# `cargo build --workspace` needs no Python toolchain (PLAN.md §17.3).
python = ["dep:pyo3", "dep:numpy"]

[dependencies]
delog-core = { workspace = true }

pyo3 = { workspace = true, optional = true }
numpy = { workspace = true, optional = true }
```

- [ ] **Step 3: Create a lib that compiles with and without the feature**

Create `crates/delog-script/src/lib.rs`:

```rust
//! Optional embedded-Python scripting for derived fields (PLAN.md §17.3, SCR-*).
//!
//! With the `python` feature off (the default) this crate carries only the
//! feature-independent script *library* (file persistence, SCR-06); the
//! interpreter engine lives behind `python`.

pub mod library;

#[cfg(feature = "python")]
pub mod engine;

#[cfg(feature = "python")]
pub use engine::{ScriptCommand, ScriptEngine, ScriptEvent};
```

- [ ] **Step 4: Create an empty library module placeholder (filled in Task 12)**

Create `crates/delog-script/src/library.rs`:

```rust
//! Persistent global script library: `.py` files in the app config dir (SCR-06).
```

- [ ] **Step 5: Build the whole workspace without Python**

Run: `cargo build --workspace`
Expected: PASS, with no requirement for libpython.

Run: `cargo build -p delog-script --features python`
Expected: PASS (requires Python dev headers installed locally). If Python is not installed and this fails to link, that is expected on this machine; note it and continue — CI runs the feature build where Python is present.

- [ ] **Step 6: Commit**

Update PLAN.md §22:
```
- [~] **SCR-02** — `delog-script` crate scaffold; `python` feature pins pyo3 + numpy.
```

```bash
cargo fmt --all
git add Cargo.toml Cargo.lock crates/delog-script/
git commit -m "script: scaffold delog-script crate behind python feature (SCR-02)"
```

---

## Task 4: Interpreter worker thread + command/event channels (`SCR-02b`)

All code in this task is under `#[cfg(feature = "python")]` and tested with `--features python`.

**Files:**
- Create: `crates/delog-script/src/engine.rs`
- Test: `crates/delog-script/src/engine.rs` (tests module, feature-gated)

- [ ] **Step 1: Write the failing test**

Create `crates/delog-script/src/engine.rs`:

```rust
//! The script worker thread: owns the CPython interpreter for the app lifetime
//! and serves REPL evals and script runs over channels (SCR-02..05).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_returns_a_result_event() {
        let engine = ScriptEngine::spawn();
        engine.send(ScriptCommand::Eval("1 + 1".into()));
        // Collect events until Done.
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p delog-script --features python eval_returns_a_result_event`
Expected: FAIL — `ScriptEngine::spawn` / `send` / `recv_blocking` not found.

- [ ] **Step 3: Implement the worker**

Add to `crates/delog-script/src/engine.rs` (above the tests module):

```rust
use pyo3::prelude::*;
use pyo3::types::PyDict;

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

    /// Block for the next event (tests only).
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
    let globals: Py<PyDict> = Python::with_gil(|py| PyDict::new(py).into());

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
    Python::with_gil(|py| {
        let g = globals.bind(py);
        match py.eval(&std::ffi::CString::new(src).unwrap(), Some(g), None) {
            Ok(value) => {
                if !value.is_none() {
                    let repr = value.repr().map(|r| r.to_string()).unwrap_or_default();
                    let _ = evt_tx.send(ScriptEvent::Result(repr));
                }
            }
            Err(_) => {
                // Not an expression — run as a statement.
                match py.run(&std::ffi::CString::new(src).unwrap(), Some(g), None) {
                    Ok(()) => {}
                    Err(err) => {
                        let _ = evt_tx.send(ScriptEvent::Error(format!("{err}")));
                    }
                }
            }
        }
    });
}
```

> pyo3 0.24 `eval`/`run` take a `&CStr`. If the installed pyo3 differs (older uses `&str`), drop the `CString::new(...).unwrap()` wrapping and pass `src` directly — the build error will tell you.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p delog-script --features python eval_returns_a_result_event`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p delog-script --features python -- -D warnings
git add crates/delog-script/src/engine.rs crates/delog-script/src/lib.rs
git commit -m "script: interpreter worker thread with persistent namespace + REPL eval (SCR-02)"
```

---

## Task 5: The `delog` Python object — `sources()` and `field()` → numpy (`SCR-03a`)

**Files:**
- Create: `crates/delog-script/src/api.rs` (the `delog` pyclass, `DelogField`, the Arrow→numpy reader)
- Modify: `crates/delog-script/src/lib.rs` (add `#[cfg(feature="python")] mod api;`)
- Modify: `crates/delog-script/src/engine.rs` (inject `delog` into globals; carry an `Arc<StoreSnapshot>`)
- Test: `crates/delog-script/src/api.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/delog-script/src/api.rs` with the module header and a test that exercises the not-yet-implemented `materialize_field`:

```rust
//! The `delog` Python API object and the Arrow→numpy read path (SCR-03).

use std::sync::Arc;

use delog_core::field_view::FieldView;
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;

    #[test]
    fn materialize_field_concatenates_chunks_in_time_order() {
        let mut id = IdentityRegistry::new();
        let src = id.add_source("flight");
        let topic = id.add_topic(src, "BARO").unwrap();
        let alt = id.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let c1: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![1.0, 2.0]))];
        let c2: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![3.0]))];
        let chunk1 = Arc::new(Chunk::try_new(Int64Array::from(vec![10, 20]), c1, &schema).unwrap());
        let chunk2 = Arc::new(Chunk::try_new(Int64Array::from(vec![30]), c2, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk1, chunk2]).unwrap());
        let snap = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();

        let (t, v) = materialize_field(&snap, alt).unwrap();
        assert_eq!(t, vec![10, 20, 30]);
        assert_eq!(v, vec![1.0, 2.0, 3.0]);
    }
}
```

- [ ] **Step 2: Make `FieldView` expose what the reader needs**

The reader needs the field's column index and a row→f64 read. Add small helpers in `crates/delog-core/src/field_view.rs`:

On `impl<'a> FieldView<'a>` add:
```rust
    /// Column index of this field inside its topic schema.
    pub fn col_index(&self) -> usize {
        self.col_index
    }
```

And make the existing private `value_at` usable: add a public free function in `field_view.rs`:
```rust
/// Read row `row` of `array` as `f64` (NaN for nulls/non-numeric), for script
/// materialization (SCR-03).
pub fn array_row_as_f64(array: &dyn Array, row: usize) -> f64 {
    match value_at(array, row) {
        SampleValue::Int(v) => v as f64,
        SampleValue::UInt(v) => v as f64,
        SampleValue::Float(v) => v,
        SampleValue::Bool(b) => b as i64 as f64,
        SampleValue::Utf8(_) | SampleValue::Null => f64::NAN,
    }
}
```

> Note: `SampleValue::Float` already returns NaN values as-is here (we bypass `as_f64`'s NaN filtering) so **gap markers are preserved** end to end.

- [ ] **Step 3: Implement `materialize_field` using the core helpers**

In `crates/delog-script/src/api.rs`, above the `tests` module, add:

```rust
use delog_core::field_view::array_row_as_f64;

/// Materialize a field as `(times_us: Vec<i64>, values: Vec<f64>)` by walking
/// its chunks in time order. This concatenates chunk buffers — the One Copy for
/// script consumption, off the render hot path.
// ZC-EXCEPTION: script materialization (PLAN.md §4.5) — a deliberate copy of the
// canonical field into a contiguous host buffer for numpy; counted, off-path.
pub fn materialize_field(
    snapshot: &StoreSnapshot,
    field: FieldId,
) -> Result<(Vec<i64>, Vec<f64>), String> {
    let view = FieldView::new(snapshot, field).map_err(|e| e.to_string())?;
    let col = view.col_index();
    let range = snapshot
        .global_time_range()
        .ok_or_else(|| "field has no data".to_string())?;
    let mut times = Vec::new();
    let mut values = Vec::new();
    for chunk in view.chunks_overlapping(range) {
        for row in 0..chunk.len() {
            times.push(chunk.t.value(row));
            values.push(array_row_as_f64(chunk.cols[col].as_ref(), row));
        }
    }
    Ok((times, values))
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p delog-script --features python materialize_field_concatenates_chunks_in_time_order`
Expected: PASS.

- [ ] **Step 5: Add the `delog` pyclass with `sources()` and `field()`**

Append to `crates/delog-script/src/api.rs`:

```rust
use numpy::IntoPyArray;
use pyo3::prelude::*;
use pyo3::types::PyList;

/// The `delog` object injected into the interpreter namespace. Holds the
/// snapshot pinned for the current run/eval. `unsendable`: it lives only on the
/// worker thread under the GIL.
#[pyclass(unsendable, name = "Delog")]
pub struct Delog {
    snapshot: Arc<StoreSnapshot>,
}

impl Delog {
    pub fn new(snapshot: Arc<StoreSnapshot>) -> Self {
        Self { snapshot }
    }

    fn resolve_path(&self, path: &str) -> Result<FieldId, String> {
        // path is "source/topic/field" by display label.
        let mut parts = path.splitn(3, '/');
        let (s, t, f) = match (parts.next(), parts.next(), parts.next()) {
            (Some(s), Some(t), Some(f)) => (s, t, f),
            _ => return Err(format!("bad field path '{path}' (want source/topic/field)")),
        };
        for src in self.snapshot.sources.iter() {
            if src.entry.removed || src.entry.label != s {
                continue;
            }
            for &topic_id in src.topics.iter() {
                let Some(topic) = self.snapshot.topic(topic_id) else { continue };
                if topic.entry.removed || topic.entry.name != t {
                    continue;
                }
                for fe in self.snapshot.fields.iter() {
                    if !fe.removed && fe.topic == topic_id && fe.name == f {
                        return Ok(fe.id);
                    }
                }
            }
        }
        Err(format!("field path '{path}' not found"))
    }
}

#[pymethods]
impl Delog {
    /// List every live field as "source/topic/field".
    fn sources(&self, py: Python<'_>) -> Py<PyList> {
        let mut paths: Vec<String> = Vec::new();
        for src in self.snapshot.sources.iter() {
            if src.entry.removed {
                continue;
            }
            for &topic_id in src.topics.iter() {
                let Some(topic) = self.snapshot.topic(topic_id) else { continue };
                if topic.entry.removed {
                    continue;
                }
                for fe in self.snapshot.fields.iter() {
                    if !fe.removed && fe.topic == topic_id {
                        paths.push(format!("{}/{}/{}", src.entry.label, topic.entry.name, fe.name));
                    }
                }
            }
        }
        PyList::new(py, paths).unwrap().into()
    }

    /// Read a field as a `DelogField` carrying numpy arrays.
    fn field(&self, py: Python<'_>, path: &str) -> PyResult<DelogField> {
        let id = self
            .resolve_path(path)
            .map_err(pyo3::exceptions::PyKeyError::new_err)?;
        let (t, v) = materialize_field(&self.snapshot, id)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        Ok(DelogField {
            t: t.into_pyarray(py).into(),
            v: v.into_pyarray(py).into(),
        })
    }
}

/// A field handed to Python: `.t` int64 µs numpy, `.v` float64 numpy.
#[pyclass(unsendable, name = "DelogField")]
pub struct DelogField {
    #[pyo3(get)]
    t: Py<numpy::PyArray1<i64>>,
    #[pyo3(get)]
    v: Py<numpy::PyArray1<f64>>,
}
```

- [ ] **Step 6: Inject `delog` into the namespace per eval**

In `crates/delog-script/src/engine.rs`, give the engine a snapshot source and set `delog` in globals before each eval. Add a field and a setter to `ScriptEngine` (and the worker), e.g. store an `Arc<DataStore>` and load a fresh snapshot per command:

- Add to `engine.rs` imports: `use std::sync::Arc; use delog_core::snapshot::DataStore; use crate::api::Delog;`
- Change `spawn` to `spawn(store: Arc<DataStore>)` and pass `store` into `worker_loop`.
- In `worker_loop`, before handling each `Eval`, do:

```rust
let snapshot = store.load();
Python::with_gil(|py| {
    let g = globals.bind(py);
    let delog = Bound::new(py, Delog::new(snapshot.clone())).unwrap();
    g.set_item("delog", delog).unwrap();
});
```

Update the Task 4 test `eval_returns_a_result_event` to build a `DataStore`:
```rust
let engine = ScriptEngine::spawn(Arc::new(DataStore::new()));
```

- [ ] **Step 7: Add an integration test that reads a field from Python**

Add to `engine.rs` tests (helper builds a `DataStore` with one BARO/Alt field; reuse the snapshot-building pattern from `api.rs` tests, then `data_store.publish(...)`):

```rust
#[test]
fn python_can_read_a_field_via_delog() {
    let store = test_store_with_baro_alt(); // Arc<DataStore>, helper in this module
    let engine = ScriptEngine::spawn(store);
    engine.send(ScriptCommand::Eval("float(delog.field('flight/BARO/Alt').v[0])".into()));
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
```

Write `test_store_with_baro_alt()` in the tests module using the `IdentityRegistry`/`Chunk`/`TopicStore`/`StoreSnapshot`/`DataStore::from_snapshot` pattern (see `snapshot.rs` tests for the exact calls).

- [ ] **Step 8: Run tests, clippy, commit**

Run: `cargo test -p delog-script --features python`
Expected: PASS.
Run: `cargo clippy -p delog-script --features python -- -D warnings` → clean. Also `cargo clippy -p delog-core -- -D warnings` (you touched field_view).

```bash
cargo fmt --all
git add crates/delog-script/src/api.rs crates/delog-script/src/lib.rs crates/delog-script/src/engine.rs crates/delog-core/src/field_view.rs PLAN.md
git commit -m "script: delog.sources()/field() with Arrow->numpy read (ZC-EXCEPTION) (SCR-03)"
```

---

## Task 6: `resample_prev` helper (`SCR-03b`)

**Files:**
- Modify: `crates/delog-script/src/api.rs` (`resample_prev` free fn + `Delog` method)
- Test: `crates/delog-script/src/api.rs` (unit + proptest)

- [ ] **Step 1: Add proptest dev-dependency**

In `crates/delog-script/Cargo.toml`:
```toml
[dev-dependencies]
proptest = { workspace = true }
arrow = { workspace = true }
```

- [ ] **Step 2: Write the failing tests**

In `api.rs` tests module:

```rust
#[test]
fn resample_prev_picks_the_last_value_at_or_before_each_base_time() {
    let src_t = vec![0_i64, 100, 200];
    let src_v = vec![10.0_f64, 20.0, 30.0];
    let base = vec![-5_i64, 0, 50, 100, 250];
    let out = super::resample_prev(&src_t, &src_v, &base);
    // before first sample -> NaN; then prev-sample hold.
    assert!(out[0].is_nan());
    assert_eq!(out[1], 10.0);
    assert_eq!(out[2], 10.0);
    assert_eq!(out[3], 20.0);
    assert_eq!(out[4], 30.0);
}

proptest::proptest! {
    #[test]
    fn resample_prev_matches_naive_scan(
        src in proptest::collection::vec((0i64..1000, -1e6f64..1e6), 1..50),
        base in proptest::collection::vec(0i64..1000, 1..50),
    ) {
        // Sort + dedup source by time (precondition: sorted timestamps).
        let mut src = src;
        src.sort_by_key(|(t, _)| *t);
        src.dedup_by_key(|(t, _)| *t);
        let st: Vec<i64> = src.iter().map(|(t, _)| *t).collect();
        let sv: Vec<f64> = src.iter().map(|(_, v)| *v).collect();
        let got = super::resample_prev(&st, &sv, &base);
        for (i, &bt) in base.iter().enumerate() {
            // naive: last index with st[idx] <= bt
            let naive = st.iter().rposition(|&t| t <= bt).map(|idx| sv[idx]);
            match naive {
                Some(v) => proptest::prop_assert_eq!(got[i], v),
                None => proptest::prop_assert!(got[i].is_nan()),
            }
        }
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p delog-script --features python resample_prev`
Expected: FAIL — `resample_prev` not found.

- [ ] **Step 4: Implement**

In `api.rs` (free function):

```rust
/// Prev-sample resample: for each `base` time, the source value at the latest
/// source timestamp `<= base` (NaN before the first sample). `src_t` must be
/// sorted ascending. O(n+m) via a merge walk.
pub fn resample_prev(src_t: &[i64], src_v: &[f64], base: &[i64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(base.len());
    for &bt in base {
        // binary search: rightmost src_t <= bt
        let idx = match src_t.binary_search(&bt) {
            Ok(i) => Some(i),
            Err(0) => None,
            Err(i) => Some(i - 1),
        };
        out.push(idx.map(|i| src_v[i]).unwrap_or(f64::NAN));
    }
    out
}
```

Add the `Delog` method (numpy in/out):
```rust
#[pymethods]
impl Delog {
    // ... existing methods ...

    /// Prev-sample resample a field's values onto `base_times`.
    fn resample_prev(
        &self,
        py: Python<'_>,
        field: &DelogField,
        base_times: numpy::PyReadonlyArray1<i64>,
    ) -> PyResult<Py<numpy::PyArray1<f64>>> {
        let t = field.t.bind(py).readonly();
        let v = field.v.bind(py).readonly();
        let out = resample_prev(t.as_slice()?, v.as_slice()?, base_times.as_slice()?);
        Ok(out.into_pyarray(py).into())
    }
}
```

> If a single `#[pymethods] impl` block per type is required by your pyo3 version, merge this method into the existing block from Task 5 rather than adding a second block.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p delog-script --features python resample_prev`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p delog-script --features python -- -D warnings
git add crates/delog-script/src/api.rs crates/delog-script/Cargo.toml Cargo.lock
git commit -m "script: delog.resample_prev prev-sample alignment helper + proptest (SCR-03)"
```

---

## Task 7: `output(name, times)` builder + emit buffer (`SCR-03c`)

**Files:**
- Modify: `crates/delog-script/src/api.rs` (`EmitBuffer`, `DelogOutput`, `Delog::output`)
- Test: `crates/delog-script/src/api.rs`

- [ ] **Step 1: Write the failing test**

In `api.rs` tests:

```rust
#[test]
fn output_builder_collects_topics_and_fields() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let buf: Rc<RefCell<Vec<super::PendingTopic>>> = Rc::default();
    let mut topic = super::PendingTopic::new("Mag".into(), vec![0, 100, 200]);
    topic.add_field("x".into(), vec![1.0, 2.0, 3.0], Some("m".into())).unwrap();
    topic
        .add_field("y".into(), vec![4.0, 5.0, 6.0], None)
        .unwrap();
    // length mismatch is rejected
    assert!(topic.add_field("bad".into(), vec![1.0], None).is_err());
    buf.borrow_mut().push(topic);
    assert_eq!(buf.borrow()[0].fields.len(), 2);
    assert_eq!(buf.borrow()[0].times.len(), 3);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p delog-script --features python output_builder_collects_topics_and_fields`
Expected: FAIL — `PendingTopic` not found.

- [ ] **Step 3: Implement the buffer types**

In `api.rs`:

```rust
use std::cell::RefCell;
use std::rc::Rc;

/// A field accumulated for emission: name + values aligned to its topic's times.
pub struct PendingField {
    pub name: String,
    pub values: Vec<f64>,
    pub unit: Option<String>,
}

/// One derived topic the script is building. Every field shares `times`.
pub struct PendingTopic {
    pub name: String,
    pub times: Vec<i64>,
    pub fields: Vec<PendingField>,
}

impl PendingTopic {
    pub fn new(name: String, times: Vec<i64>) -> Self {
        Self { name, times, fields: Vec::new() }
    }

    pub fn add_field(
        &mut self,
        name: String,
        values: Vec<f64>,
        unit: Option<String>,
    ) -> Result<(), String> {
        if values.len() != self.times.len() {
            return Err(format!(
                "field '{name}': {} values but topic '{}' has {} timestamps",
                values.len(),
                self.name,
                self.times.len()
            ));
        }
        self.fields.push(PendingField { name, values, unit });
        Ok(())
    }
}

pub type EmitBuffer = Rc<RefCell<Vec<PendingTopic>>>;
```

- [ ] **Step 4: Wire `EmitBuffer` into `Delog` and add the pyclass output builder**

Change `Delog` to carry an `EmitBuffer`:
```rust
#[pyclass(unsendable, name = "Delog")]
pub struct Delog {
    snapshot: Arc<StoreSnapshot>,
    emit: EmitBuffer,
}

impl Delog {
    pub fn new(snapshot: Arc<StoreSnapshot>, emit: EmitBuffer) -> Self {
        Self { snapshot, emit }
    }
    pub fn emit_buffer(&self) -> EmitBuffer {
        Rc::clone(&self.emit)
    }
}
```

Add the output method + pyclass:
```rust
#[pymethods]
impl Delog {
    /// Begin a derived topic; `times_us` is shared by all its fields.
    fn output(&self, times_us: numpy::PyReadonlyArray1<i64>, name: &str) -> PyResult<DelogOutput> {
        let times = times_us.as_slice()?.to_vec();
        let idx = {
            let mut buf = self.emit.borrow_mut();
            buf.push(PendingTopic::new(name.to_string(), times));
            buf.len() - 1
        };
        Ok(DelogOutput { emit: Rc::clone(&self.emit), index: idx })
    }
}

/// Returned by `delog.output(...)`; `add_field` appends to the pending topic.
#[pyclass(unsendable, name = "DelogOutput")]
pub struct DelogOutput {
    emit: EmitBuffer,
    index: usize,
}

#[pymethods]
impl DelogOutput {
    #[pyo3(signature = (name, values, unit=None))]
    fn add_field(
        &self,
        name: &str,
        values: numpy::PyReadonlyArray1<f64>,
        unit: Option<String>,
    ) -> PyResult<()> {
        let vals = values.as_slice()?.to_vec();
        self.emit.borrow_mut()[self.index]
            .add_field(name.to_string(), vals, unit)
            .map_err(pyo3::exceptions::PyValueError::new_err)
    }
}
```

> Python call form: `delog.output(times_us, "Mag")`. (pyo3 positional order matches the Rust signature; keep `times_us` first.)

Update the worker's `Delog::new(...)` calls and the Task 5 test helper to pass an `EmitBuffer` (`Rc::default()`).

- [ ] **Step 5: Run tests, clippy, commit**

Run: `cargo test -p delog-script --features python` → PASS.
Run: `cargo clippy -p delog-script --features python -- -D warnings` → clean.

```bash
cargo fmt --all
git add crates/delog-script/src/api.rs crates/delog-script/src/engine.rs
git commit -m "script: delog.output(times, name) derived-topic builder + emit buffer (SCR-03)"
```

---

## Task 8: Run-script lifecycle — emit derived source + replace-on-rerun (`SCR-04`)

**Files:**
- Modify: `crates/delog-script/src/engine.rs` (`ScriptCommand::RunScript`, emission, replace-on-rerun; carry an `IngestSender`)
- Create: `crates/delog-script/src/emit.rs` (turn `PendingTopic`s into `ParsedBatch`es + submit)
- Test: `crates/delog-script/src/engine.rs`

- [ ] **Step 1: Write the failing test**

In `engine.rs` tests:

```rust
#[test]
fn running_a_script_emits_a_derived_source() {
    let store = Arc::new(DataStore::new());
    let (sender, receiver) = delog_core::ingest::ingest_channel();
    let ingestor = delog_core::ingestor::Ingestor::new(delog_core::ingestor::NullObserver);
    let store2 = ingestor.store();
    let h = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(store2.clone(), sender);
    let script = r#"
import numpy as np
t = np.array([0, 100, 200], dtype=np.int64)
out = delog.output(t, "Mag")
out.add_field("v", np.array([1.0, 2.0, 3.0]), unit="m")
"#;
    engine.send(ScriptCommand::RunScript { name: "test".into(), source: script.into() });
    // wait for Done
    loop {
        match engine.recv_blocking() {
            ScriptEvent::Done => break,
            ScriptEvent::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
    drop(engine);

    // The derived source "script:test" with topic "Mag" should be present.
    let snap = store2.load();
    let found = snap.sources.iter().any(|s| s.entry.label == "script:test" && !s.entry.removed);
    assert!(found, "derived source not published");
    let _ = h; // ingest thread stays alive while sender exists; test ends here.
    let _ = store; // silence unused
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p delog-script --features python running_a_script_emits_a_derived_source`
Expected: FAIL — `RunScript` variant / `spawn` arity mismatch.

- [ ] **Step 3: Implement the emit module**

Create `crates/delog-script/src/emit.rs`:

```rust
//! Turn a script's pending derived topics into batches on the ingest path (SCR-04).

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
use delog_core::schema::{FieldSchema, TopicSchema};

use crate::api::PendingTopic;

/// Open a `script:<name>` derived source, submit one batch per topic, close it.
/// Returns the new SourceId. Skips topics with no fields.
pub fn emit_topics(
    sink: &mut dyn IngestSink,
    script_name: &str,
    topics: &[PendingTopic],
) -> Result<SourceId, String> {
    let key = format!("script:{script_name}");
    let source = sink.open_source(&key, SourceKind::Derived);
    for topic in topics {
        if topic.fields.is_empty() {
            continue;
        }
        let fields: Result<Vec<FieldSchema>, String> = topic
            .fields
            .iter()
            .map(|f| {
                FieldSchema::new(
                    f.name.clone(),
                    arrow::datatypes::DataType::Float64,
                    f.unit.clone(),
                    1.0,
                )
                .map_err(|e| e.to_string())
            })
            .collect();
        let schema = Arc::new(
            TopicSchema::new(topic.name.clone(), fields?).map_err(|e| e.to_string())?,
        );
        let timestamps = Int64Array::from(topic.times.clone());
        let columns: Vec<ArrayRef> = topic
            .fields
            .iter()
            .map(|f| Arc::new(Float64Array::from(f.values.clone())) as ArrayRef)
            .collect();
        sink.submit(ParsedBatch::new(source, schema, timestamps, columns));
    }
    sink.close_source(source, ParseSummary::default());
    Ok(source)
}
```

Add `#[cfg(feature = "python")] pub mod emit;` to `lib.rs` (it depends only on core + arrow, but keep it under the feature since only the engine uses it; if you prefer, it can be feature-independent — keep it gated for now to avoid an unused-arrow warning in the no-python build).

- [ ] **Step 4: Extend the engine**

In `engine.rs`:
- Add to `ScriptCommand`:
```rust
    /// Run a full script buffer; on success emits one `script:<name>` source.
    RunScript { name: String, source: String },
```
- Change `spawn(store)` → `spawn(store: Arc<DataStore>, sender: IngestSender)`; thread `sender` into `worker_loop`.
- Track the previously-emitted SourceId per script name in the worker (a `HashMap<String, SourceId>`).
- Handle `RunScript`:

```rust
ScriptCommand::RunScript { name, source } => {
    let snapshot = store.load();
    let emit: crate::api::EmitBuffer = std::rc::Rc::default();
    let run_result = Python::with_gil(|py| {
        let g = globals.bind(py);
        let delog = Bound::new(py, Delog::new(snapshot.clone(), std::rc::Rc::clone(&emit))).unwrap();
        g.set_item("delog", delog).unwrap();
        py.run(&std::ffi::CString::new(source.as_str()).unwrap(), Some(g), None)
    });
    match run_result {
        Ok(()) => {
            let topics = emit.borrow();
            let mut sink = sender.file_sink();
            match crate::emit::emit_topics(&mut sink, &name, &topics) {
                Ok(_new_id) => {
                    // Replace-on-rerun: remove the prior source for this name.
                    if let Some(prev) = prev_sources.insert(name.clone(), _new_id) {
                        sender.remove_source(prev);
                    }
                }
                Err(e) => { let _ = evt_tx.send(ScriptEvent::Error(e)); }
            }
        }
        Err(err) => {
            // No partial source on failure.
            let _ = evt_tx.send(ScriptEvent::Error(format!("{err}")));
        }
    }
    let _ = evt_tx.send(ScriptEvent::Done);
}
```

Declare `let mut prev_sources: std::collections::HashMap<String, SourceId> = HashMap::new();` at the top of `worker_loop`, and import `IngestSender`, `SourceId`, `HashMap`.

> Ordering note: open the new source, then remove the previous one — so the browser never flickers to empty between runs.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p delog-script --features python running_a_script_emits_a_derived_source`
Expected: PASS.

- [ ] **Step 6: Commit**

Update PLAN.md §22:
```
- [~] **SCR-04** — Run-script lifecycle: emit one derived source per run + replace-on-rerun.
```

```bash
cargo fmt --all
cargo clippy -p delog-script --features python -- -D warnings
git add crates/delog-script/src/emit.rs crates/delog-script/src/engine.rs crates/delog-script/src/lib.rs PLAN.md
git commit -m "script: run-script emits a derived source with replace-on-rerun (SCR-04)"
```

---

## Task 9: stdout/stderr capture streaming (`SCR-05a`)

**Files:**
- Modify: `crates/delog-script/src/engine.rs` (an `OutputCapture` pyclass; redirect `sys.stdout`/`sys.stderr`)
- Test: `crates/delog-script/src/engine.rs`

- [ ] **Step 1: Write the failing test**

```rust
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
```

Add a `fn dummy_sender() -> IngestSender { delog_core::ingest::ingest_channel().0 }` helper to the tests module (the receiver drops immediately, making the sink inert — fine; this test emits no source).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p delog-script --features python print_is_captured_as_output_events`
Expected: FAIL (output goes to the process stdout, not events).

- [ ] **Step 3: Implement the capture pyclass and install it once**

In `engine.rs`:

```rust
/// A Python file-like object that forwards `write` to the event channel.
#[pyclass(unsendable)]
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
```

In `worker_loop`, once before the command loop, install the capture on `sys`:

```rust
Python::with_gil(|py| {
    let sys = py.import("sys").unwrap();
    let cap = Bound::new(py, OutputCapture { tx: evt_tx.clone() }).unwrap();
    sys.setattr("stdout", &cap).unwrap();
    sys.setattr("stderr", &cap).unwrap();
});
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p delog-script --features python print_is_captured_as_output_events`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p delog-script --features python -- -D warnings
git add crates/delog-script/src/engine.rs
git commit -m "script: capture stdout/stderr into output events (SCR-05)"
```

---

## Task 10: Error path — traceback event, no partial source (`SCR-05b`)

**Files:**
- Modify: `crates/delog-script/src/engine.rs` (richer traceback formatting)
- Test: `crates/delog-script/src/engine.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn failing_script_emits_error_and_no_source() {
    let store = Arc::new(DataStore::new());
    let (sender, receiver) = delog_core::ingest::ingest_channel();
    let ingestor = delog_core::ingestor::Ingestor::new(delog_core::ingestor::NullObserver);
    let store2 = ingestor.store();
    let h = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(store2.clone(), sender);
    // Emits a field THEN raises — proves we don't publish partial output.
    let script = "import numpy as np\nout = delog.output(np.array([0],dtype=np.int64),'X')\nout.add_field('v', np.array([1.0]))\nraise ValueError('boom')\n";
    engine.send(ScriptCommand::RunScript { name: "bad".into(), source: script.into() });
    let mut err = None;
    loop {
        match engine.recv_blocking() {
            ScriptEvent::Error(e) => err = Some(e),
            ScriptEvent::Done => break,
            _ => {}
        }
    }
    drop(engine);
    assert!(err.unwrap().contains("boom"));
    let snap = store2.load();
    assert!(!snap.sources.iter().any(|s| s.entry.label == "script:bad" && !s.entry.removed));
    let _ = h;
}
```

- [ ] **Step 2: Run to verify it fails or passes**

Run: `cargo test -p delog-script --features python failing_script_emits_error_and_no_source`
Expected: it should already largely PASS given Task 8's structure (emission only happens on `Ok`). If the error text lacks the message, proceed to Step 3 to format the traceback.

- [ ] **Step 3: Format the traceback**

In the `Err(err)` arm of `RunScript` (and in `eval_line`), format with the traceback:

```rust
let msg = Python::with_gil(|py| {
    let tb = err
        .traceback(py)
        .and_then(|t| t.format().ok())
        .unwrap_or_default();
    format!("{tb}{err}")
});
let _ = evt_tx.send(ScriptEvent::Error(msg));
```

(Adapt `err.traceback(py)` to your pyo3 version; the goal is the value error message plus any traceback string.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p delog-script --features python failing_script_emits_error_and_no_source`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p delog-script --features python -- -D warnings
git add crates/delog-script/src/engine.rs
git commit -m "script: stream tracebacks and never publish a partial source on error (SCR-05)"
```

---

## Task 11: Cooperative cancel (`SCR-05c`)

**Files:**
- Modify: `crates/delog-script/src/engine.rs` (a `request_interrupt` on the handle)
- Test: `crates/delog-script/src/engine.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn interrupt_stops_a_long_loop_with_keyboardinterrupt() {
    let engine = ScriptEngine::spawn(Arc::new(DataStore::new()), dummy_sender());
    engine.send(ScriptCommand::Eval("while True:\n    pass".into()));
    // Give the interpreter a moment to enter the loop, then interrupt.
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p delog-script --features python interrupt_stops_a_long_loop_with_keyboardinterrupt`
Expected: FAIL — `request_interrupt` not found (and the loop would hang without it; the test only works once implemented).

- [ ] **Step 3: Implement the interrupt**

`PyErr_SetInterrupt` sets the interpreter's pending-SIGINT flag and is safe to call from another thread; the running script raises `KeyboardInterrupt` at its next bytecode check.

Add to `impl ScriptEngine`:
```rust
    /// Ask the running script to stop (raises KeyboardInterrupt at the next
    /// bytecode boundary). Pure C calls (e.g. a large numpy op) cannot be
    /// interrupted mid-call — documented limitation (SCR-05).
    pub fn request_interrupt(&self) {
        // Safety: PyErr_SetInterrupt only sets a flag and is documented as
        // callable without holding the GIL.
        unsafe { pyo3::ffi::PyErr_SetInterrupt() };
    }
```

The `while True: pass` eval will then raise; the existing `eval_line` error path sends `ScriptEvent::Error` containing `KeyboardInterrupt`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p delog-script --features python interrupt_stops_a_long_loop_with_keyboardinterrupt`
Expected: PASS.

> If your pyo3 version requires `Py_Initialize` signal handlers for `PyErr_SetInterrupt` to be observed, ensure the interpreter is started normally (the `auto-initialize` feature does this). If the test is flaky in CI, gate it behind `#[ignore]` with a comment and keep the production code path.

- [ ] **Step 5: Commit**

Update PLAN.md §22:
```
- [~] **SCR-05** — REPL eval, stdout/stderr capture, traceback streaming, cooperative cancel.
```

```bash
cargo fmt --all
cargo clippy -p delog-script --features python -- -D warnings
git add crates/delog-script/src/engine.rs PLAN.md
git commit -m "script: cooperative cancel via PyErr_SetInterrupt (SCR-05)"
```

---

## Task 12: Global script library persistence (`SCR-06`)

This module is **feature-independent** (pure std::fs) so it is testable without Python.

**Files:**
- Modify: `crates/delog-script/src/library.rs`
- Test: `crates/delog-script/src/library.rs`

- [ ] **Step 1: Write the failing test**

In `library.rs`:

```rust
//! Persistent global script library: `.py` files in a directory (SCR-06).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A scripts library rooted at a directory of `.py` files.
pub struct ScriptLibrary {
    dir: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_list_load_delete_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("delog-scripts-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let lib = ScriptLibrary::new(&tmp);

        lib.save("accel_mag", "print('hi')").unwrap();
        assert_eq!(lib.list().unwrap(), vec!["accel_mag".to_string()]);
        assert_eq!(lib.load("accel_mag").unwrap(), "print('hi')");
        lib.delete("accel_mag").unwrap();
        assert!(lib.list().unwrap().is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_names_with_path_separators() {
        let tmp = std::env::temp_dir().join("delog-scripts-bad");
        let lib = ScriptLibrary::new(&tmp);
        assert!(lib.save("../evil", "x").is_err());
        assert!(lib.load("a/b").is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p delog-script save_list_load_delete_roundtrip`
Expected: FAIL — methods not found.

- [ ] **Step 3: Implement**

```rust
impl ScriptLibrary {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn validate(name: &str) -> io::Result<()> {
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid script name '{name}'"),
            ));
        }
        Ok(())
    }

    fn path(&self, name: &str) -> io::Result<PathBuf> {
        Self::validate(name)?;
        Ok(self.dir.join(format!("{name}.py")))
    }

    /// Script names (file stems), sorted.
    pub fn list(&self) -> io::Result<Vec<String>> {
        let mut out = Vec::new();
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in rd {
            let p = entry?.path();
            if p.extension().and_then(|e| e.to_str()) == Some("py")
                && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
            {
                out.push(stem.to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn load(&self, name: &str) -> io::Result<String> {
        fs::read_to_string(self.path(name)?)
    }

    pub fn save(&self, name: &str, source: &str) -> io::Result<()> {
        let path = self.path(name)?;
        fs::create_dir_all(&self.dir)?;
        fs::write(path, source)
    }

    pub fn delete(&self, name: &str) -> io::Result<()> {
        fs::remove_file(self.path(name)?)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p delog-script library` (runs without `--features python`)
Expected: PASS.

- [ ] **Step 5: Commit**

Update PLAN.md §22:
```
- [~] **SCR-06** — Global script library persistence (config-dir `.py` files): list/load/save/delete.
```

```bash
cargo fmt --all
cargo clippy -p delog-script -- -D warnings
git add crates/delog-script/src/library.rs PLAN.md
git commit -m "script: global .py library persistence (list/load/save/delete) (SCR-06)"
```

---

## Task 13: `delog-app` `scripting` feature + Tools menu + Session accessors (`SCR-07a`)

**Files:**
- Modify: `crates/delog-app/Cargo.toml` (feature + optional deps)
- Modify: `crates/delog-app/src/session.rs` (add `store()` and `ingest_sender()` accessors)
- Modify: `crates/delog-app/src/app.rs` (Tools menu; gated `open_scripts` toggle field)
- Modify: `crates/delog-app/src/main.rs` or wherever modules are declared (declare `#[cfg(feature="scripting")] mod scripts;`)

- [ ] **Step 1: Add the feature and optional deps**

In `crates/delog-app/Cargo.toml`:

```toml
[features]
default = []
# Embedded-Python scripting (PLAN.md §17.3, SCR-*). OFF by default.
scripting = ["dep:delog-script", "delog-script/python", "dep:egui_code_editor"]

[dependencies]
# ... existing ...
delog-script = { workspace = true, optional = true }
egui_code_editor = { workspace = true, optional = true }
```

- [ ] **Step 2: Add Session accessors**

In `crates/delog-app/src/session.rs`, on `impl Session` (near `snapshot`):

```rust
    /// The published store, for background engines that load fresh snapshots.
    pub fn store(&self) -> Arc<DataStore> {
        Arc::clone(&self.store)
    }

    /// A clone of the ingest sender, for engines that emit derived sources.
    pub fn ingest_sender(&self) -> IngestSender {
        self.sender.clone()
    }
```

Confirm `DataStore` and `IngestSender` are imported in `session.rs` (they are — `store`/`sender` fields use them).

- [ ] **Step 3: Verify the no-scripting build still compiles**

Run: `cargo build -p delog-app`
Expected: PASS (accessors compile; no scripting code yet).

- [ ] **Step 4: Add a `scripts` UI module stub and Tools menu**

Create `crates/delog-app/src/scripts.rs`:

```rust
//! Scripts window: REPL + editor + library (SCR-07). Feature-gated.

/// All UI + engine state for the scripts window.
pub struct ScriptsPanel {
    pub open: bool,
}

impl Default for ScriptsPanel {
    fn default() -> Self {
        Self { open: false }
    }
}
```

Declare it where the other modules are declared (search `mod browser;` in `app.rs` or `main.rs` and add beside it):
```rust
#[cfg(feature = "scripting")]
mod scripts;
```

Add a gated field to `DelogApp` (the struct, near `session: Session`):
```rust
    #[cfg(feature = "scripting")]
    scripts: scripts::ScriptsPanel,
```
…and initialize it in `DelogApp`'s constructor with `#[cfg(feature = "scripting")] scripts: scripts::ScriptsPanel::default(),`.

In the menu bar (`app.rs`, the `egui::MenuBar` block ~line 952–1012), add a Tools menu before Help:
```rust
ui.menu_button("Tools", |ui| {
    #[cfg(feature = "scripting")]
    if ui.button("Scripts…").clicked() {
        self.scripts.open = true;
        ui.close();
    }
    #[cfg(not(feature = "scripting"))]
    {
        ui.add_enabled(false, egui::Button::new("Scripts (build with --features scripting)"));
    }
});
```

> `ui.close()` matches the existing menu style in this file (the recon showed `ui.close()` usage). If the codebase uses `ui.close_menu()`, match that instead.

- [ ] **Step 5: Build both configurations**

Run: `cargo build -p delog-app`
Expected: PASS (Tools menu shows the disabled hint).
Run: `cargo build -p delog-app --features scripting`
Expected: PASS if Python is installed (links libpython). Otherwise note the expected linker requirement.

- [ ] **Step 6: Commit**

Update PLAN.md §22:
```
- [~] **SCR-07** — Scripts window + Tools menu in delog-app (feature-gated).
```

```bash
cargo fmt --all
cargo clippy -p delog-app -- -D warnings
git add crates/delog-app/Cargo.toml crates/delog-app/src/session.rs crates/delog-app/src/app.rs crates/delog-app/src/scripts.rs Cargo.lock PLAN.md
git commit -m "app: scripting feature, Tools menu, Session accessors, scripts module stub (SCR-07)"
```

---

## Task 14: Scripts window UI — engine wiring, editor, REPL (`SCR-07b`)

This is UI code; it is verified by build + a manual run (per memory note: GUI changes need a manual run on the user's machine). Keep logic minimal and delegate to the tested engine/library.

**Files:**
- Modify: `crates/delog-app/src/scripts.rs`
- Modify: `crates/delog-app/src/app.rs` (call `self.scripts.ui(...)` once per frame; construct the engine lazily)

- [ ] **Step 1: Flesh out `ScriptsPanel` with engine + library + buffers**

Replace `crates/delog-app/src/scripts.rs` contents:

```rust
//! Scripts window: REPL + editor + library (SCR-07). Feature-gated.

use std::sync::Arc;

use delog_core::snapshot::DataStore;
use delog_core::ingest::IngestSender;
use delog_script::library::ScriptLibrary;
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};
use egui_code_editor::{CodeEditor, ColorTheme, Syntax};

pub struct ScriptsPanel {
    pub open: bool,
    engine: Option<ScriptEngine>,
    library: ScriptLibrary,
    names: Vec<String>,
    current_name: String,
    editor_text: String,
    repl_input: String,
    console: String,
    status: String,
}

impl ScriptsPanel {
    pub fn new(scripts_dir: std::path::PathBuf) -> Self {
        let library = ScriptLibrary::new(scripts_dir);
        let names = library.list().unwrap_or_default();
        Self {
            open: false,
            engine: None,
            library,
            names,
            current_name: String::new(),
            editor_text: String::new(),
            repl_input: String::new(),
            console: String::new(),
            status: String::new(),
        }
    }

    /// Lazily start the interpreter on first open.
    fn engine(&mut self, store: Arc<DataStore>, sender: IngestSender) -> &ScriptEngine {
        self.engine
            .get_or_insert_with(|| ScriptEngine::spawn(store, sender))
    }

    fn drain(&mut self) {
        if let Some(engine) = &self.engine {
            for ev in engine.drain_events() {
                match ev {
                    ScriptEvent::Output(s) => self.console.push_str(&s),
                    ScriptEvent::Result(r) => {
                        self.console.push_str(&r);
                        self.console.push('\n');
                    }
                    ScriptEvent::Error(e) => {
                        self.console.push_str(&e);
                        self.console.push('\n');
                        self.status = "error".into();
                    }
                    ScriptEvent::Done => self.status = "done".into(),
                }
            }
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, store: Arc<DataStore>, sender: IngestSender) {
        if !self.open {
            return;
        }
        self.drain();
        let mut open = self.open;
        egui::Window::new("Scripts")
            .open(&mut open)
            .default_size([720.0, 480.0])
            .show(ctx, |ui| {
                egui::SidePanel::left("scripts_library").show_inside(ui, |ui| {
                    ui.heading("Library");
                    if ui.button("New").clicked() {
                        self.current_name.clear();
                        self.editor_text.clear();
                    }
                    let names = self.names.clone();
                    for name in names {
                        if ui.selectable_label(self.current_name == name, &name).clicked()
                            && let Ok(src) = self.library.load(&name)
                        {
                            self.current_name = name.clone();
                            self.editor_text = src;
                        }
                    }
                });
                egui::TopBottomPanel::bottom("scripts_repl")
                    .resizable(true)
                    .min_height(140.0)
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("REPL:");
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.repl_input)
                                    .desired_width(f32::INFINITY),
                            );
                            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                let line = std::mem::take(&mut self.repl_input);
                                self.console.push_str(">>> ");
                                self.console.push_str(&line);
                                self.console.push('\n');
                                self.engine(store.clone(), sender.clone())
                                    .send(ScriptCommand::Eval(line));
                            }
                        });
                        egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.console.as_str())
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Monospace),
                            );
                        });
                    });
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.current_name);
                        if ui.button("Save").clicked() && !self.current_name.is_empty() {
                            match self.library.save(&self.current_name, &self.editor_text) {
                                Ok(()) => {
                                    self.names = self.library.list().unwrap_or_default();
                                    self.status = format!("saved {}", self.current_name);
                                }
                                Err(e) => self.status = format!("save failed: {e}"),
                            }
                        }
                        if ui.button("Run ▶").clicked() && !self.current_name.is_empty() {
                            self.console.push_str(&format!("# run {}\n", self.current_name));
                            self.engine(store.clone(), sender.clone()).send(
                                ScriptCommand::RunScript {
                                    name: self.current_name.clone(),
                                    source: self.editor_text.clone(),
                                },
                            );
                        }
                        if ui.button("Cancel ⏹").clicked()
                            && let Some(e) = &self.engine
                        {
                            e.request_interrupt();
                        }
                        ui.label(&self.status);
                    });
                    CodeEditor::default()
                        .id_source("script_editor")
                        .with_syntax(Syntax::python())
                        .with_theme(ColorTheme::GITHUB_DARK)
                        .with_numlines(true)
                        .show(ui, &mut self.editor_text);
                });
            });
        self.open = open;
        ctx.request_repaint(); // keep draining engine events while open
    }
}
```

> `egui_code_editor` API names (`CodeEditor`, `Syntax::python()`, `ColorTheme::GITHUB_DARK`, `.with_numlines`, `.show`) are for the 0.2 line. If the pinned version differs, adjust to its README — the build error names the exact symbol.

- [ ] **Step 2: Construct the panel with the scripts dir and call its `ui` each frame**

You need a scripts directory next to `settings.json`. In `crates/delog-app/src/layout.rs`, expose a public helper (the recon found `storage_dir(APP_ID)` private there):

```rust
/// The app config directory (where settings.json lives), if resolvable.
pub fn config_dir() -> Option<std::path::PathBuf> {
    storage_dir(APP_ID)
}
```

In `app.rs`, change the `ScriptsPanel` field init (Task 13) to:
```rust
    #[cfg(feature = "scripting")]
    scripts: scripts::ScriptsPanel::new(
        crate::layout::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("scripts"),
    ),
```

Then, once per frame in the app's `update` (after the menu bar, near where other windows like `vehicle_dialog` are drawn), add:
```rust
    #[cfg(feature = "scripting")]
    self.scripts.ui(ctx, self.session.store(), self.session.ingest_sender());
```

- [ ] **Step 3: Build both configurations**

Run: `cargo build -p delog-app`
Expected: PASS (no scripting).
Run: `cargo build -p delog-app --features scripting`
Expected: PASS (requires Python). Fix any `egui_code_editor` symbol mismatches surfaced here.

- [ ] **Step 4: Manual run (per memory: GUI needs a manual run)**

Run: `cargo run -p delog-app --features scripting`
Then in the app: open a fixture log, **Tools ▸ Scripts…**, type in the REPL `delog.sources()` and press Enter — confirm field paths print in the console. Paste a script that emits a field, click **Run ▶**, and confirm a `script:<name>` source appears in the data browser. Re-run and confirm it replaces (does not duplicate).

Report the result. If anything misbehaves, capture the console text / a screenshot before fixing.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p delog-app --features scripting -- -D warnings
git add crates/delog-app/src/scripts.rs crates/delog-app/src/app.rs crates/delog-app/src/layout.rs
git commit -m "app: scripts window — editor, REPL, run/cancel/save wired to engine (SCR-07)"
```

---

## Task 15: Golden fixture test, build matrix, docs & checklist (`SCR-08`)

**Files:**
- Create: `crates/delog-script/tests/golden_accel_mag.rs`
- Modify: `CLAUDE.md` (Commands section — build matrix)
- Modify: `PLAN.md` (§1.3 non-goals; §22 SCR section finalize; BLG-07)

- [ ] **Step 1: Write the golden end-to-end test**

Create `crates/delog-script/tests/golden_accel_mag.rs`. The engine **reads** from
the `DataStore` passed to `spawn` and **writes** through the `IngestSender`. So the
test uses two stores: a read store holding the IMU input (built directly and
published), and a write store owned by a real ingestor thread that the engine
emits into and we assert on.

```rust
#![cfg(feature = "python")]

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::chunk::Chunk;
use delog_core::identity::IdentityRegistry;
use delog_core::ingest::ingest_channel;
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_core::store::TopicStore;
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};

/// Read store: IMU.AccX/Y/Z = (3,4,0) at t=0 -> magnitude 5.
fn read_store() -> Arc<DataStore> {
    let mut id = IdentityRegistry::new();
    let src = id.add_source("flight");
    let topic = id.add_topic(src, "IMU").unwrap();
    for f in ["AccX", "AccY", "AccZ"] {
        id.add_field(topic, f).unwrap();
    }
    let schema = Arc::new(
        TopicSchema::new(
            "IMU",
            ["AccX", "AccY", "AccZ"]
                .iter()
                .map(|n| FieldSchema::new(*n, DataType::Float64, Some("m/s^2"), 1.0).unwrap()),
        )
        .unwrap(),
    );
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Float64Array::from(vec![3.0])),
        Arc::new(Float64Array::from(vec![4.0])),
        Arc::new(Float64Array::from(vec![0.0])),
    ];
    let chunk = Arc::new(Chunk::try_new(Int64Array::from(vec![0]), cols, &schema).unwrap());
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    let snap = StoreSnapshot::from_registry(&id, [(topic, store)], 0).unwrap();
    Arc::new(DataStore::from_snapshot(snap))
}

#[test]
fn accel_magnitude_script_emits_expected_values() {
    // Write side: a real ingestor thread we emit the derived source into.
    let ingestor = Ingestor::new(NullObserver);
    let write_store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(read_store(), sender);
    let script = r#"
import numpy as np
x = delog.field('flight/IMU/AccX').v
y = delog.field('flight/IMU/AccY').v
z = delog.field('flight/IMU/AccZ').v
t = delog.field('flight/IMU/AccX').t
out = delog.output(t, "AccMag")
out.add_field("mag", np.sqrt(x*x + y*y + z*z), unit="m/s^2")
"#;
    engine.send(ScriptCommand::RunScript {
        name: "accel_mag".into(),
        source: script.into(),
    });
    loop {
        match engine.recv_blocking() {
            ScriptEvent::Done => break,
            ScriptEvent::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
    drop(engine); // shuts the worker; ingest thread stays up via `sender` clone in the worker

    // Emission flows through the channel; the ingest thread seals it
    // asynchronously, so poll briefly for the derived topic to appear.
    let out = {
        let mut snap = write_store.load();
        for _ in 0..100 {
            if snap.topics.iter().any(|t| t.entry.name == "AccMag") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            snap = write_store.load();
        }
        snap
    };
    let derived_topic = out
        .topics
        .iter()
        .find(|t| t.entry.name == "AccMag")
        .expect("AccMag topic emitted");
    let ts = out.topic_store(derived_topic.entry.id).unwrap();
    assert_eq!(ts.chunks[0].cols.len(), 1);
    let mag = ts.chunks[0].cols[0]
        .as_any()
        .downcast_ref::<Float64Array>()
        .unwrap()
        .value(0);
    assert!((mag - 5.0).abs() < 1e-9, "magnitude was {mag}");

    let _ = ingest_thread; // dropping the engine drops the worker's sender clone -> thread ends
}
```

> The worker holds the only live `IngestSender` clone; once `engine` is dropped, the
> ingest thread sees all senders gone and exits, so the test does not hang.

- [ ] **Step 2: Run the golden test**

Run: `cargo test -p delog-script --features python accel_magnitude_script_emits_expected_values`
Expected: PASS (magnitude == 5.0).

- [ ] **Step 3: Update CLAUDE.md build matrix**

In `CLAUDE.md`, under `## Commands`, add after the existing `cargo run` line:

```bash
# Optional Python scripting (PLAN.md §17.3, SCR-*) — OFF by default:
cargo build --workspace --features delog-app/scripting   # full scripting build (needs Python)
cargo test  -p delog-script --features python            # scripting engine tests
```

- [ ] **Step 4: Finalize PLAN.md**

- In §1.3 non-goals, change `scripting (backlog)` to note it is now in-progress under SCR-* (e.g. `scripting (in progress — SCR-*, optional feature)`).
- Mark `BLG-07` as superseded: `- [x] **BLG-07** — superseded by the SCR section (optional embedded-Python scripting).`
- Flip the SCR-01..08 entries you set to `[~]` to `[x]` for the ones now complete, and add the final two lines if missing:
```
- [x] **SCR-03** — `delog` API: sources()/field()→numpy, resample_prev, output() builder.
- [x] **SCR-08** — Tests: engine, golden accel-mag script, numpy↔Arrow round-trip, error path, resample_prev proptest, substrate tests.
```
Add a note line: `delog-app gains a \`scripting\` feature (OFF by default); see CLAUDE.md build matrix.`

- [ ] **Step 5: Full-workspace verification both ways**

Run: `cargo test --workspace` → PASS (no Python needed; library + core substrate tests run).
Run: `cargo clippy --workspace -- -D warnings` → clean.
Run: `cargo test -p delog-script --features python` → PASS (engine + golden, needs Python).
Run: `cargo clippy -p delog-script --features python -- -D warnings` → clean.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/delog-script/tests/golden_accel_mag.rs CLAUDE.md PLAN.md
git commit -m "script: golden accel-magnitude test; docs + checklist finalize (SCR-08)"
```

---

## Self-review notes (for the executor)

- **Spec coverage:** Tasks map 1:1 to spec sections — substrate (§4 → T1,T2), crate/feature (§3,§9 → T3,T13), execution model (§5 → T4,T8), `delog` API (§6 → T5,T6,T7), persistence (§7 → T12), UI (§8 → T13,T14), errors/cancel/safety (§10 → T9,T10,T11), testing (§11 → tests throughout + T15).
- **Python-dependent tasks** (T4–T11, T14, T15) require a local Python toolchain to build/test; if absent on the dev machine, build the no-feature workspace (T1–T3, T12, T13 no-feature paths) and run the `--features python` steps in CI / on the user's RTX-4080 machine.
- **pyo3/numpy/egui_code_editor exact APIs** may drift from the 0.24/0.2 idioms shown; each such step ends in a build/test command that is the source of truth — adapt symbols to the pinned versions when the compiler points at them.
- **Manual GUI verification** (T14 Step 4) is required because plots/windows can't be asserted headless here (per the project memory note).
