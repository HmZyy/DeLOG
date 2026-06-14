# Live Python Transforms Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a same-topic live Python transform API so scripts can continuously append derived fields while live telemetry arrives.

**Architecture:** Keep snapshot scripting unchanged and add a separate live-transform path. The ingest thread mirrors live batches to the script engine through a bounded non-blocking queue; the script worker filters registered transforms, runs Python callables on copied numpy arrays, and submits derived batches to one appendable `script:<name>` live-derived source.

**Tech Stack:** Rust 2024, pyo3/numpy, arrow-rs, existing `delog-core` ingest channel, existing `delog-script` worker, egui app session wiring.

---

## File Structure

- Modify `crates/delog-core/src/ingest.rs`: add `SourceKind::LiveDerived`.
- Modify `crates/delog-core/src/ingestor.rs`: make `LiveDerived` seal like `Live`, and notify observers of accepted batches.
- Modify `crates/delog-app/src/session.rs`: expose a script-live batch mirror sink and route live batches to it without blocking ingest.
- Modify `crates/delog-app/src/scripts.rs`: register the script engine's live-batch sink with `Session`.
- Create `crates/delog-script/src/live.rs`: live-transform specs, batch materialization, output validation, and ParsedBatch emission helpers.
- Modify `crates/delog-script/src/api.rs`: add `delog.live_transform(...)` decorator registration.
- Modify `crates/delog-script/src/engine.rs`: add the bounded live-batch queue, active transform generations, and live-batch execution loop.
- Modify `crates/delog-script/src/lib.rs`: export the new live module/types.
- Add integration tests under `crates/delog-script/tests/` and focused unit tests in the changed modules.
- Modify `docs/scripting.md`: document snapshot mode vs live transform mode.
- Update `PLAN.md` §22 by adding or marking a `SCR-*` checklist item for live Python transforms.

---

### Task 1: Core Ingest Support For Appendable Derived Live Sources

**Files:**
- Modify: `crates/delog-core/src/ingest.rs`
- Modify: `crates/delog-core/src/ingestor.rs`
- Test: `crates/delog-core/src/ingestor.rs`

- [x] **Step 1: Write the failing test**

Add this test to the existing `#[cfg(test)] mod tests` in `crates/delog-core/src/ingestor.rs`:

```rust
#[test]
fn live_derived_source_seals_on_age_like_live() {
    let mut ing = Ingestor::new(NullObserver);
    let source = open_with(&mut ing, "script:live_math", SourceKind::LiveDerived);

    ing.process(IngestMsg::Batch(batch(source, "NAV_RAD", &[1])));
    ing.flush_aged_live();
    assert_eq!(ing.chunks_sealed(), 0, "not stale yet");

    std::thread::sleep(LIVE_MAX_AGE + Duration::from_millis(10));
    ing.flush_aged_live();

    assert_eq!(ing.chunks_sealed(), 1, "live-derived pending rows seal by age");
    let snap = ing.store().load();
    let topic = snap
        .topics
        .iter()
        .find(|t| t.entry.name == "NAV_RAD")
        .expect("derived live topic registered");
    assert_eq!(snap.topic_store(topic.entry.id).unwrap().chunks.len(), 1);
}
```

- [x] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p delog-core live_derived_source_seals_on_age_like_live
```

Expected: compile failure because `SourceKind::LiveDerived` does not exist.

- [x] **Step 3: Add `LiveDerived` to `SourceKind`**

In `crates/delog-core/src/ingest.rs`, change `SourceKind` to:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A log file parsed start-to-finish; may block on backpressure (§5).
    File,
    /// A live link; must never block — full channel drops the batch (ING-03).
    Live,
    /// A completed programmatically-generated source (snapshot scripts).
    Derived,
    /// A programmatically-generated live source that appends over time.
    LiveDerived,
}
```

- [x] **Step 4: Seal `LiveDerived` like `Live`**

In `crates/delog-core/src/ingestor.rs`, update `open_source`:

```rust
let seal_rows = match kind {
    SourceKind::File | SourceKind::Derived => FILE_CHUNK_ROWS,
    SourceKind::Live | SourceKind::LiveDerived => LIVE_CHUNK_ROWS,
};
```

Update `flush_aged_live` filter:

```rust
.filter(|(_, state)| matches!(state.kind, SourceKind::Live | SourceKind::LiveDerived))
```

- [x] **Step 5: Add an observer hook for accepted batches**

Extend `IngestObserver` in `crates/delog-core/src/ingestor.rs`:

