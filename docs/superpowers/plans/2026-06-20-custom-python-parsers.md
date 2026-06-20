# Custom Python File Parsers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add globally saved Python parsers under Tools ▸ Parsers that run unchanged legacy `Parse(numpy.float32[])` files and ingest their tuple output as normal DeLOG sources.

**Architecture:** Keep Python execution in the existing single `delog-script` worker. Add focused persistence and compatibility-conversion modules, validate and convert the entire Python result before opening a source, then route completed Arrow arrays through the existing blocking ingest sink. `delog-app` adds a dedicated parser editor panel while `ScriptsPanel` retains ownership of the one shared engine and routes typed parser events.

**Tech Stack:** Rust 2024, pyo3 0.29, numpy 0.29, arrow-rs 59, egui/eframe 0.34, egui_code_editor, rfd, Criterion.

---

## File Structure

- Modify `crates/delog-core/src/schema.rs`: optional field descriptions with a backward-compatible builder.
- Modify `crates/delog-app/src/browser.rs`: carry and display field descriptions on hover.
- Create `crates/delog-script/src/parser_library.rs`: safe `parsers/*.py` persistence and rename semantics.
- Create `crates/delog-script/src/custom_parser.rs`: float32 file loading, Python tuple validation, topic/clock grouping, Arrow conversion, and transactional emission.
- Modify `crates/delog-script/src/engine.rs`: parser commands/events, shared metrics, syntax validation, and orchestration.
- Modify `crates/delog-script/src/lib.rs`: expose the two new modules and event/result types.
- Create `crates/delog-script/tests/golden_custom_parser.rs`: unchanged legacy-contract end-to-end test.
- Create `crates/delog-script/benches/custom_parser.rs`: staged large-output benchmark.
- Modify `crates/delog-script/Cargo.toml`: Criterion bench declaration.
- Create `crates/delog-app/src/parsers.rs`: menu-facing parser list, file picker, editor, save/rename/delete, progress, and error popup state.
- Modify `crates/delog-app/src/scripts.rs`: own `ParsersPanel`, share the one engine, and route parser events.
- Modify `crates/delog-app/src/app.rs`: Tools ▸ Parsers menu and combined loading indicator/cancel behavior.
- Modify `crates/delog-app/src/main.rs`: register the parser UI module.
- Create `crates/delog-app/assets/icons/folder-open.svg` and modify `crates/delog-app/src/icons.rs`: open-with-parser icon.
- Modify `crates/delog-app/tests/popup_policy.rs`: parser popup policy coverage.
- Modify `docs/scripting.md`: user-facing custom parser documentation.
- Modify `PLAN.md`: mark SCR-11 complete only after all verification passes.

### Task 1: Field Description Metadata

**Files:**
- Modify: `crates/delog-core/src/schema.rs`
- Modify: `crates/delog-app/src/browser.rs`

- [ ] **Step 1: Write failing schema and browser-model tests**

Add to `schema.rs`:

```rust
#[test]
fn field_description_is_optional_and_builder_preserves_other_metadata() {
    let plain = FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap();
    assert_eq!(plain.description, None);
    let described = plain.with_description("filtered altitude");
    assert_eq!(described.description.as_deref(), Some("filtered altitude"));
    assert_eq!(described.unit.as_deref(), Some("m"));
}
```

Extend the existing browser model fixture with `.with_description("latitude")` and assert its `FieldNode.description` is preserved.

- [ ] **Step 2: Run the focused tests and verify failure**

Run: `cargo test -p delog-core schema::tests::field_description_is_optional_and_builder_preserves_other_metadata && cargo test -p delog-app browser::tests`

Expected: compilation fails because `description` and `with_description` do not exist.

- [ ] **Step 3: Add optional descriptions without changing existing constructor call sites**

Add to `FieldSchema`:

```rust
pub description: Option<String>,
```

Initialize it to `None` in `FieldSchema::new`, then add:

