# DéLOG Data Flow Editor

The Data Flow editor builds derived numeric signals as a visual graph. A graph
reads fields from the current data snapshot, applies operations, and publishes
its outputs as a derived source named **`dataflow:<name>`**. It uses DéLOG's
native evaluation engine, so it works without Python and is available in
`--no-default-features` builds.

Evaluation processes a snapshot of the current data on request. When a live
MAVLink link is connected, the currently loaded flow keeps recomputing as new
samples arrive - see [Live data](#live-data). Otherwise evaluation is a
one-shot snapshot: publish again after loading or receiving more data.

- [Opening the editor](#opening-the-editor)
- [Building a graph](#building-a-graph)
- [Timelines and alignment](#timelines-and-alignment)
- [Units and NaN gaps](#units-and-nan-gaps)
- [Publishing](#publishing)
- [Live data](#live-data)
- [Saving graphs](#saving-graphs)
- [Worked examples](#worked-examples)
- [Python Script node](#python-script-node)
- [Limitations](#limitations)
- [Developer guide](#developer-guide)

---

## Opening the editor

Choose **File > Data Flow**. The floating window remains available when no log
is loaded, so you can build or edit a graph skeleton before opening data.

The toolbar shows the graph name, persistence and undo controls, publication
status, and a reminder that execution uses the current snapshot. The canvas is
in the center and the selected node's inspector is on the right.

## Building a graph

Right-click the canvas to open the unified Add menu. Start typing to fuzzy-search
node names and aliases, use the arrow keys to move the highlight, and press
<kbd>Enter</kbd> to add the selected node. The arithmetic symbols `+`, `-`, `*`,
and `/` select their corresponding operations.

Choose **Add Data...** to switch the same menu into its data picker. The picker
searches source labels, topics, fields, and units in the current snapshot. It
lists numeric fields only. Picking a field creates a Data Field node whose
selector is saved with the graph.

Drag from a node's output port on the right to an input port on the left. An
input accepts one connection. DéLOG rejects self-links, cycles, invalid ports,
and incompatible scalar/signal connections, and reports the reason in the
Logging dock.

Click a node to select it. Its inspector shows parameters, diagnostics, and the
latest preview statistics. Constant values and Scale / Offset parameters may
also be edited directly on the canvas. Drag a node by its body to move it.
Use **Undo** and **Redo** for graph edits; one drag is one undoable move. Press
<kbd>Delete</kbd> or <kbd>Backspace</kbd> to remove the selected node.
Select an edge and press <kbd>Delete</kbd> or <kbd>Backspace</kbd> to disconnect it.

## Timelines and alignment

Two signals having the same number of rows does not mean their samples occurred
at the same times. Every evaluated signal carries a `TimelineId`, and binary
signal operations require both inputs to carry the same timeline. If they do
not, the operation reports:

> Timeline mismatch: the inputs do not share the same timestamps. Add an Align
> node before this operation.

An **Align to Timeline** node has Data and Reference inputs. It samples Data at
the Reference timestamps, returns a signal on the Reference timeline, and keeps
the Data unit. Its modes are:

- `prev`: use the latest Data sample at or before each Reference timestamp.
- `nearest`: use the closest Data sample; ties choose the earlier timestamp.
- `linear`: interpolate between the neighboring Data samples.

The exact endpoint, duplicate-timestamp, and NaN rules are shared with Python
scripting and documented under
[`DelogField.align`](scripting.md#delogfieldalignbase-modeprev---npndarrayfloat64).

## Units and NaN gaps

Units are metadata. The evaluator does not perform dimensional conversion.

| Operation | Output unit |
| --- | --- |
| Data Field | The source field's unit |
| Constant | Scalar; no unit |
| Add / Subtract | Preserved when both input units match; otherwise cleared with a diagnostic |
| Multiply / Divide by a Constant | The signal input's unit |
| Multiply / Divide two signals | Cleared |
| Scale / Offset | The input unit |
| Align to Timeline | The Data input unit |
| Output field | The configured override, or the incoming signal unit when no override is set |

NaN represents a gap. Input NaNs propagate through arithmetic, division may
produce non-finite values according to normal floating-point rules, and Align
does not fill a source NaN. Plots render NaN as a line break. This makes missing
data visible instead of silently turning it into a plausible value.

## Publishing

Add a **Derived Topic Output** node for each topic the graph should publish.
In its inspector, set the topic name and add one row per output field. Each row
has a field name and an optional unit override. Connect every field input to a
signal on the same timeline.

Click **Run** to evaluate all Output nodes on a worker thread. Publication
is all-or-nothing: an unresolved input, timeline conflict, missing connection,
or invalid output prevents the entire derived source from being emitted. The
diagnostics stay attached to their nodes and errors also go to the Logging dock.

Publishing a graph named `altitude-check` creates
`dataflow:altitude-check`. Publishing it again replaces the previous source of
that name, so reruns do not accumulate duplicates. Original sources and their
samples are never mutated.

## Live data

Evaluation described above processes one snapshot per request. When a live
MAVLink link is connected, the currently loaded flow instead re-evaluates
automatically on a throttled cadence as new samples arrive - there is no
toggle for this; it follows the link. Node preview statistics accumulate over
the whole live session, not just the most recent recompute window.

Clicking **Run** while live seeds the derived source from all data already in
the store, then keeps appending new samples as they arrive. The resulting
`dataflow:<name>` topic behaves like any other live topic: it plots normally,
its extent keeps growing as new samples publish, and it keeps updating even
after the Data Flow editor window is closed.

Without a live link connected, nothing changes: the preview still updates on
edit, and **Run** still publishes a one-shot snapshot.

Only the currently loaded flow updates live. Loading a different flow does
not carry the update forward - the previous flow's already-published
`dataflow:<name>` data remains in the store, frozen at whatever it last
computed.

**Settings > Data Flow** exposes two parameters for the live recompute:

| Setting | Meaning |
| --- | --- |
| Overlap (s) | How far before the newest sample each live re-evaluation reaches back, so lookback nodes such as Align keep finding a prior sample |
| Interval (ms) | Minimum time between live re-evaluations |

## Saving graphs

The name field is both the graph name and its library filename. **Save** writes
a versioned JSON document. **Load** lists saved graphs; when the current graph
has unsaved changes, the first click arms the load and the second confirms it.
Saved data flows form a global library:

| Platform | Location |
| --- | --- |
| Linux | `~/.local/share/delog/dataflows/` |
| macOS | `~/Library/Application Support/DeLOG/dataflows/` |
| Windows | `%APPDATA%\DeLOG\data\dataflows\` |

A Data Field selector records the source label when one was selected, plus the
topic, optional instance, and field name. Loading a graph never discards a node
because its field is absent from the current log. The node remains unresolved
and reports a diagnostic; it can resolve again when a matching log is loaded.
This lets one library graph be reused across recordings with consistent names.

## Worked examples

### Scale a field and publish it

To convert a field with a linear transformation:

```text
Data Field -> Scale / Offset -> Output
```

Pick the input field, set `multiplier` and `offset`, then configure one Output
field. For example, a multiplier of `0.01` converts centimeters to meters; set
the Output field's unit override to `m` because Scale / Offset deliberately
preserves the input unit metadata rather than guessing a conversion.

### Compare GPS and barometric altitude

GPS and barometer topics normally have independent sample times. Use the BARO
signal as the comparison timeline:

```text
GPS.Alt  -> Align to Timeline (Data) -> Subtract (A) -> Output
BARO.Alt -> Align to Timeline (Reference)
BARO.Alt --------------------------------> Subtract (B)
```

Choose the alignment mode appropriate for the data. The Align output now has
the BARO timeline, so Subtract can compare it with `BARO.Alt`. Aligning before
Subtract is essential: equal array lengths, when they happen, do not establish
sample-by-sample correspondence.

## Python Script node

The **Python Script** node runs a small user-written Python function inside
the graph, with N wired inputs and M named outputs, evaluated on the same
worker thread and snapshot-only model as every other node. It requires a
build with the `scripting` feature (the default build has it); see
[Feature-gating](#feature-gating) below for what happens without it.

Add one from the unified Add menu (category **Script**; aliases `python`,
`py`, `script`, `code`). Its inspector lets you edit:

- **Name** - the node's label.
- **Inputs** - a row per input port (name, with Add/Remove); wire a Data
  Field, constant, or any upstream signal to each one.
- **Outputs** - a row per output port (name and an optional unit override,
  with Add/Remove); each becomes its own output socket, connectable like any
  other node's output.
- **Code** - a Python source buffer with an **Apply** button. Editing the
  buffer does not re-evaluate the graph on every keystroke; Apply commits the
  code as one undoable edit and triggers one re-evaluation.

### The `flow(inputs)` contract

The script must define a module-level function `flow(inputs)` that returns a
`dict` keyed by output port name:

```python
import numpy as np

def flow(inputs):
    # inputs.<port> -> a signal object for a wired signal input:
    #   .t    np.int64 timestamps, microseconds
    #   .v    np.float64 values
    #   .unit str | None
    # a scalar-wired port (e.g. a Constant node) arrives as a plain float
    a = inputs.a
    return {
        "sum":  a.v + inputs.b.v,           # bare values: inherits a shared input timeline
        "half": (a.t[::2], a.v[::2]),       # explicit (times, values)
        "cal":  (a.t, a.v * 2.0, "m/s^2"),  # explicit (times, values, unit)
    }
```

Each output's value in the returned dict takes one of three forms:

- **`values`** - a 1-D array; the evaluator assigns it a timeline per the
  rules below.
- **`(times, values)`** - explicit timestamps, no unit.
- **`(times, values, unit)`** - explicit timestamps and an explicit unit
  string.

The dict's keys must be exactly the node's declared output names (no
missing or extra keys). `times`, when given, must be a 1-D `int64` array
sorted ascending (equal adjacent timestamps are allowed); `values` must be a
1-D array coercible to `float64` and the same length as `times`.

### Timeline rules

The evaluator - not the Python code - assigns each output a `TimelineId`,
applied per output in declared order:

1. **Bare `values`** (no explicit times) are legal only when every wired
   signal input to the node shares one timeline and the returned array's
   length matches it; the output inherits that timeline. A node with no
   signal inputs, or with signal inputs on different timelines, must return
   explicit times - otherwise:
   `Output '<name>' must return explicit times because the inputs are on
   different timelines.`
2. **Explicit `times` equal to a wired input's timestamps** (matching
   length and values) reuse that input's timeline.
3. **Explicit `times` equal to an earlier output of the same evaluation**
   share that output's timeline, so one script node can produce several
   outputs on one shared new axis, or an output that shares its own input's
   axis, without an Align node in between.
4. **Otherwise**, the output gets a fresh timeline
   (`TimelineId::NodeOutput`) distinct per output index.

An output's unit is the port's configured unit override if set, otherwise
the unit the script returned (or none).

### Purity and caching

Node scripts run in a **fresh Python namespace on every evaluation** - no
persistence between evaluations, no REPL globals, and **no snapshot access**:
a script node cannot call anything like `delog.topic(...)`, `delog.emit`, or
read runtime variables. It is purely a function of its wired input values.
This keeps it cacheable under the same fingerprint rule as every other node
(`hash(node kind JSON, upstream fingerprints)`): editing the code or a port
invalidates only that node and everything downstream; unrelated nodes keep
their cached values. An impure script (using `time`, `random`, file I/O) is
possible - it is not sandboxed - but its cached result can go stale exactly
like any other impure operation; this is a documented trade-off, not a bug.

### Errors, print, and cancellation

A raised Python exception becomes that node's diagnostic (with traceback),
shown in its inspector, exactly like a native kernel error. **`print(...)`**
output goes to the **Scripting Console** (`View > Scripting`, <kbd>F9</kbd>),
not the node itself, because node scripts run on the same embedded-Python
worker as regular scripts. A script stuck in a `while True:` loop can be
cancelled the same way any other evaluation is superseded: editing another
node (or re-evaluating) requests cancellation, which raises
`KeyboardInterrupt` in the running script at its next bytecode boundary - a
script stuck inside one long C call cannot be interrupted mid-call, matching
the limitation documented for [Python scripting](scripting.md#console-errors-and-cancellation).

### Feature-gating

The Script node lives entirely behind the `scripting` cargo feature (the
same feature that gates Python scripting generally) and pulls in no Python
dependency when off. In a `--no-default-features` build:

- The node cannot be added (its template is absent from the Add menu).
- A saved graph containing one still **loads without crashing**: the node
  renders as a disabled Unknown node (same fallback as any node type from a
  newer DéLOG the current build doesn't recognize) and evaluating it reports
  a diagnostic instead of running Python.
- **Saving that graph back writes the script node's JSON byte-for-byte
  unchanged** - loading and re-saving in a Python-less build never loses or
  mangles the code or port declarations.

## Limitations

- Data flows process snapshots only. Publish again after loading or receiving
  more data.
- Data selection and arithmetic are numeric-only.
- Arithmetic nodes are binary; chain nodes for three or more inputs.
- There is no resample-by-rate, expression, moving filter, derivative, or
  runtime-parameter node yet. (A Python Script node covers ad hoc arithmetic
  and simple filters without leaving the graph; see above.)
- Maximum-gap alignment, plot-preview outputs, drag-to-empty insertion, and
  workspace-tile embedding are deferred.
- A **Script** node that needs unbounded history (for example, a cumulative
  integral since t = 0) is correct only via a full **Run**, not in windowed
  live recompute; its values at the recompute-window seam can be wrong.
- Aligning against a very sparse reference signal may need a larger live
  recompute overlap margin to find the previous sample; a full **Run** is
  always correct regardless of overlap.

Use [Python scripting](scripting.md) when an analysis needs arbitrary NumPy,
string data, custom algorithms, or live transforms.

---

## Developer guide

### Document schema

Data-flow files are versioned at the document root. Nodes use a flat tagged
representation; edges identify the destination input by its zero-based port
number. A minimal graph looks like this:

```json
{
  "delog_dataflow": 1,
  "name": "scaled-altitude",
  "next_id": 4,
  "viewport": { "offset": [0.0, 0.0], "zoom": 1.0 },
  "nodes": [
    {
      "id": 1,
      "pos": [20.0, 40.0],
      "type": "data_field",
      "source": "flight_01",
      "topic": "GPS",
      "instance": 0,
      "field": "Alt"
    },
    {
      "id": 2,
      "pos": [240.0, 40.0],
      "type": "scale_offset",
      "multiplier": 0.01,
      "offset": 0.0
    },
    {
      "id": 3,
      "pos": [460.0, 40.0],
      "type": "output",
      "topic": "ALTITUDE",
      "fields": [{ "name": "alt", "unit": "m" }]
    }
  ],
  "edges": [
    { "from": 1, "to": 2, "to_port": 0 },
    { "from": 2, "to": 3, "to_port": 0 }
  ]
}
```

Version 1 readers reject an unsupported document version. An unknown node type
is retained as raw JSON and written back unchanged. The canvas can therefore
round-trip graphs made by a newer DéLOG even though it cannot evaluate the
unknown node.

Edges carry a `from_port` (zero-based, selecting one of the source node's
output ports) in addition to `to_port`. Nodes with more than one output -
today, only the Python Script node - use it to say which of their outputs an
edge draws from:

```json
{ "from": 2, "from_port": 1, "to": 3, "to_port": 0 }
```

The document is versioned by **minimum required version**, not by feature
presence: `from_port` is omitted entirely when it is `0` (the default, and
every single-output node's only port), and the root `"delog_dataflow"` field
is written as `2` only when at least one edge in the graph has a non-zero
`from_port`; otherwise it is written as `1`. This means a graph containing a
Script node with exactly one output still saves as version 1 and loads in
older builds (the node round-trips as `Unknown` there, per the existing
degrade path); only a graph that actually wires from a second-or-later output
port requires a version-2-aware reader. Readers accept versions 1 and 2.

### Crate layout

The native backend lives in `crates/delog-flow`:

| Module | Responsibility |
| --- | --- |
| `graph` | Node kinds, typed ports, edges, cycle checks, IDs, and viewport |
| `command` | Reversible graph edits and batch rollback for undo/redo |
| `doc` | Versioned JSON encoding, validation, and unknown-node preservation |
| `resolve` | Data Field selector resolution against immutable snapshots |
| `types` | Signals, scalars, metadata, and `TimelineId` |
| `eval` | Dependency ordering, kernels, diagnostics, cancellation, and cache |
| `publish` | Output validation and all-or-nothing topic preparation |

The application-side `dataflow` module owns the template registry, metadata
picker, persistent graph store, background controller, canvas, inspector, and
floating window. The editor canvas is a thin adapter over `egui_graph`. DeLOG
retains ownership of the persisted graph, typed-port validation, commands, undo
history, evaluation, and publication; the crate supplies node interaction,
sockets, edges, selection, pan, and zoom. Shared alignment, topic-instance
parsing, and derived-topic preparation live in `delog-core`, so scripting and
data flows use one implementation.

### Adding a node kind

When adding an operation:

1. Add its `NodeKind` variant, display label, input port specifications, and
   `outputs()` (a `Vec<PortSpec>`; most kinds return one entry, but nothing
   stops a kind from declaring more, as the Script node does) in `graph.rs`.
2. Encode and decode its tagged fields in `doc.rs`, preserving the existing
   versioning and unknown-node behavior. If the kind can have more than one
   output port, confirm `to_json`/`from_json` still write the minimum
   required document version (see [Document schema](#document-schema)).
3. Implement its numeric kernel and diagnostics in `eval.rs`, returning one
   `Value` per declared output port, in port order.
4. Add its entry and aliases to the application registry.
5. Add graph, document round-trip, evaluator, cache, and UI-registry tests as
   appropriate, including a case with more than one output port if the kind
   supports it.

Changing a node's inputs also requires checking saved-edge compatibility and
whether the data-flow document version must change.

### Timeline invariants and caching

`TimelineId::Topic` identifies a resolved input topic. Operations that preserve
timestamps preserve that ID. Align creates values at the Reference timestamps
and adopts the Reference ID. Derived timelines can use `TimelineId::Node` when
an operation creates a new time axis. Binary signal kernels compare IDs before
combining values; row count is not a substitute for timeline identity.

Evaluation follows only the dependencies needed by selected previews and
Output nodes. Each node cache key includes its kind and upstream fingerprints;
unchanged subgraphs reuse their values across evaluations. Cancellation and
generation checks prevent an obsolete worker result from replacing a newer
edit's preview or publication request.

### Publication path

The evaluator produces values and node diagnostics without touching the data
store. `publish::build_outputs` validates every Output node, requires a common
timeline within each topic, and prepares all topics together. Only after that
whole preparation succeeds does the application controller send them through
`delog-core`'s derived-topic helpers and the normal ingestion channel. A
successful republish removes the controller's previous `dataflow:<name>` source
after the replacement has been prepared, keeping graph evaluation separate
from UI and store mutation.
