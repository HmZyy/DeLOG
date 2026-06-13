# Python Scripting for Derived Fields — Design

**Date:** 2026-06-13
**Status:** Approved design, pending implementation plan
**Supersedes/expands:** PLAN.md `BLG-07` (Python scripting for derived fields); shares substrate with `ANA-04` (built-in derived fields) and the §4.6 source-removal path.

---

## 1. Goal

Let an engineer write and run Python scripts that read the loaded dataset and
produce **new** fields/topics, with an in-app interactive REPL + code editor and
a reusable library of saved scripts. The whole capability is **optional at build
time** — DeLOG must build and run with no Python toolchain present.

This is a *trusted-user analysis tool*, not a sandbox: scripts run with full host
privileges (see §10).

---

## 2. Requirements (decided)

| Decision | Choice |
| --- | --- |
| Python runtime | **Embedded CPython via `pyo3`** (`auto-initialize`) — scripts get numpy/scipy. |
| Mutation scope | **Add-only.** Scripts emit new fields/topics as a derived source; canonical log data is never mutated. "Altering" a field means publishing a new derived version alongside it. |
| Input/output model | Fields handed to scripts as **numpy arrays** (`t`: int64 µs, `v`: typed). Scripts return new fields as explicit `(times_us, values)` they build themselves. A `delog.resample_prev` helper covers cross-topic alignment. |
| Run lifecycle | **Manual run, replace-on-rerun.** Re-running removes the prior `script:<name>` source and republishes. Live sources are snapshotted at run time. |
| Console | **Interactive REPL** with a persistent interpreter session shared with the editor; output (stdout/stderr/tracebacks) captured inline. |
| Script scope | **Global library** of standalone `.py` files in the app config dir, reusable across any loaded log. |
| Build optionality | **`scripting` feature, OFF by default.** Plain `cargo build --workspace` needs no Python. |

---

## 3. Architecture

### 3.1 Integration approach (chosen: A)

A new crate sits parallel to `delog-parsers`:

```
delog-app ──► delog-script ──► delog-core
```

`delog-script` owns a long-lived worker thread holding the CPython interpreter +
GIL. It **reads** the store via `StoreSnapshot`/`FieldView` and **writes** by
submitting `ParsedBatch`es through the existing `IngestSink` as a derived source.
Derived fields therefore inherit chunking, per-chunk stats, the render cache, GPU
rendering and layout persistence with **zero downstream special cases** — exactly
the PLAN §17.3 decision. The engine is headless-testable; no `egui` types leak
below `delog-app`. Only the REPL/editor UI lives in `delog-app`.

Rejected alternatives:
- **B — pyo3 directly in `delog-app`:** drags Python/numpy into the shell, makes
  the engine untestable headless, blurs §3.2 boundaries.
- **C — out-of-process Python (Arrow IPC):** best isolation, but heavy plumbing,
  two-way serialization, awkward REPL state, external interpreter management.
  Recorded as the future hardening path if real isolation is ever needed.

### 3.2 Dependency placement

- New crate `crates/delog-script`. Deps when its `python` feature is on:
  `delog-core` + `pyo3` + `numpy` (all pinned in the workspace table). The crate
  **never** depends on `egui`, `delog-cache`, `delog-render`, or `delog-stream`.
  Dependency edge is strictly downward (`app → script → core`).
- `delog-app` gains a `scripts/` UI module, gated behind its `scripting` feature;
  that feature also pulls in `egui_code_editor` (pinned) for the editor (§8).

---

## 4. The derived-source substrate (unconditional, shared) — `SCR-01`

Two small `delog-core` additions, compiled into **every** build (no Python). They
are what this feature needs *and* the missing pieces of `ANA-04` and §4.6:

- **`SourceKind::Derived`** — bounded, file-like seal semantics (64Ki rows, like
  `File`), but flagged distinctly so the data browser can give it its own icon and
  so it stays out of recent-files.
- **`IngestMsg::RemoveSource { source }`** — tombstones the source in the
  `IdentityRegistry`, drops its orphaned `TopicStore`s, and republishes a snapshot
  without it. This *is* replace-on-rerun, and it completes the §4.6 removal path
  that currently exists only as the `removed` tombstone flag honored by readers.