```rust
pub fn with_description(mut self, description: impl Into<String>) -> Self {
    let description = description.into();
    self.description = (!description.is_empty()).then_some(description);
    self
}
```

Add `description: Option<String>` to `browser::FieldNode`, populate it from schema, and attach non-empty descriptions to the field-row response:

```rust
let response = if let Some(description) = &field.description {
    response.on_hover_text(description)
} else {
    response
};
```

- [ ] **Step 4: Run focused tests**

Run: `cargo test -p delog-core schema::tests && cargo test -p delog-app browser::tests`

Expected: all schema and browser tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/delog-core/src/schema.rs crates/delog-app/src/browser.rs
git commit -m "Add field description metadata"
```

### Task 2: Parser Library Persistence

**Files:**
- Create: `crates/delog-script/src/parser_library.rs`
- Modify: `crates/delog-script/src/lib.rs`

- [ ] **Step 1: Write parser-library tests first**

Create the module with tests for normalization, sorted listing, round-trip, rename, and unsafe names:

```rust
#[test]
fn save_list_load_rename_delete_roundtrip() {
    let dir = temp_dir("roundtrip");
    let lib = ParserLibrary::new(&dir);
    assert_eq!(lib.normalize_name("dt46").unwrap(), "dt46.py");
    lib.save(None, "dt46", "def Parse(x): return []").unwrap();
    lib.save(None, "alpha.py", "def Parse(x): return []").unwrap();
    assert_eq!(lib.list().unwrap(), vec!["alpha.py", "dt46.py"]);
    lib.save(Some("dt46.py"), "renamed.py", "def Parse(x): return []").unwrap();
    assert!(lib.load("dt46.py").is_err());
    assert!(lib.load("renamed.py").unwrap().contains("Parse"));
    lib.delete("renamed.py").unwrap();
}

#[test]
fn rejects_paths_and_non_python_extensions() {
    let lib = ParserLibrary::new(temp_dir("invalid"));
    for name in ["", "../x.py", "a/b.py", "a\\b.py", "x.txt"] {
        assert!(lib.normalize_name(name).is_err(), "accepted {name}");
    }
}
```

- [ ] **Step 2: Run the test and verify failure**

Run: `cargo test -p delog-script --features python parser_library`

Expected: compilation fails because `ParserLibrary` is not defined.

- [ ] **Step 3: Implement the library**

Implement:

```rust
pub struct ParserLibrary { dir: PathBuf }

impl ParserLibrary {
    pub fn new(dir: impl Into<PathBuf>) -> Self;
    pub fn normalize_name(&self, name: &str) -> io::Result<String>;
    pub fn list(&self) -> io::Result<Vec<String>>;
    pub fn load(&self, name: &str) -> io::Result<String>;
    pub fn save(&self, old_name: Option<&str>, name: &str, source: &str) -> io::Result<String>;
    pub fn delete(&self, name: &str) -> io::Result<()>;
    pub fn dir(&self) -> &Path;
}
```

`save` must create the directory, write the normalized destination, and remove `old_name` only after the destination write succeeds and only when the normalized names differ. Re-export the module from `lib.rs` behind `python`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p delog-script --features python parser_library`

