# Snapshot Scripting Ergonomics Design

## Context

DeLOG snapshot scripts currently expose a small Python API:

- `delog.sources()` returns string paths like `source/topic/field`.
- `delog.field(path)` materializes one field as `DelogField`.
- `delog.resample_prev(field, base_times)` aligns one field to another timeline.
- `delog.output(times_us, name).add_field(...)` emits derived topics.

This API is simple and stable, but common scripts spend too much code on
discovering paths, hardcoding source labels, aligning data manually, and
emitting boilerplate. The first ergonomic pass should preserve the existing
model while making the common snapshot workflow easier:

1. Find inputs without brittle full paths.
2. Align fields using object methods instead of standalone helpers.
3. Emit one derived topic in one call.

Live transforms will later need similar names for joins and contextual batches,
so these additions should establish reusable vocabulary without redesigning live
processing now.

## Goals

- Keep all existing scripting APIs working unchanged.
- Add structured discovery objects for sources, topics, and fields.
- Make ambiguous discovery fail with actionable candidate lists.
- Add table-style reads for multiple fields from one topic.
- Add method-based previous-sample alignment.
- Add a concise one-call snapshot emit helper.
- Keep output Float64-only and one timeline per emitted topic.

## Non-Goals

- No live transform behavior changes in this slice.
- No new output data types such as strings, ints, bools, or annotations.
- No UI changes.
- No mini query language or lazy execution engine.
- No automatic interpolation beyond the existing previous-sample behavior.

## Proposed API

### Input Discovery

Keep `delog.sources()` as the compatibility escape hatch. Add structured lookup:

```python
delog.catalog() -> Catalog
delog.find(topic, field=None, source=None, instance=None) -> TopicRef | FieldRef
delog.find_all(topic=None, field=None, source=None, instance=None) -> list[TopicRef | FieldRef]
delog.topic(name, source=None, instance=None) -> TopicRef
```

`delog.catalog()` returns a `Catalog` object with `sources()`, `topics()`, and
`fields()` methods. These return `SourceRef`, `TopicRef`, and `FieldRef`
objects respectively. It is the structured replacement for manually parsing
`delog.sources()`.

`delog.topic("IMU")` is equivalent to finding a topic by name. If it matches
exactly one live topic, it returns a `TopicRef`. If it matches none, it raises
`KeyError`. If it matches multiple topics, it raises `ValueError` listing the
candidate source/topic paths and asks the user to pass `source=` or `instance=`.

`delog.find("IMU", "AccX")` and `delog.find(topic="IMU", field="AccX")` return
a `FieldRef` when the result is unique. With `field=None`, `find` returns a
`TopicRef`. `find_all` returns all matches and never raises for ambiguity.

`instance` matches the numeric suffix already used in topic names such as
`IMU[0]`. A script can still match the full topic name directly, but
`instance=` avoids string formatting in common cases.

### Reference Objects

`TopicRef` is a lightweight descriptor, not materialized data:

```python
TopicRef.source: str
TopicRef.name: str
TopicRef.instance: int | None
TopicRef.path: str
TopicRef.fields() -> list[FieldRef]
TopicRef.field(name) -> FieldRef
TopicRef.read(*fields) -> DelogTable
```

`FieldRef` is also a descriptor:

```python
FieldRef.source: str
FieldRef.topic: str
FieldRef.name: str
FieldRef.unit: str | None
FieldRef.path: str
FieldRef.read() -> DelogField
```

The existing `delog.field(path)` keeps returning `DelogField`. A new overload or
companion path can accept `FieldRef` as well:

```python
accx = delog.topic("IMU").field("AccX")
field = delog.field(accx)
```

The preferred form is `accx.read()`.

### Tables

`TopicRef.read(*fields)` reads several fields from one topic into `DelogTable`:

```python
imu = delog.topic("IMU").read("AccX", "AccY", "AccZ")

imu.t
imu.AccX
imu["AccY"]
imu.fields()
```

Rules:

- All fields come from one topic and therefore share one timeline.
- Numeric fields expose `float64` arrays.
- String fields follow the existing read convention if included: `.s` support
  remains field-level, while table access should expose string columns as numpy
  unicode arrays.