```rust
pub trait IngestObserver: Send {
    fn on_diagnostic(&mut self, _diag: Diag) {}
    fn on_progress(&mut self, _source: SourceId, _frac: f32) {}
    fn on_close(&mut self, _source: SourceId, _summary: ParseSummary) {}
    fn on_batch(&mut self, _kind: SourceKind, _batch: &ParsedBatch) {}
}
```

In `accept_batch`, capture `kind` before moving the batch data:

```rust
let Some(source) = self.sources.get(&batch.source) else {
    self.observer.on_diagnostic(Diag::warning(
        "batch-unknown-source",
        format!("batch for unopened source {:?} dropped", batch.source),
    ));
    return;
};
let seal_rows = source.seal_rows;
let source_kind = source.kind;
let source_id = batch.source;
self.observer.on_batch(source_kind, &batch);
```

- [x] **Step 6: Run the test and verify it passes**

Run:

```bash
cargo test -p delog-core live_derived_source_seals_on_age_like_live
```

Expected: PASS.

- [x] **Step 7: Commit**

```bash
git add crates/delog-core/src/ingest.rs crates/delog-core/src/ingestor.rs
git commit -m "core: add live-derived ingest source kind"
```

---

### Task 2: Live Transform Types And Batch Materialization

**Files:**
- Create: `crates/delog-script/src/live.rs`
- Modify: `crates/delog-script/src/lib.rs`
- Test: `crates/delog-script/src/live.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/delog-script/src/live.rs` with the module header and tests first:

```rust
//! Live Python transform support: same-topic batch transforms for scripts.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float32Array, Int16Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::identity::SourceId;
    use delog_core::ingest::ParsedBatch;
    use delog_core::schema::{FieldSchema, TopicSchema};

    use super::*;

    fn nav_batch() -> ParsedBatch {
        let schema = Arc::new(
            TopicSchema::new(
                "NAV_CONTROLLER_OUTPUT",
                [
                    FieldSchema::new("nav_roll", DataType::Float32, Some("deg"), 1.0).unwrap(),
                    FieldSchema::new("nav_pitch", DataType::Float32, Some("deg"), 1.0).unwrap(),
                    FieldSchema::new("nav_bearing", DataType::Int16, Some("deg"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let columns: Vec<ArrayRef> = vec![
            Arc::new(Float32Array::from(vec![0.0, 90.0])),
            Arc::new(Float32Array::from(vec![45.0, -45.0])),
            Arc::new(Int16Array::from(vec![180, -90])),
        ];
        ParsedBatch::new(
            SourceId(7),
            schema,
            Int64Array::from(vec![100, 200]),
            columns,
        )
    }

    #[test]
    fn spec_matches_topic_and_required_fields() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAV_CONTROLLER_OUTPUT".into(),
            vec!["nav_roll".into(), "nav_bearing".into()],
            "NAV_RAD".into(),
        )
        .unwrap();

        assert!(spec.matches(&nav_batch()));
    }

    #[test]
    fn spec_rejects_missing_required_fields() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAV_CONTROLLER_OUTPUT".into(),
            vec!["missing".into()],
            "NAV_RAD".into(),
        )
        .unwrap();

        assert!(!spec.matches(&nav_batch()));
    }

    #[test]
    fn materialize_batch_widens_numeric_fields_to_f64() {
        let spec = LiveTransformSpec::new(
            "script".into(),
            1,
            "NAV_CONTROLLER_OUTPUT".into(),
            vec!["nav_roll".into(), "nav_bearing".into()],
            "NAV_RAD".into(),
        )
        .unwrap();

        let materialized = LiveTransformBatch::from_parsed(&spec, &nav_batch()).unwrap();

        assert_eq!(materialized.times, vec![100, 200]);
        assert_eq!(materialized.values["nav_roll"], vec![0.0, 90.0]);
        assert_eq!(materialized.values["nav_bearing"], vec![180.0, -90.0]);
    }
}
```

- [ ] **Step 2: Run the tests and verify they fail**

Run:

```bash
cargo test -p delog-script --features python live::
```

Expected: compile failures for missing `LiveTransformSpec` and `LiveTransformBatch`.

- [ ] **Step 3: Implement live transform types**

Add above the tests in `crates/delog-script/src/live.rs`:

```rust
use std::collections::HashMap;

use delog_core::field_view::array_row_as_f64;
use delog_core::ingest::ParsedBatch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTransformSpec {
    pub script_name: String,
    pub generation: u64,
    pub topic: String,
    pub fields: Vec<String>,
    pub output_topic: String,
}

impl LiveTransformSpec {
    pub fn new(
        script_name: String,
        generation: u64,
        topic: String,
        fields: Vec<String>,
        output_topic: String,
    ) -> Result<Self, String> {
        if topic.is_empty() {
            return Err("live_transform topic must not be empty".into());
        }
        if fields.is_empty() {
            return Err("live_transform fields must not be empty".into());
        }
        if output_topic.is_empty() {
            return Err("live_transform output_topic must not be empty".into());
        }
        Ok(Self {
            script_name,
            generation,
            topic,
            fields,
            output_topic,
        })
    }

    pub fn matches(&self, batch: &ParsedBatch) -> bool {
        batch.topic() == self.topic
            && self
                .fields
                .iter()
                .all(|field| batch.schema.field_index(field).is_some())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveTransformBatch {
    pub times: Vec<i64>,
    pub values: HashMap<String, Vec<f64>>,
}

impl LiveTransformBatch {
    // ZC-EXCEPTION: script live materialization copies the incoming live batch
    // into contiguous f64 buffers for CPython/numpy; off render hot path.
    pub fn from_parsed(spec: &LiveTransformSpec, batch: &ParsedBatch) -> Result<Self, String> {
        if !spec.matches(batch) {
            return Err(format!(
                "batch topic '{}' does not satisfy live transform '{}'",
                batch.topic(),
                spec.output_topic
            ));
        }
        let times: Vec<i64> = (0..batch.timestamps.len())
            .map(|row| batch.timestamps.value(row))
            .collect();
        let mut values = HashMap::new();
        for field in &spec.fields {
            let idx = batch
                .schema
                .field_index(field)
                .ok_or_else(|| format!("field '{field}' missing from {}", batch.topic()))?;
            let col = batch.columns[idx].as_ref();
            let vals = (0..batch.timestamps.len())
                .map(|row| array_row_as_f64(col, row))
                .collect();
            values.insert(field.clone(), vals);
        }
        Ok(Self { times, values })
    }
}
```

- [ ] **Step 4: Export the module**

In `crates/delog-script/src/lib.rs`, add:

```rust
#[cfg(feature = "python")]
pub mod live;
```

- [ ] **Step 5: Run the tests and verify they pass**

Run:

```bash
cargo test -p delog-script --features python live::
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/delog-script/src/live.rs crates/delog-script/src/lib.rs
git commit -m "script: add live transform batch types"
```

---

### Task 3: Python Decorator Registration

**Files:**
- Modify: `crates/delog-script/src/api.rs`
- Modify: `crates/delog-script/src/engine.rs`
- Test: `crates/delog-script/src/engine.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/delog-script/src/engine.rs` tests:

```rust
#[test]
fn live_transform_decorator_registers_a_transform() {
    let ingestor = Ingestor::new(NullObserver);
    let store = ingestor.store();
    let (sender, _receiver) = ingest_channel();
    let engine = ScriptEngine::spawn(store, sender);

    let script = r#"
@delog.live_transform(
    topic="NAV_CONTROLLER_OUTPUT",
    fields=["nav_roll", "nav_pitch"],
    output_topic="NAV_RAD",
)
def convert(batch):
    return {}
"#;
    engine.send(ScriptCommand::RunScript {
        name: "nav_rad".into(),
        source: script.into(),
    });

    assert_eq!(engine.recv_blocking(), ScriptEvent::Done);
    assert_eq!(engine.transform_specs().len(), 1);
    assert_eq!(engine.transform_specs()[0].topic, "NAV_CONTROLLER_OUTPUT");
    assert_eq!(engine.transform_specs()[0].output_topic, "NAV_RAD");
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p delog-script --features python live_transform_decorator_registers_a_transform
```

Expected: compile failure because `live_transform` and `transform_specs` do not exist.

- [ ] **Step 3: Add registration buffers to the API**

In `crates/delog-script/src/api.rs`, add:

```rust
use pyo3::types::{PyDict, PyTuple};
use crate::live::LiveTransformSpec;

pub struct PendingLiveTransform {
    pub spec: LiveTransformSpec,
    pub callable: Py<PyAny>,
}

pub type LiveTransformBuffer = Rc<RefCell<Vec<PendingLiveTransform>>>;
```

Update `Delog`:

```rust
pub struct Delog {
    snapshot: Arc<StoreSnapshot>,
    emit: EmitBuffer,
    live: LiveTransformBuffer,
    script_name: String,
    generation: u64,
}
```

Update `Delog::new` signature and callers:

```rust
pub fn new(
    snapshot: Arc<StoreSnapshot>,
    emit: EmitBuffer,
    live: LiveTransformBuffer,
    script_name: String,
    generation: u64,
) -> Self {
    Self {
        snapshot,
        emit,
        live,
        script_name,
        generation,
    }
}
```

- [ ] **Step 4: Implement `delog.live_transform(...)`**

Add this method to `#[pymethods] impl Delog`:

```rust
#[pyo3(signature = (*, topic, fields, output_topic))]
fn live_transform(
    &self,
    py: Python<'_>,
    topic: String,
    fields: Vec<String>,
    output_topic: String,
) -> PyResult<Py<PyAny>> {
    let spec = LiveTransformSpec::new(
        self.script_name.clone(),
        self.generation,
        topic,
        fields,
        output_topic,
    )
    .map_err(pyo3::exceptions::PyValueError::new_err)?;

    #[pyclass(unsendable)]
    struct Decorator {
        spec: LiveTransformSpec,
        live: LiveTransformBuffer,
    }

    #[pymethods]
    impl Decorator {
        fn __call__(&self, py: Python<'_>, func: Py<PyAny>) -> PyResult<Py<PyAny>> {
            self.live.borrow_mut().push(PendingLiveTransform {
                spec: self.spec.clone(),
                callable: func.clone_ref(py),
            });
            Ok(func)
        }
    }

    Ok(Bound::new(
        py,
        Decorator {
            spec,
            live: Rc::clone(&self.live),
        },
    )?
    .into_any()
    .unbind())
}
```

- [ ] **Step 5: Store active transform metadata in the engine**

In `crates/delog-script/src/engine.rs`, add an `Arc<Mutex<Vec<LiveTransformSpec>>>` to `ScriptEngine` for test-visible specs. In `spawn`, clone it into `worker_loop`. When a script run succeeds, replace specs for that script name with the newly registered specs.

Use this public test helper:

```rust
#[cfg(test)]
pub fn transform_specs(&self) -> Vec<crate::live::LiveTransformSpec> {
    self.transform_specs.lock().unwrap().clone()
}
```

- [ ] **Step 6: Run the test and verify it passes**

Run:

```bash
cargo test -p delog-script --features python live_transform_decorator_registers_a_transform
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/delog-script/src/api.rs crates/delog-script/src/engine.rs
git commit -m "script: register live transform decorators"
```

---

### Task 4: Execute Live Transform Callables And Validate Output

**Files:**
- Modify: `crates/delog-script/src/live.rs`
- Modify: `crates/delog-script/src/engine.rs`
- Test: `crates/delog-script/tests/golden_live_nav_transform.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/delog-script/tests/golden_live_nav_transform.rs`:

