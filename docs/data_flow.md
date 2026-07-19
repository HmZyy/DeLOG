# DéLOG Data Flow Editor

The Data Flow editor builds derived numeric signals as a visual graph. A graph
reads fields from the current data snapshot, applies operations, and publishes
its outputs as a derived source named **`dataflow:<name>`**. It uses DéLOG's
native evaluation engine, so it works without Python and is available in
`--no-default-features` builds.

Data flows are snapshot-only. Evaluation processes the data loaded when you
request it; it does not continue processing future live batches.

- [Opening the editor](#opening-the-editor)
- [Building a graph](#building-a-graph)
- [Timelines and alignment](#timelines-and-alignment)
- [Units and NaN gaps](#units-and-nan-gaps)
- [Publishing](#publishing)
- [Saving graphs](#saving-graphs)
- [Worked examples](#worked-examples)
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

Click **Publish** to evaluate all Output nodes on a worker thread. Publication
is all-or-nothing: an unresolved input, timeline conflict, missing connection,
or invalid output prevents the entire derived source from being emitted. The
diagnostics stay attached to their nodes and errors also go to the Logging dock.

Publishing a graph named `altitude-check` creates
`dataflow:altitude-check`. Publishing it again replaces the previous source of
that name, so reruns do not accumulate duplicates. Original sources and their
samples are never mutated.

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

## Limitations

- Data flows process snapshots only. Publish again after loading or receiving
  more data.
- Data selection and arithmetic are numeric-only.
- Arithmetic nodes are binary; chain nodes for three or more inputs.
- There is no resample-by-rate, expression, moving filter, derivative,
  saved-script, or runtime-parameter node yet.
- Live execution, maximum-gap alignment, plot-preview outputs, drag-to-empty
  insertion, and workspace-tile embedding are deferred.

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
   output port type in `graph.rs`.
2. Encode and decode its tagged fields in `doc.rs`, preserving the existing
   versioning and unknown-node behavior.
3. Implement its numeric kernel and diagnostics in `eval.rs`.
4. Add its entry and aliases to the application registry.
5. Add graph, document round-trip, evaluator, cache, and UI-registry tests as
   appropriate.

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