Expected: both parser-library tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/delog-script/src/parser_library.rs crates/delog-script/src/lib.rs
git commit -m "Persist custom Python parsers"
```

### Task 3: Float32 Input And Python Output Conversion

**Files:**
- Create: `crates/delog-script/src/custom_parser.rs`
- Modify: `crates/delog-script/src/lib.rs`

- [ ] **Step 1: Write failing raw-file and return-contract tests**

Cover native endian loading, trailing-byte omission, topic splitting, clock selection, duplicate replacement, and validation. Core examples:

```rust
#[test]
fn reads_native_f32_and_ignores_trailing_bytes() {
    let path = std::env::temp_dir().join("delog-custom-parser-raw.dat");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1.5_f32.to_ne_bytes());
    bytes.extend_from_slice(&(-2.0_f32).to_ne_bytes());
    bytes.extend_from_slice(&[9, 8]);
    std::fs::write(&path, bytes).unwrap();
    let raw = read_float32_file(&path).unwrap();
    assert_eq!(raw.values, vec![1.5, -2.0]);
    assert_eq!(raw.ignored_trailing_bytes, 2);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn legacy_result_uses_rtc_and_last_duplicate_wins() {
    Python::attach(|py| {
        let np = py.import("numpy").unwrap();
        let locals = PyDict::new(py);
        locals.set_item("np", np).unwrap();
        let expression = std::ffi::CString::new(
            "[(\"AP.rtc\", np.array([1., 2.]), \"clock\"), (\"AP.x\", np.array([3., 4.]), \"old\"), (\"AP.x\", np.array([5., 6.]), \"new\")]"
        ).unwrap();
        let result = py.eval(&expression, None, Some(&locals)).unwrap();
        let parsed = parse_python_result(py, &result).unwrap();
        assert_eq!(parsed.topics[0].timestamps.value(0), 1_000_000);
        assert_eq!(parsed.topics[0].timestamps.value(1), 2_000_000);
        assert_eq!(parsed.topics[0].fields.len(), 2); // rtc + last x
        assert_eq!(parsed.warnings.len(), 1);
    });
}
```

Also test `.rtc` over `.index`, index fallback, visible clock fields, Unicode/trailing-space names, bool/int/uint/f32/f64 arrays, NaN values, missing dots/clocks, non-1D arrays, non-finite clocks, decreasing clocks, and unequal lengths.

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test -p delog-script --features python custom_parser`

Expected: compilation fails because the custom parser module and types do not exist.

- [ ] **Step 3: Implement raw file loading and source-independent output types**

Define:

```rust
pub struct RawFloatFile {
    pub values: Vec<f32>,
    pub input_bytes: u64,
    pub ignored_trailing_bytes: u8,
}

pub struct ParserField {
    pub name: String,
    pub description: String,
    pub values: ArrayRef,
}

pub struct ParserTopic {
    pub name: String,
    pub timestamps: Int64Array,
    pub fields: Vec<ParserField>,
}

pub struct ParserOutput {
    pub topics: Vec<ParserTopic>,
    pub warnings: Vec<String>,
    pub arrow_bytes: u64,
}
```

Implement `read_float32_file(path)` with `fs::read`, `chunks_exact(4)`, and `f32::from_ne_bytes`. Add the required ZC exception comment directly above the conversion.

- [ ] **Step 4: Implement dtype-preserving one-dimensional NumPy conversion**

Implement `numpy.ascontiguousarray(value)` and dispatch on `dtype.kind` plus `dtype.itemsize` to these Arrow arrays: `BooleanArray`, signed/unsigned 8/16/32/64 arrays, `Float32Array`, and `Float64Array`. Reject other kinds and dimensions with the full field name. Do not coerce integer views to float.

- [ ] **Step 5: Implement grouping and clock validation**

Implement:

```rust
pub fn parse_python_result(py: Python<'_>, result: &Bound<'_, PyAny>) -> PyResult<ParserOutput>;
```

Require a list/sequence of exact three-item tuples `(str, 1-D array, str)`. Preserve first-seen topic and field order. Split at the first dot. Replace an exact duplicate in place and append one warning. Select `<topic>.rtc`, otherwise `<topic>.index`; convert finite seconds with checked `round(seconds * 1_000_000.0) -> i64`; require non-decreasing clocks; require all topic arrays to have the clock length. Build `FieldSchema` later, so this phase stays source-independent and transactional.

- [ ] **Step 6: Run focused tests**

Run: `cargo test -p delog-script --features python custom_parser`