```rust
#![cfg(feature = "python")]

use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Array, Int16Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::ingest::{IngestSink, ingest_channel};
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_script::{ScriptCommand, ScriptEngine, ScriptEvent};

fn read_store() -> Arc<DataStore> {
    Arc::new(DataStore::from_snapshot(StoreSnapshot::empty()))
}

fn nav_batch(source: delog_core::identity::SourceId) -> delog_core::ingest::ParsedBatch {
    let schema = Arc::new(
        TopicSchema::new(
            "NAV_CONTROLLER_OUTPUT",
            [
                FieldSchema::new("nav_roll", DataType::Float32, Some("deg"), 1.0).unwrap(),
                FieldSchema::new("nav_pitch", DataType::Float32, Some("deg"), 1.0).unwrap(),
                FieldSchema::new("nav_bearing", DataType::Int16, Some("deg"), 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Float32Array::from(vec![0.0, 90.0])),
        Arc::new(Float32Array::from(vec![45.0, -45.0])),
        Arc::new(Int16Array::from(vec![180, -90])),
    ];
    delog_core::ingest::ParsedBatch::new(source, schema, Int64Array::from(vec![1, 2]), columns)
}

#[test]
fn live_transform_appends_derived_batches() {
    let ingestor = Ingestor::new(NullObserver);
    let write_store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));

    let engine = ScriptEngine::spawn(read_store(), sender.clone());
    let script = r#"
DEG_TO_RAD = 0.017453292519943295

@delog.live_transform(
    topic="NAV_CONTROLLER_OUTPUT",
    fields=["nav_roll", "nav_pitch", "nav_bearing"],
    output_topic="NAV_CONTROLLER_OUTPUT_RAD",
)
def convert(batch):
    return {
        "nav_roll_rad": batch.nav_roll * DEG_TO_RAD,
        "nav_pitch_rad": (batch.nav_pitch * DEG_TO_RAD, "rad"),
        "nav_bearing_rad": (batch.t, batch.nav_bearing * DEG_TO_RAD, "rad"),
    }
"#;
    engine.send(ScriptCommand::RunScript {
        name: "nav_rad".into(),
        source: script.into(),
    });

    wait_done(&engine);

    let raw_source = {
        let mut sink = sender.file_sink();
        sink.open_source("live", delog_core::ingest::SourceKind::Live)
    };
    engine.try_send_live_batch(nav_batch(raw_source)).unwrap();
    wait_done(&engine);

    let snap = wait_for_topic(&write_store, "NAV_CONTROLLER_OUTPUT_RAD");
    let topic = snap
        .topics
        .iter()
        .find(|t| t.entry.name == "NAV_CONTROLLER_OUTPUT_RAD")
        .unwrap();
    let store = snap.topic_store(topic.entry.id).unwrap();
    let idx = store.schema.field_index("nav_bearing_rad").unwrap();
    let values = store.chunks[0].cols[idx]
        .as_any()
        .downcast_ref::<arrow::array::Float64Array>()
        .unwrap();
    assert!((values.value(0) - std::f64::consts::PI).abs() < 1e-12);
    assert!((values.value(1) + std::f64::consts::FRAC_PI_2).abs() < 1e-12);

    drop(engine);
    drop(sender);
    let _ = ingest_thread.join();
}

fn wait_done(engine: &ScriptEngine) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for event in engine.drain_events() {
            match event {
                ScriptEvent::Done => return,
                ScriptEvent::Error(err) => panic!("script error: {err}"),
                _ => {}
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for ScriptEvent::Done"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn wait_for_topic(store: &DataStore, topic: &str) -> StoreSnapshot {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let snap = store.load();
        if snap.topics.iter().any(|t| t.entry.name == topic) {
            return (*snap).clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for topic {topic}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p delog-script --features python live_transform_appends_derived_batches
```

Expected: compile failures for `try_send_live_batch` and live execution.

- [ ] **Step 3: Add Python batch object and output parser**

In `crates/delog-script/src/live.rs`, add a `#[pyclass] LiveBatchPy` with `.t` and dynamic field attributes:

```rust
#[pyclass(unsendable, name = "LiveBatch")]
pub struct LiveBatchPy {
    #[pyo3(get)]
    pub t: Py<PyArray1<i64>>,
    fields: HashMap<String, Py<PyArray1<f64>>>,
}

#[pymethods]
impl LiveBatchPy {
    fn __getattr__(&self, name: &str) -> PyResult<Py<PyArray1<f64>>> {
        self.fields
            .get(name)
            .map(|arr| Python::attach(|py| arr.clone_ref(py)))
            .ok_or_else(|| pyo3::exceptions::PyAttributeError::new_err(name.to_owned()))
    }
}
```

Add `LiveTransformResult`:

```rust
pub struct LiveTransformResult {
    pub topic: String,
    pub times: Vec<i64>,
    pub fields: Vec<crate::api::PendingField>,
}
```

Add `parse_transform_result(py, spec, input_times, obj) -> PyResult<LiveTransformResult>` that accepts `values`, `(values, unit)`, and `(times, values, unit)`. Validate every output has `len == input_times.len()` and explicit times equal `input_times`.

- [ ] **Step 4: Add emission helper for one live result**

In `crates/delog-script/src/live.rs`, add:

```rust
pub fn result_to_batch(
    source: SourceId,
    result: LiveTransformResult,
) -> Result<ParsedBatch, String> {
    let fields = result
        .fields
        .iter()
        .map(|f| FieldSchema::new(f.name.clone(), DataType::Float64, f.unit.clone(), 1.0))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let schema = Arc::new(TopicSchema::new(result.topic, fields).map_err(|e| e.to_string())?);
    let timestamps = Int64Array::from(result.times);
    let columns: Vec<ArrayRef> = result
        .fields
        .into_iter()
        .map(|f| Arc::new(Float64Array::from(f.values)) as ArrayRef)
        .collect();
    Ok(ParsedBatch::new(source, schema, timestamps, columns))
}
```