Both are exercised by unit tests in `delog-core` independent of scripting.

---

## 5. Execution model

A single **script worker thread** owns the interpreter for the app's lifetime.
The UI thread sends commands over a channel and only renders streamed results —
it never blocks (PLAN §19.6, the never-block rule).

Commands (UI → worker):
- `Eval(line)` — evaluate one REPL line in the persistent namespace.
- `RunScript { name, source }` — run a full script buffer.
- `Cancel` — cooperative interrupt (see §10).

Events (worker → UI), streamed:
- `Output { stream, text }` — captured stdout/stderr chunks.
- `Result(repr)` — REPL expression value repr.
- `Error(traceback)` — Python exception text.
- `Done { name, emitted_source, rows, elapsed }` / `Failed { name }`.

Worker behavior:
- Redirects `sys.stdout`/`sys.stderr` to a writer that streams chunks back.
- Exposes the `delog` module (§6).
- Pins a **fresh `StoreSnapshot` per top-level run/eval** — `delog` read calls do
  `store.load()` at call time, so live data is snapshotted at the run moment.
- On `RunScript`: buffers all fields the script emits; on success, submits them as
  **one** derived source `script:<name>` (`SourceKind::Derived`) via `IngestSink`,
  then issues `RemoveSource` for any prior `script:<name>` (replace-on-rerun). On
  failure, **nothing** is emitted (no partial source).
- REPL and editor share the same persistent namespace.

---

## 6. The `delog` Python API — `SCR-03`

```python
delog.sources()                          # introspect: source/topic/field paths, units, dtypes
f = delog.field("flight_42/IMU/AccX")    # -> .t (int64 µs np), .v (float64 np), .unit, .dtype
delog.resample_prev(f, base_times)       # prev-sample align onto another timeline (the ANA-08 helper)

out = delog.output("MyDerived", times_us)  # derived topic; times shared by all its fields
out.add_field("AccMag", values, unit="m/s^2")
# fields the script builds are flushed as one `script:` source when the run completes
```

The `times_us` array is defined **once per output topic**, mirroring the chunk
model (one timestamp column + value columns), so every field in a topic shares a
timeline and there is no per-field alignment ambiguity.

- **Field path** is `source/topic/field` by name. A path missing in the current
  log raises a clean Python error (and a `Diag`), so library scripts degrade
  gracefully across logs.
- **Read path (ZC note):** a field spans many chunks, so materializing one numpy
  array **concatenates them — one copy, on the worker thread, off the render hot
  path.** Numeric values are exposed as `float64` (NaN-preserving) and times as
  `int64`; this covers derived-field math. Exact int/bool dtype passthrough is a
  documented v1 simplification, deferred. Marked `// ZC-EXCEPTION: script
  materialization` with a counter metric, consistent with how the One Copy Rule
  treats the render cache. The canonical store is never copied for rendering.
- **Write path:** numpy → Arrow `Float64Array` (one copy) → `ParsedBatch`. Output
  times are explicit `int64` µs; the ingest thread already sorts/validates
  defensively (ING-05). **NaN is preserved end to end** — a gap marker in, a gap
  (line break) out; never interpolated away.

---

## 7. Persistence — `SCR-06`

Global library of standalone `.py` files in the app config dir next to
`settings.json` (e.g. `~/.config/delog/scripts/*.py`). Filename = display name;
files are externally editable. The Scripts panel lists, loads, saves, and deletes
library files. The REPL namespace is ephemeral; useful REPL work is promoted into
a saved file via the editor.

---

## 8. UI — `SCR-07`

A toggleable **Scripts** window (`egui::Window`, following the existing
`vehicle_dialog` window pattern), opened from a new **Tools ▸ Scripts** menu
entry. (The Tools menu does not exist yet; it is added here, as PLAN §19.2 already
specs it. The bottom dock of §19.1 is also not built yet, so a window is the
least-invasive home for v1; it can migrate into the dock when that lands.) Layout
inside the window:

- **Left:** library list (the `.py` files) with new/save/delete.
- **Center:** code editor — **`egui_code_editor`** (pinned in the workspace table,
  pulled in only under `delog-app`'s `scripting` feature) configured for Python
  syntax highlighting, line numbers, and a dark theme matching §19.4.
- **Bottom:** REPL console — input line + scrollback of captured
  stdout/stderr/results, with **Run / Cancel / Save** buttons and a status line
  (run state, timing, emitted source).

The entire `scripts/` UI module + the Tools menu entry are
`#[cfg(feature = "scripting")]`. The `cfg` surface is confined to the optional
dependency and this one module's registration — no scattered `cfg`s.

---

## 9. Build optionality — `scripting` feature

- `delog-core` substrate (§4) is **always** compiled (no Python).
- `delog-script` carries its own `python` feature that pulls `pyo3` + `numpy` +
  the engine. **With the feature off the crate compiles to essentially nothing**,
  so `cargo build --workspace` needs no Python toolchain.
- `delog-app`'s `scripting` feature enables `delog-script/python` and gates the
  Scripts UI.
- **OFF by default.**

Build matrix (to be added to CLAUDE.md Commands):
```bash
cargo build --workspace                                   # no scripting, no Python needed (default)
cargo build --workspace --features delog-app/scripting    # full Python scripting
```

---

## 10. Errors, cancellation, safety

- **Errors:** a Python traceback streams to the console **and** emits a `Diag`
  into the diagnostics hub. A failed run emits **no** partial source.
- **Cancellation:** cooperative. Cancel raises `KeyboardInterrupt` in the running
  script (like Ctrl-C, via the interpreter's pending-interrupt mechanism). Long
  pure-C calls (e.g. a large numpy op) cannot be interrupted mid-call —
  documented limitation.
- **Safety:** embedded CPython is **not** sandboxed; scripts run with full host
  privileges (filesystem, network). This is an explicit, documented trusted-user
  feature. Approach C (out-of-process) is the recorded future path if isolation
  is ever required.

---

## 11. Testing — `SCR-08`

- Headless engine tests: read field → numpy; run a script that emits a field →
  assert a `script:` source appears in the published snapshot with expected values.
- Golden fixture script (e.g. accel magnitude) over a fixture log → expected output.
- numpy↔Arrow dtype round-trip, including **NaN-gap preservation**.
- Error path: a script raising an exception → `Diag` emitted, **no** partial source.
- `proptest` for `resample_prev` against a naive prev-sample scan.
- `delog-core` substrate tests for `SourceKind::Derived` seal behavior and
  `IngestMsg::RemoveSource` (tombstone + store drop + republish), independent of
  the `scripting` feature.

Definition of Done per PLAN §0 applies: clippy-clean (both with and without the
`scripting` feature), tests green, checklist updated in the same commit.

---

## 12. Checklist additions (PLAN.md §22)

New `SCR` area; `BLG-07` marked superseded-by-`SCR`:

- `SCR-01` — Derived-source substrate in `delog-core`: `SourceKind::Derived`
  (file-like seal) + `IngestMsg::RemoveSource` (tombstone/drop/republish).
  *Shared with `ANA-04` and §4.6.*
- `SCR-02` — `delog-script` crate scaffold; `python` feature pinning `pyo3` +
  `numpy`; interpreter worker thread + command/event channels.
- `SCR-03` — `delog` Python API: `sources()`, `field()` → numpy, `resample_prev`,
  `output()` builder → `ParsedBatch`.
- `SCR-04` — Run-script lifecycle: emit one derived source per run + replace-on-rerun.
- `SCR-05` — REPL eval loop; stdout/stderr capture streaming; cooperative cancel.
- `SCR-06` — Global script library persistence (config-dir `.py` files): list/load/save/delete.
- `SCR-07` — Scripts dock tab + Tools menu entry in `delog-app` (feature-gated).
- `SCR-08` — Tests: engine, golden script, numpy↔Arrow round-trip, error path,
  `resample_prev` proptest, substrate tests.
- `delog-app` `scripting` feature (OFF by default) wiring + CLAUDE.md build-matrix update.