Expected: all raw-input, dtype, grouping, duplicate, clock, and error tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/delog-script/src/custom_parser.rs crates/delog-script/src/lib.rs
git commit -m "Convert legacy Python parser output"
```

### Task 4: Transactional Emission And Worker Commands

**Files:**
- Modify: `crates/delog-script/src/custom_parser.rs`
- Modify: `crates/delog-script/src/engine.rs`
- Modify: `crates/delog-script/src/lib.rs`
- Create: `crates/delog-script/tests/golden_custom_parser.rs`

- [ ] **Step 1: Write failing transactional emission tests**

Add a unit test that builds a `ParserOutput`, emits it through a real ingestor, and asserts source/topic/schema/description/value fidelity. Add a failure test that runs a parser returning unequal lengths and asserts the store has no non-removed source.

Create `golden_custom_parser.rs` with an unchanged script:

```python
import numpy as np

def Parse(raw_data):
    n = len(raw_data) // 2
    raw_data = raw_data[:n * 2].reshape(-1, 2)
    index = np.linspace(0, 1, n)
    return [
        ("AP.index", index, "normalized index"),
        ("AP.rtc", raw_data[:, 0], "seconds"),
        ("AP.value", raw_data[:, 1].view("int32"), "raw integer view"),
        ("AP.value", raw_data[:, 1], "last duplicate wins"),
    ]
```

Write native-endian float values to a temporary file, dispatch the worker command, wait for completion, and assert canonical timestamps, last duplicate values, descriptions, warning diagnostic, and source label.

- [ ] **Step 2: Run the tests and verify failure**

Run: `cargo test -p delog-script --features python --test golden_custom_parser`

Expected: compilation fails because parser commands/events and emission do not exist.

- [ ] **Step 3: Add typed parser commands and events**

Extend `ScriptCommand`:

```rust
ValidateParser { name: String, source: String },
ParseFile { parser_name: String, source: String, path: PathBuf },
```

Add:

```rust
pub enum ParserEvent {
    SyntaxValid { name: String },
    Reading { parser_name: String, path: PathBuf },
    Running { parser_name: String, path: PathBuf },
    Succeeded { parser_name: String, path: PathBuf, topics: u64, rows: u64 },
    Failed { parser_name: String, path: Option<PathBuf>, message: String },
}
```

Wrap it as `ScriptEvent::Parser(ParserEvent)`. Parser events must not toggle the Scripts Console's `running` flag.

- [ ] **Step 4: Implement syntax-only validation**

Handle `ValidateParser` with Python `compile(source, name, "exec")`. Emit `SyntaxValid` on success or `Failed { path: None, ... }` with `format_pyerr` on failure. Never execute the compiled code.

- [ ] **Step 5: Implement parse orchestration and transactional emission**

Add `emit_parser_output(sink, source_label, output) -> Result<ParseSummary, String>` to `custom_parser.rs`. It must open `SourceKind::File` only after `ParserOutput` exists, create `FieldSchema` values using `.with_description`, submit one batch per topic, emit duplicate warnings as `python-parser-duplicate`, then close the source with topic/row/time counts.

Handle `ParseFile` in this order:

1. Emit `Reading` and read `RawFloatFile`.
2. Emit `Running`.
3. Create a fresh `PyDict`; run source in that namespace.
4. Get callable `Parse` and pass `raw.values.into_pyarray(py)`.
5. Call `parse_python_result` completely.
6. Only after success, call `emit_parser_output` with the selected file stem.
7. Emit `Succeeded`; otherwise emit one `Failed` and no source.

Use a fresh namespace so parser globals do not leak into scripts or subsequent parsers.

- [ ] **Step 6: Add shared metrics to the engine**

Change `ScriptEngine::spawn` to accept `Arc<MetricsRegistry>`. Record gauges/counters with these exact keys:

```text
python_parser_input_bytes
python_parser_output_bytes
python_parser_read
python_parser_python
python_parser_convert
python_parser_total
python_parser_cancelled
python_parser_failures
```

Use scoped timers for durations, `record` for bytes, and `add` for counters. Pass the session registry from every engine construction.
Keep the `PyErr` until it is classified: `PyKeyboardInterrupt` increments
`python_parser_cancelled`; every other read, compile, call, validation, or emit
error increments `python_parser_failures`. Format the traceback only after that
classification. Update engine unit tests to pass `Arc::new(MetricsRegistry::new())`
and assert both counters on their corresponding paths.

- [ ] **Step 7: Run unit and golden tests**

Run: `cargo test -p delog-script --features python custom_parser && cargo test -p delog-script --features python --test golden_custom_parser`

Expected: all tests pass, including no-source-on-failure.

- [ ] **Step 8: Commit**

```bash
git add crates/delog-script/src/custom_parser.rs crates/delog-script/src/engine.rs crates/delog-script/src/lib.rs crates/delog-script/tests/golden_custom_parser.rs
git commit -m "Run custom parsers through Python worker"
```

### Task 5: Parser Editor And File Picker UI

**Files:**
- Create: `crates/delog-app/src/parsers.rs`
- Modify: `crates/delog-app/src/scripts.rs`
- Modify: `crates/delog-app/src/main.rs`

- [ ] **Step 1: Write failing pure-state tests**

Test the default buffer, edit load, syntax-check/save handshake, rename, delete staging, parser event transitions, and request creation. Keep filesystem behavior in `ParserLibrary`; UI tests should assert state only.

```rust
#[test]
fn new_parser_uses_compatible_template() {
    let mut panel = panel();
    panel.add_new();
    assert_eq!(panel.filename(), "new_parser.py");
    assert!(panel.source().contains("def Parse(raw_data):"));
    assert!(!panel.can_delete());
}