- Missing fields raise `KeyError` with available field names for that topic.
- Empty `read()` reads every field in the topic. This supports quick
  exploration, while `TopicRef.fields()` still makes the field set explicit.

### Alignment

Keep `delog.resample_prev(field, base_times)` unchanged. Add method forms:

```python
gps_alt = delog.topic("GPS").field("Alt").read()
baro_alt = delog.topic("BARO").field("Alt").read()

gps_on_baro = gps_alt.align_prev(baro_alt)
gps_on_baro = gps_alt.align_prev(baro_alt.t)
```

`DelogField.align_prev(base)` accepts either another `DelogField`, a
`DelogTable`, or an `int64` timestamp array. It returns a `float64` numpy array,
matching `delog.resample_prev`.

`DelogTable.align_prev(...)` is deferred. The first implementation should make
the common one-field alignment read naturally before adding table-wide alignment
policy.

### Emission

Keep the builder API:

```python
out = delog.output(times, "derived")
out.add_field("value", values, unit="m")
```

Add a one-call helper for the common single-topic case:

```python
delog.emit("imu_derived", imu.t, {
    "Acc_norm": (norm, "m/s^2"),
    "AccX_smooth": smooth_x,
})
```

Accepted field entry forms:

- `values`
- `(values, unit)`

All values must be `float64`-compatible and the same length as `times`. This
helper buffers into the same pending-topic mechanism as `delog.output`, so
snapshot emission remains all-or-nothing.

`DelogTable.emit_as(topic_name)` is deferred. It would need a broader table
mutation API, so the first implementation should stay with explicit
`delog.emit(...)`.

## Example Workflow

Current style:

```python
import numpy as np

accx = delog.field("flight_42/IMU[0]/AccX")
accy = delog.field("flight_42/IMU[0]/AccY")
accz = delog.field("flight_42/IMU[0]/AccZ")

norm = np.sqrt(accx.v**2 + accy.v**2 + accz.v**2)

out = delog.output(accx.t, "imu_derived")
out.add_field("Acc_norm", norm, unit="m/s^2")
```

Proposed style:

```python
import numpy as np

imu = delog.topic("IMU", instance=0).read("AccX", "AccY", "AccZ")
norm = np.sqrt(imu.AccX**2 + imu.AccY**2 + imu.AccZ**2)

delog.emit("imu_derived", imu.t, {
    "Acc_norm": (norm, "m/s^2"),
})
```

Cross-topic alignment:

```python
baro = delog.topic("BARO").field("Alt").read()
gps = delog.topic("GPS").field("Alt").read()

delog.emit("altitude_compare", baro.t, {
    "BaroAlt": (baro.v, "m"),
    "GpsAlt": (gps.align_prev(baro), "m"),
    "Delta": (baro.v - gps.align_prev(baro), "m"),
})
```

## Error Handling

- Missing topic: `KeyError("topic 'IMU' not found")`.
- Missing field: `KeyError("field 'AccX' not found in topic 'IMU'; available: ...")`.
- Ambiguous topic/field lookup: `ValueError` with candidate paths and a hint to
  pass `source=` or `instance=`.
- Invalid emit lengths: same style as existing `DelogOutput.add_field` errors.
- Invalid emit field entry: `ValueError` naming the field and accepted forms.

## Testing

- Unit tests for topic/field path resolution, including topics containing `/`.
- Unit tests for ambiguous lookup and candidate-list errors.
- Unit tests for `TopicRef.read(...)` returning shared timestamps and named
  columns.
- Unit tests for `FieldRef.read()` matching `delog.field(path)`.
- Unit tests for `DelogField.align_prev(...)` matching `delog.resample_prev`.
- Golden script test covering discovery, table read, alignment, and `emit`.
- Documentation examples in `docs/scripting.md`.

## Rollout

Implement in a compatibility-first order:

1. Add Rust-side ref structs and Python classes for `TopicRef`, `FieldRef`, and
   `DelogTable`.
2. Add discovery methods: `catalog`, `find`, `find_all`, and `topic`.
3. Add `FieldRef.read()` and `TopicRef.read(...)`.
4. Add `DelogField.align_prev(...)`.
5. Add `delog.emit(...)`.
6. Update `docs/scripting.md` with examples for discovery, table reads,
   alignment, and one-call emission.

This order keeps each step independently testable and does not require changing
live transforms.