- [ ] **Step 5: Execute transforms in the worker**

In `engine.rs`, store active transforms:

```rust
struct ActiveTransform {
    spec: crate::live::LiveTransformSpec,
    callable: Py<PyAny>,
    source: SourceId,
    consecutive_errors: u8,
    disabled: bool,
}
```

When live batches arrive, for each active transform whose spec matches:

1. Materialize with `LiveTransformBatch::from_parsed`.
2. Build `LiveBatchPy` under the GIL.
3. Call the Python callable.
4. Parse the return value.
5. Convert to `ParsedBatch`.
6. Submit through a sink.

Use this error threshold:

```rust
const LIVE_TRANSFORM_ERROR_LIMIT: u8 = 3;
```

After three consecutive errors, set `disabled = true` and emit one `ScriptEvent::Error`.

- [ ] **Step 6: Add live batch sender API**

In `ScriptEngine`, add a bounded channel and method:

```rust
pub const LIVE_TRANSFORM_QUEUE_CAP: usize = 128;

pub fn live_batch_sender(&self) -> std::sync::mpsc::SyncSender<ParsedBatch> {
    self.live_tx.clone()
}

pub fn try_send_live_batch(&self, batch: ParsedBatch) -> Result<(), ParsedBatch> {
    match self.live_tx.try_send(batch) {
        Ok(()) => Ok(()),
        Err(std::sync::mpsc::TrySendError::Full(batch)) => Err(batch),
        Err(std::sync::mpsc::TrySendError::Disconnected(batch)) => Err(batch),
    }
}
```

Make the worker loop poll command and live queues so a `Done` event is emitted after each processed live batch in tests.

- [ ] **Step 7: Run the integration test and verify it passes**

Run:

```bash
cargo test -p delog-script --features python live_transform_appends_derived_batches
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/delog-script/src/live.rs crates/delog-script/src/engine.rs crates/delog-script/tests/golden_live_nav_transform.rs
git commit -m "script: execute same-topic live transforms"
```

---

### Task 5: App Session Routing From Live Ingest To Script Engine

**Files:**
- Modify: `crates/delog-app/src/session.rs`
- Modify: `crates/delog-app/src/scripts.rs`
- Test: `crates/delog-app/src/session.rs`

- [ ] **Step 1: Write the failing unit test**

Add a test-only helper observer test in `crates/delog-app/src/session.rs` tests module:

```rust
#[test]
fn app_observer_mirrors_only_live_batches_without_blocking() {
    use std::sync::mpsc::sync_channel;
    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::ingest::{ParsedBatch, SourceKind};
    use delog_core::schema::{FieldSchema, TopicSchema};

    let (tx, rx) = sync_channel(1);
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

    observer.on_batch(SourceKind::File, &batch);
    assert!(rx.try_recv().is_err(), "file batches are not mirrored");

    observer.on_batch(SourceKind::Live, &batch);
    assert!(rx.try_recv().is_ok(), "live batches are mirrored");
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p delog-app --features scripting app_observer_mirrors_only_live_batches_without_blocking
```

Expected: compile failure because `AppObserver` has no `live_scripts` field.

- [ ] **Step 3: Add live script sink to session observer**

In `session.rs`, add:

```rust
#[cfg(feature = "scripting")]
type LiveScriptSink = Arc<Mutex<Option<std::sync::mpsc::SyncSender<delog_core::ingest::ParsedBatch>>>>;
```

Add to `AppObserver`:

```rust
#[cfg(feature = "scripting")]
live_scripts: LiveScriptSink,
```

Add to `Session`:

```rust
#[cfg(feature = "scripting")]
live_scripts: LiveScriptSink,
```

Initialize it in `Session::new` and pass the same `Arc` to `AppObserver`.

- [ ] **Step 4: Implement non-blocking mirroring**

In `impl IngestObserver for AppObserver`, add:

```rust
#[cfg(feature = "scripting")]
fn on_batch(&mut self, kind: SourceKind, batch: &delog_core::ingest::ParsedBatch) {
    if kind != SourceKind::Live {
        return;
    }
    let Some(tx) = self.live_scripts.lock().unwrap().clone() else {
        return;
    };
    match tx.try_send(batch.clone()) {
        Ok(()) => {}
        Err(std::sync::mpsc::TrySendError::Full(_)) => {
            push_diag(
                &self.diagnostics,
                Diag::warning("script-live-drop", "live transform queue full; dropped batch"),
            );
        }
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
            *self.live_scripts.lock().unwrap() = None;
        }
    }
}
```