#[test]
fn runtime_failure_opens_concise_error_and_stops_spinner() {
    let mut panel = panel();
    panel.handle_event(ParserEvent::Failed {
        parser_name: "bad.py".into(),
        path: Some("flight.dat".into()),
        message: "Traceback...".into(),
    });
    assert!(!panel.is_running());
    assert!(panel.error_message().unwrap().contains("bad.py"));
}
```

- [ ] **Step 2: Run the test and verify failure**

Run: `cargo test -p delog-app parsers::tests`

Expected: compilation fails because `ParsersPanel` does not exist.

- [ ] **Step 3: Implement `ParsersPanel` state and persistence actions**

The panel owns `ParserLibrary`, filename/source/editor status, saved original name, delete confirmation, pending syntax-checked save, parse phase, concise error popup, and an `mpsc` channel for `(parser_name, parser_source, selected_path)` requests. Expose:

```rust
pub struct ParseRequest {
    pub parser_name: String,
    pub source: String,
    pub path: PathBuf,
}

pub enum ParserUiAction {
    ValidateAndSave { old_name: Option<String>, name: String, source: String },
    Delete { name: String },
}

pub fn list(&self) -> Vec<String>;
pub fn add_new(&mut self);
pub fn edit(&mut self, name: &str);
pub fn request_open(&self, ctx: &egui::Context, name: &str);
pub fn take_parse_requests(&self) -> Vec<ParseRequest>;
pub fn handle_event(&mut self, event: ParserEvent);
pub fn is_running(&self) -> bool;
pub fn active_label(&self) -> Option<String>;
pub fn ui(&mut self, ctx: &egui::Context) -> Option<ParserUiAction>;
```

`request_open` loads the parser source before spawning `rfd::FileDialog::pick_file` on a named worker thread. The picker has an All files filter and title `Open file with <parser>`.

- [ ] **Step 4: Implement the centered editor and confirmations**

Use `egui::Window::new("Custom Python Parser")`, `.collapsible(false)`, centered `default_pos` plus `CENTER_CENTER` pivot, and the existing code editor theme. Save returns `ParserUiAction::ValidateAndSave`; `ScriptsPanel` sends `ValidateParser`, and actual `ParserLibrary::save(old_name, name, source)` occurs only after the matching `SyntaxValid { name }`. Delete is disabled without `saved_original_name`, and uses a centered non-collapsible confirmation before returning `ParserUiAction::Delete`. Runtime failure uses a centered concise popup while the full message remains available to app diagnostics.

- [ ] **Step 5: Share the existing engine through `ScriptsPanel`**

Add `parsers: ParsersPanel` to `ScriptsPanel`. Extend its constructor to accept scripts and parsers directories. Route `ScriptEvent::Parser(event)` to `parsers.handle_event(event)`; keep all other events on the current Console path. Dispatch parser parse requests and validation commands through `self.engine(store, sender, metrics)`, ensuring no second `ScriptEngine` is created.

- [ ] **Step 6: Run UI state tests**

Run: `cargo test -p delog-app parsers::tests scripts::tests`

Expected: parser panel and existing script panel tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/delog-app/src/parsers.rs crates/delog-app/src/scripts.rs crates/delog-app/src/main.rs
git commit -m "Add custom parser editor panel"
```

