# Live Python Transforms Design

## Context

DeLOG's current Python scripting API is snapshot-based. A script reads a
`StoreSnapshot`, computes derived arrays, and emits a complete `script:<name>`
source when the run finishes. This works for post-processing loaded data, but it
does not continuously update while live MAVLink batches keep arriving.

The goal is to add a scripting path for live transformations without replacing
the existing snapshot scripting mode.

## Scope

Version 1 supports same-topic live batch transforms:

- A script registers one or more transform functions.
- Each transform matches one input topic name and a declared field set.
- When a live batch for that topic arrives, DeLOG sends that batch to the script
  worker.
- The transform emits derived fields on the same timestamps as the input batch.
- Derived output is appended through the normal ingest path as a live derived
  source.

Version 1 does not support cross-topic joins, resampling, rolling windows,
multi-batch state, or transforming file parses while loading. Those features
need additional buffering, ordering, and replay semantics; they are deferred
until the append path is stable.

## Python API

Scripts register transforms with a decorator:

```python
DEG_TO_RAD = 0.017453292519943295

@delog.live_transform(
    topic="NAV_CONTROLLER_OUTPUT",
    fields=["nav_roll", "nav_pitch", "nav_bearing"],
    output_topic="NAV_CONTROLLER_OUTPUT_RAD",
)
def nav_controller_rad(batch):
    return {
        "nav_roll_rad": (batch.t, batch.nav_roll * DEG_TO_RAD, "rad"),
        "nav_pitch_rad": (batch.t, batch.nav_pitch * DEG_TO_RAD, "rad"),
        "nav_bearing_rad": (batch.t, batch.nav_bearing * DEG_TO_RAD, "rad"),
    }
```

`batch` exposes:

- `batch.t`: `np.ndarray[int64]` timestamps from the incoming batch.
- `batch.<field>`: one `np.ndarray[float64]` per declared input field.

The transform returns a dictionary from output field name to one of:

- `values`
- `(values, unit)`
- `(times, values, unit)`

For v1, output times must equal `batch.t` in length. If explicit `times` are
returned, they must match the input batch timestamps exactly. This keeps derived
topic append ordering simple and prevents hidden resampling semantics.

## Runtime Model

The existing `RunScript` command keeps its current snapshot behavior. Live
transforms are a new registration mode:

1. Running a script executes it once on the script worker.
2. Calls to `delog.live_transform(...)` register transform descriptors and Python
   callable handles in the worker.
3. The app attaches the registered transforms to the current session.
4. New live ingest batches are mirrored to the transform subsystem when they
   match a registered topic and field set.
5. The script worker executes matching transforms and returns derived
   `PendingTopic`/batch data.
6. The derived data is submitted through `IngestSender` as `SourceKind::Derived`.

Raw live ingest must not wait for Python. The live transform queue is bounded.
If it fills, DeLOG drops transform work, increments a dropped-transform metric,
and emits a rate-limited diagnostic. The original live telemetry remains
unaffected.

## Source and Topic Semantics

Each script run that registers live transforms owns one derived source named
`script:<name>`. Unlike snapshot scripts, this source is opened once and then
appended to as transform results arrive.

Rerunning a script with the same name replaces the previous transform generation:

- Stop routing new batches to the old generation.
- Mark the old derived source removed, matching existing replace-on-rerun
  behavior.
- Open a new `script:<name>` source for the new generation.

This preserves clear ownership and avoids mixing outputs from different script
versions. Plot layouts that reference old fields may need to be re-resolved, as
they do for current replace-on-rerun behavior.

## Architecture

Add a live-transform layer to `delog-script`:

- `LiveTransformSpec`: topic, required input fields, output topic, script name,
  generation id.
- `LiveTransformRegistry`: active specs published by the script worker to the
  app/session layer.
- `LiveTransformBatch`: copied input batch data ready for Python execution.
- `LiveTransformResult`: derived output arrays and units ready for ingestion.

The app/session layer owns routing because it already has both the live ingest
context and the script engine handle. `delog-core` remains UI- and Python-free.
The transform layer may copy live batch arrays into contiguous numpy buffers;
this is outside the render hot path and must be documented as a scripting
materialization exception, matching the existing snapshot script API.

## Error Handling

Registration errors fail the script run and print to the script console.

Per-batch transform errors do not stop raw live ingest. They:

- emit a traceback to the script console,
- increment a per-transform error count,
- emit a rate-limited diagnostic tied to `script:<name>`,
- disable the transform after a small consecutive-error threshold.

Invalid transform output, including wrong lengths or non-numeric arrays, is
treated as a per-batch transform error.

## Testing

Unit tests:

- decorator registration captures topic, fields, output topic, and callable name,
- invalid registrations are rejected,
- output validation rejects mismatched lengths,
- rerunning a script replaces the previous generation.

Integration tests:

- a synthetic live `NAV_CONTROLLER_OUTPUT` batch produces appended radian fields,
- non-matching topics do not execute transforms,
- missing required fields do not execute transforms and produce a diagnostic,
- a full transform queue drops derived work without blocking raw live ingest.

Manual verification:

- start a live MAVLink stream,
- run a saved transform script,
- plot derived fields,
- confirm derived traces continue appending as raw live data arrives.