Add `Session` methods:

```rust
#[cfg(feature = "scripting")]
pub fn set_live_script_sink(
    &self,
    sink: Option<std::sync::mpsc::SyncSender<delog_core::ingest::ParsedBatch>>,
) {
    *self.live_scripts.lock().unwrap() = sink;
}
```

- [ ] **Step 5: Register script engine live sink from UI**

In `crates/delog-app/src/scripts.rs`, add:

```rust
pub fn live_batch_sender(
    &mut self,
    store: Arc<DataStore>,
    sender: IngestSender,
) -> std::sync::mpsc::SyncSender<delog_core::ingest::ParsedBatch> {
    self.engine(store, sender).live_batch_sender()
}
```

In `crates/delog-app/src/app.rs`, before drawing scripts UI, call:

```rust
#[cfg(feature = "scripting")]
{
    let live_sink = self
        .scripts
        .live_batch_sender(self.session.store(), self.session.ingest_sender());
    self.session.set_live_script_sink(Some(live_sink));
}
```

- [ ] **Step 6: Run tests and verify they pass**

Run:

```bash
cargo test -p delog-app --features scripting app_observer_mirrors_only_live_batches_without_blocking
cargo test -p delog-script --features python live_transform_appends_derived_batches
```

Expected: both PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/delog-app/src/session.rs crates/delog-app/src/scripts.rs crates/delog-app/src/app.rs
git commit -m "app: route live batches to Python transforms"
```

---

### Task 6: Rerun Replacement And Shutdown Semantics

**Files:**
- Modify: `crates/delog-script/src/engine.rs`
- Test: `crates/delog-script/src/engine.rs`

- [ ] **Step 1: Write the failing test**

Add to `engine.rs` tests:

```rust
#[test]
fn rerunning_live_transform_removes_previous_generation() {
    let ingestor = Ingestor::new(NullObserver);
    let write_store = ingestor.store();
    let (sender, receiver) = ingest_channel();
    let ingest_thread = std::thread::spawn(move || ingestor.run(receiver));
    let engine = ScriptEngine::spawn(Arc::new(DataStore::new()), sender);

    let script = r#"
@delog.live_transform(topic="A", fields=["v"], output_topic="B")
def f(batch):
    return {"x": batch.v}
"#;
    engine.send(ScriptCommand::RunScript {
        name: "live".into(),
        source: script.into(),
    });
    assert_eq!(engine.recv_blocking(), ScriptEvent::Done);
    engine.send(ScriptCommand::RunScript {
        name: "live".into(),
        source: script.into(),
    });
    assert_eq!(engine.recv_blocking(), ScriptEvent::Done);

    let snap = write_store.load();
    let active = snap
        .sources
        .iter()
        .filter(|s| s.entry.label == "script:live" && !s.entry.removed)
        .count();
    assert_eq!(active, 1, "only latest live script source remains active");

    drop(engine);
    let _ = ingest_thread.join();
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p delog-script --features python rerunning_live_transform_removes_previous_generation
```

Expected: FAIL because previous live-derived source is not removed.

- [ ] **Step 3: Implement generation replacement**

In the worker's successful `RunScript` path:

1. Build `next_generation += 1`.
2. Remove active transforms for `name`.
3. If a previous live source exists in `prev_sources`, call `sender.remove_source(prev)`.
4. If the run registered live transforms, open a new source with:

```rust
let mut sink = sender.file_sink();
let source = sink.open_source(&format!("script:{name}"), SourceKind::LiveDerived);
```

5. Attach that `source` to each `ActiveTransform`.
6. Do not call `close_source` for live transform sources.

- [ ] **Step 4: Run the test and verify it passes**

Run:

```bash
cargo test -p delog-script --features python rerunning_live_transform_removes_previous_generation
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/delog-script/src/engine.rs
git commit -m "script: replace live transform generations"
```

---

### Task 7: Documentation And Example Script

**Files:**
- Modify: `docs/scripting.md`
- Create: `scripts/nav_controller_live_rad.py`
- Modify: `PLAN.md`

- [ ] **Step 1: Document the live transform mode**

In `docs/scripting.md`, add a section after "How scripts produce data":

````markdown
## Live transforms

Snapshot scripts run once against the current store. Live transforms register
callbacks that run for future live batches.

```python
DEG_TO_RAD = 0.017453292519943295

@delog.live_transform(
    topic="NAV_CONTROLLER_OUTPUT",
    fields=["nav_roll", "nav_pitch", "nav_bearing"],
    output_topic="NAV_CONTROLLER_OUTPUT_RAD",
)
def nav_controller_rad(batch):
    return {
        "nav_roll_rad": (batch.nav_roll * DEG_TO_RAD, "rad"),
        "nav_pitch_rad": (batch.nav_pitch * DEG_TO_RAD, "rad"),
        "nav_bearing_rad": (batch.t, batch.nav_bearing * DEG_TO_RAD, "rad"),
    }
```

Version 1 live transforms are same-topic only. The callback sees one incoming
topic batch at a time. It cannot join across topics, resample, or keep rolling
window state.
````

- [ ] **Step 2: Add the example script**

Create `scripts/nav_controller_live_rad.py`:

```python
DEG_TO_RAD = 0.017453292519943295


@delog.live_transform(
    topic="NAV_CONTROLLER_OUTPUT",
    fields=["nav_roll", "nav_pitch", "nav_bearing"],
    output_topic="NAV_CONTROLLER_OUTPUT_RAD",
)
def nav_controller_rad(batch):
    return {
        "nav_roll_rad": (batch.nav_roll * DEG_TO_RAD, "rad"),
        "nav_pitch_rad": (batch.nav_pitch * DEG_TO_RAD, "rad"),
        "nav_bearing_rad": (batch.nav_bearing * DEG_TO_RAD, "rad"),
    }
```

- [ ] **Step 3: Update `PLAN.md` checklist**

Add a scripting checklist item in §22 if no live-transform item exists:

```markdown
- [x] **SCR-08** — Live Python same-topic transform callbacks with appendable derived live sources.
```

If `SCR-08` already exists, mark that existing item `[x]` instead of creating a duplicate.

- [ ] **Step 4: Commit**

```bash
git add docs/scripting.md scripts/nav_controller_live_rad.py PLAN.md
git commit -m "docs: document live Python transforms"
```

---

### Task 8: Full Verification

**Files:**
- All files touched by previous tasks.

- [ ] **Step 1: Run formatting**

Run:

```bash
cargo fmt --all
```

Expected: exits 0.

- [ ] **Step 2: Run focused tests**

Run:

```bash
cargo test -p delog-core live_derived_source_seals_on_age_like_live
cargo test -p delog-script --features python live_transform
cargo test -p delog-script --features python live_transform_appends_derived_batches
cargo test -p delog-app --features scripting app_observer_mirrors_only_live_batches_without_blocking
```

Expected: all PASS.

- [ ] **Step 3: Run clippy**

Run:

```bash
cargo clippy -p delog-core -- -D warnings
cargo clippy -p delog-script --features python -- -D warnings
cargo clippy -p delog-app --features scripting -- -D warnings
```

Expected: all clean.

- [ ] **Step 4: Build the app**

Run:

```bash
cargo build -p delog-app
```

Expected: exits 0.

- [ ] **Step 5: Manual verification**

Run:

```bash
cargo run -p delog-app
```

Manual steps:

1. Start a live MAVLink stream that emits `NAV_CONTROLLER_OUTPUT`.
2. Save or run `nav_controller_live_rad`.
3. Plot `script:nav_controller_live_rad/NAV_CONTROLLER_OUTPUT_RAD/nav_roll_rad`.
4. Confirm the plot continues appending while raw live `NAV_CONTROLLER_OUTPUT/nav_roll` appends.

- [ ] **Step 6: Commit verification fixes**

After any verification fixes made during Task 8, commit the touched files. For example, if the fixes touched the engine and docs:

```bash
git add crates/delog-script/src/engine.rs docs/scripting.md
git commit -m "fix: stabilize live Python transform verification"
```

---

## Self-Review

Spec coverage:

- Same-topic live transforms: Tasks 2, 3, 4.
- Appendable derived live source: Tasks 1, 4, 6.
- Non-blocking raw ingest/backpressure: Task 5.
- Rerun replacement: Task 6.
- Error handling and validation: Task 4.
- Documentation and example: Task 7.
- Verification: Task 8.

Placeholder scan: no deferred-work markers or unspecified error-handling steps remain. Each task gives concrete file paths, commands, and expected results.

Type consistency: `LiveTransformSpec`, `LiveTransformBatch`, `LiveTransformResult`, `SourceKind::LiveDerived`, `ScriptEngine::live_batch_sender`, and `ScriptEngine::try_send_live_batch` are introduced before later tasks reference them.