### Task 6: Tools Menu, Toolbar State, Icons, And Diagnostics

**Files:**
- Modify: `crates/delog-app/src/app.rs`
- Modify: `crates/delog-app/src/icons.rs`
- Create: `crates/delog-app/assets/icons/folder-open.svg`
- Modify: `crates/delog-app/tests/popup_policy.rs`

- [ ] **Step 1: Add failing policy/source tests**

Extend `popup_policy.rs` to read `src/parsers.rs` and assert both parser windows contain `.collapsible(false)` and centered placement. Add a source-policy assertion that `app.rs` contains `ui.menu_button("Parsers"` and both `pencil()` and `folder_open()` row actions.

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p delog-app --test popup_policy`

Expected: fails because menu and folder icon wiring are absent.

- [ ] **Step 3: Add the open-file icon**

Add the Lucide `folder-open` SVG with `viewBox="0 0 24 24"`, `fill="none"`, white stroke, round caps/joins, and the standard folder-open paths. Expose:

```rust
#[cfg_attr(not(feature = "scripting"), allow(dead_code))]
pub fn folder_open() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/folder-open.svg")
}
```

- [ ] **Step 4: Wire Tools ▸ Parsers exactly as approved**

Under the existing Tools menu, add a Parsers submenu with **Add new parser...**, separator, then sorted custom parser rows only. Each row uses a fixed-width filename label followed by pencil and folder-open image buttons with `Edit` and `Open file with parser` tooltips. Do not show built-in parsers.

Construct `ScriptsPanel` with sibling config paths:

```rust
let config = crate::layout::config_dir().unwrap_or_else(|| PathBuf::from("."));
ScriptsPanel::new(config.join("scripts"), config.join("parsers"))
```

- [ ] **Step 5: Combine native and Python load indicators and cancellation**

The toolbar loading branch must activate when either `session.has_active_loads()` or `scripts.parsers().is_running()`. Append the parser's active label, show a spinner for Python phases, and have Cancel call both `session.cancel_all()` and `scripts.request_interrupt()` when applicable. Do not claim numeric Python progress.

- [ ] **Step 6: Route full parser failures to diagnostics**

When `ScriptsPanel` receives `ParserEvent::Failed`, return a diagnostic payload to `app.rs`; emit `Diag::error("python-parser", full_message)` through `Session::push_diagnostic`. Keep the popup concise and include parser name plus selected filename.

- [ ] **Step 7: Run app tests and no-scripting compile**

Run: `cargo test -p delog-app --test popup_policy && cargo test -p delog-app && cargo check -p delog-app --no-default-features`

Expected: all tests pass and the app still compiles without Python/scripting.

- [ ] **Step 8: Commit**

```bash
git add crates/delog-app/src/app.rs crates/delog-app/src/icons.rs crates/delog-app/assets/icons/folder-open.svg crates/delog-app/tests/popup_policy.rs
git commit -m "Expose custom parsers in Tools menu"
```

### Task 7: Benchmark And User Documentation

**Files:**
- Modify: `crates/delog-script/Cargo.toml`
- Create: `crates/delog-script/benches/custom_parser.rs`
- Modify: `docs/scripting.md`

- [ ] **Step 1: Add the benchmark declaration**

Add Criterion to `delog-script` dev-dependencies and:

```toml
[[bench]]
name = "custom_parser"
harness = false
required-features = ["python"]
```

- [ ] **Step 2: Implement staged synthetic benchmarks**

Benchmark `read_float32_file` with a temporary file containing at least one million native-endian `f32` values. Benchmark Python call plus `parse_python_result` with a parser returning one RTC clock and 64 fields of equal length. Use `iter_batched` so file/result setup is outside the timed conversion iteration where appropriate. Name benchmark groups `python_parser/read_float32`, `python_parser/python_and_convert`, and include throughput bytes/elements.

- [ ] **Step 3: Document the exact user contract**

Add **Tools ▸ Parsers** to `docs/scripting.md`, including:

```python
import numpy as np

def Parse(raw_data):
    t = np.arange(raw_data.size, dtype=np.float64) * 0.01
    return [
        ("DATA.rtc", t, "time in seconds"),
        ("DATA.value", raw_data, "raw float32 values"),
    ]
```

Document native-endian float32 input/trailing-byte behavior, topic prefix grouping, RTC-over-index clocks, seconds-to-microseconds conversion, equal lengths, duplicate last-wins warnings, global parser storage, environment imports, trusted full-user execution, serialized worker behavior, memory copies, and native-call cancellation limits.

- [ ] **Step 4: Run benchmark smoke and documentation checks**

Run: `cargo bench -p delog-script --features python --bench custom_parser -- --sample-size 10`

Expected: all benchmark groups complete without panic and print timing/throughput.

Run: `git diff --check`

Expected: no whitespace errors.

- [ ] **Step 5: Commit**

```bash
git add crates/delog-script/Cargo.toml crates/delog-script/benches/custom_parser.rs docs/scripting.md Cargo.lock
git commit -m "Document and benchmark custom parsers"
```

### Task 8: Full Verification And Checklist Completion

**Files:**
- Modify: `PLAN.md`

- [ ] **Step 1: Format and run the full feature matrix**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p delog-app --no-default-features
cargo test -p delog-script --features python
```

Expected: every command exits 0 with no warnings.

- [ ] **Step 2: Run the parser benchmark once more after release-profile compilation**

Run: `cargo bench -p delog-script --features python --bench custom_parser -- --sample-size 10`

Expected: all groups complete; record read/Python/conversion results in the SCR-11 checklist note.

- [ ] **Step 3: Perform manual GUI verification**

Run: `cargo run -p delog-app`

Verify: add, syntax-error rejection, save, rename, edit, delete confirmation, open-file picker, unchanged legacy parser execution, toolbar spinner, successful browser source, field tooltip, runtime traceback diagnostic/popup, and Cancel behavior. Confirm built-in parsers do not appear under Tools ▸ Parsers.

- [ ] **Step 4: Mark SCR-11 complete with evidence**

Change SCR-11 to `[x]` and append the exact automated test totals, benchmark stage timings/throughput, manual GUI result, ZC exception metrics, and design/plan links. Do not mark complete if GUI verification or any required command failed.

- [ ] **Step 5: Commit checklist completion**

```bash
git add PLAN.md
git commit -m "Complete custom Python parsers"
```

- [ ] **Step 6: Inspect final history and worktree**

Run: `git log --oneline -10 && git status --short`

Expected: focused commits for Tasks 1-8 and no tracked implementation changes left uncommitted. Ignore the brainstorming companion's untracked `.superpowers/` directory; never include it in implementation commits.
