# DéLOG Python Scripting

DéLOG can run Python scripts that read the loaded dataset and **produce new
fields and topics** - derived signals that appear in the data browser and plot
exactly like parsed log data. Scripts get the full embedded CPython interpreter
(including `numpy`), so derived-field math is just numpy.

For simple numeric derived signals, the [Data Flow editor](data_flow.md) is a
visual alternative that does not require Python. That editor also has its own
[Python Script node](data_flow.md#python-script-node), for when one step of
an otherwise-visual graph needs arbitrary code: it runs on this same embedded
interpreter and worker thread (so its `print()` output lands here, in the
Scripting Console), but under a narrower, snapshot-only, no-`delog`-object
contract suited to a single graph node rather than a whole script.

This is an **optional, build-time feature**. It is off by default.

- [Enabling scripting](#enabling-scripting)
- [Bundled distribution (no system Python required)](#bundled-distribution-no-system-python-required)
- [The Scripts UI](#the-scripts-ui)
- [Custom file parsers](#custom-file-parsers)
- [How scripts produce data](#how-scripts-produce-data)
- [Declarative operations](#declarative-operations)
- [Live transforms](#live-transforms)
- [Runtime variables](#runtime-variables)
- [API reference](#api-reference)
- [The `delog` object](#the-delog-object)
- [Data model & conventions](#data-model--conventions)
- [The script library](#the-script-library)
- [Console, errors, and cancellation](#console-errors-and-cancellation)
- [Worked examples](#worked-examples)
- [Bundled example scripts](#bundled-example-scripts)
- [Limitations & gotchas](#limitations--gotchas)

---

## Enabling scripting

Scripting embeds CPython via `pyo3` and is the **`scripting` feature, which is on
by default**. So a normal build already has it:

```bash
cargo run -p delog-app          # scripting included
cargo build --workspace         # scripting included
```

Because it's on by default, the default build needs a Python 3 toolchain
(interpreter + dev headers). To build **without** Python, disable the default
feature (Cargo features are additive, so the opt-out is `--no-default-features`,
not a `without-scripting` feature):

```bash
cargo run   -p delog-app --no-default-features   # no scripting, no Python needed
cargo build -p delog-app --no-default-features
cargo build --workspace  --no-default-features
```

If the build links the wrong `libpython` (e.g. `numpy` fails to import with
`No module named 'math'`), pin the interpreter with a **local, gitignored**
`.cargo/config.toml` that sets `PYO3_PYTHON` to your interpreter and adds its
libdir to the rpath.

### Bundled distribution (no system Python required)

Release downloads include a **bundled** variant — a Windows installer
(`...-bundled-setup.exe`) and a Linux `AppImage` — that ship a private Python
interpreter and NumPy *inside* the application. Install/run it and scripting
works with **no system Python**.

Under the hood the bundled binary is built with the `bundled-python` cargo
feature:

```bash
cargo build --release -p delog-app --no-default-features --features bundled-python
```

At startup it pins `PYTHONHOME` to a `python/` directory beside the executable,
so scripts import the bundled stdlib and NumPy and never touch a system Python.
If that directory is missing the app exits with a clear error (the install is
incomplete — reinstall).

To verify a bundled install:

```bash
delog --check-scripting     # prints the bundled python + package versions, exits 0
```

---

## The Scripts UI

Scripts are a top-level menu. The interactive prompt lives in the
**View ▸ Scripting (F9)** dock.

### Scripts ▸ Run

A submenu listing every saved script in your [library](#the-script-library).
Click a script name to run it immediately.

Running from here works even with the editor and Scripting Console closed - the derived
source shows up in the data browser, and any `print` output is buffered for the
next time you open the Scripting Console.

### Scripts ▸ Editor

Opens the floating script editor window:

- **Script drawer** (left) - create a new script with **+ New**, click a saved
  script to load it, or use a row's `...` menu to edit, duplicate, or remove it.
- **Code editor** (center) - write a full script here. Syntax-highlighted,
  25 rows by default.
- **Toolbar** (above the editor): a **name** field, a **Save** button (writes
  the editor buffer to the library under that name), and a single **Run/Cancel**
  toggle (▶ runs the editor buffer; while a script is running it becomes ⏹ and
  interrupts it).

### View ▸ Scripting (F9)

Opens the bottom Scripting Console dock. Type one line, press <kbd>Enter</kbd>
to evaluate it in a persistent interpreter session. The scrollback shows
results, `print` output, and errors.

The REPL and the editor share **one persistent interpreter**, so names you
define in the REPL are visible to subsequent REPL lines (and vice-versa) for the
life of the app session.

Press <kbd>Tab</kbd> to autocomplete the token at the cursor. Completions are
drawn live from the interpreter session, so they cover Python keywords and
builtins, names you have defined, imported modules, attribute chains on live
objects (`f.` after `f = delog.topic("IMU").field("AccX").read()`), and the whole `delog` API. A single
match is inserted directly; several open a dropdown - <kbd>Up</kbd>/<kbd>Down</kbd>
(or <kbd>Ctrl</kbd>+<kbd>P</kbd>/<kbd>Ctrl</kbd>+<kbd>N</kbd>, or repeated
<kbd>Tab</kbd>) move the highlight, <kbd>Enter</kbd> accepts, and <kbd>Esc</kbd>
dismisses. <kbd>Ctrl</kbd>+<kbd>N</kbd> also opens the dropdown when it is closed,
like <kbd>Tab</kbd>. Because completion introspects real objects, Tab on an
attribute of a call expression (e.g. `delog.topic("IMU").`) evaluates that call.

With no completion dropdown open, <kbd>Up</kbd>/<kbd>Down</kbd> walk the command
history: <kbd>Up</kbd> recalls older submitted lines, <kbd>Down</kbd> moves back
toward newer ones, and stepping past the newest restores whatever you were
typing. History lasts for the app session.

`Settings ▸ Scripting ▸ Open Scripting Console` controls whether the dock opens
automatically on any output, only on errors, or never.

> Running the editor buffer without a name runs it as **`scratch`** (so output
> still shows); **Save** requires a name.

---

## Custom file parsers

Beyond derived-field scripts, DéLOG can run **Python file parsers** (under the
top-level **Parsers** menu) for formats its built-in parsers don't handle. A parser
defines a single `Parse(raw_data)` function that turns raw bytes into topics and
fields. It uses the same embedded-Python environment and serialized worker as
scripts.

Custom parsers have their own dedicated guide:
**[docs/custom_parsers.md](custom_parsers.md)**.

---

## How scripts produce data

A script reads existing fields, computes new arrays, and **emits** them. Emitted
fields are grouped into one or more *output topics* and published as a single
derived source named **`script:<name>`** (where `<name>` is the script/run
name). That source flows through DéLOG's normal ingestion path, so derived
fields automatically get chunking, statistics, caching, GPU plotting, and layout
persistence - no special handling.

Key behaviors:

- **Re-running replaces.** Running `script:foo` again removes the previous
  `script:foo` source and republishes a fresh one - no duplicates.
- **Live data is snapshotted at run time.** A script sees the data that exists
  the moment it runs.
- **All-or-nothing.** If the script raises, **nothing** is emitted (no partial
  source); the traceback goes to the console.
- **Add-only.** Scripts never mutate the original log - they only add derived
  sources alongside it.

---

## Declarative operations

For common conversions, splits, and joins, declare the operation instead of
writing separate snapshot and live callbacks. These are the exact signatures:

```python
delog.transform(topic, *, multiplier=1.0, offset=0.0, fields=None,
                unit=None, units=None, output_topic=None, source=None,
                instance=None, mode="both")
delog.split_by(topic, field, *, fields=None, output_topic=None, source=None,
               instance=None, mode="both")
delog.merge(topics, *, base_topic, output_topic, source=None, mode="both")
```

`mode` is `"snapshot"`, `"live"`, or `"both"` (the default). Snapshot mode
processes the data visible when the script runs. Live mode processes only
future live batches. Both mode backfills the current snapshot and then
continues on the same appendable `script:<name>` source. Per-input watermarks
discard a mirrored or late batch row whose timestamp is at or before the last
snapshot row, so the backfill is not duplicated. Re-running still replaces the
previous generation.

Declarative calls entered in the scripting console follow the same snapshot
and live behavior and publish under `script:console`. A later declarative
console call replaces the previous console generation; ordinary console input
does not remove it.

`source=` is an exact source-label filter, using the labels shown by
`delog.catalog()` (for example, `"flight"` or a live connection's label).
It applies to both the snapshot and future batches, including the first live
batch before its raw source is visible in the browser. `instance=` disambiguates
topic instances for `transform` and `split_by`. Without those selectors, an
ambiguous snapshot topic is an error. Generated topics from all declarations in
one run share the run's `script:<name>` source; declarations do not consume
their own derived output as live input. Each generated topic name has exactly
one owning declaration per run. Static collisions fail before the source is
opened; dynamic `split_by` names are claimed atomically on first output. The
owner must keep the exact field order, names, types, and units thereafter.

### Transform

`transform` copies the input topic and applies this arithmetic, in this order,
to every selected numeric field:

```text
output = input * multiplier + offset
```

With `fields=None`, all numeric fields are selected. An explicit `fields=[...]`
limits the arithmetic to those names; every other field still passes through.
Utf8 fields also pass through unchanged as Utf8 values, including when selected
(they are not converted to numeric data). Numeric output is Float64. Existing
units are retained unless `unit="..."` overrides every transformed numeric
field or `units={"field": "..."}` overrides named fields; `unit` and `units`
cannot be combined. The output topic defaults to the input topic name.

```python
delog.transform("NAV_CONTROLLER_OUTPUT", multiplier=0.017453292519943295,
                fields=["nav_roll", "nav_pitch", "nav_bearing"],
                unit="rad", output_topic="NAV_CONTROLLER_OUTPUT_RAD")
```

The multiplier and offset must be finite, `fields` cannot be empty, requested
fields must exist when the input schema is available, and an explicitly empty
`output_topic` is rejected.

### Split by a field

`split_by` emits one topic per distinct key and removes the split field from
the emitted fields. With `fields=None`, it copies every other field; an explicit
`fields=[...]` copies only those fields (and still omits the key). Utf8 keys must
be non-empty. Numeric keys must be finite; integral values are formatted without
a decimal suffix. Rows with `""`, NaN, or infinite keys are skipped.

The default output template is `"{topic}/{value}"`. A custom `output_topic`
must contain `{value}`; `{topic}` is optional. Both placeholders are replaced
for each key. Units and the string-versus-numeric kind are preserved; numeric
fields are normalized to Arrow Float64, while string fields remain Arrow Utf8.

```python
delog.split_by("PARAM_VALUE", "param_id")
```

For example, keys `SYS_ID` and `RATE` produce `PARAM_VALUE/SYS_ID` and
`PARAM_VALUE/RATE`. A missing key or selected field is an error once a matching
schema is available.

### Merge topics

`topics` is an insertion-ordered mapping from topic names to non-empty lists of
fields. `base_topic` must be one of its keys and supplies the output timeline.
Every other input is aligned by previous-sample hold: at each base timestamp,
the merge uses that field's latest sample at or before the base time. Before a
secondary field's first sample, numeric values are NaN and Utf8 values are
`""`. Units and the string-versus-numeric kind are preserved; numeric fields
are normalized to Arrow Float64, while string fields remain Arrow Utf8.

```python
delog.merge({"NAV_CONTROLLER_OUTPUT": ["nav_roll"], "GPS": ["alt"]},
            base_topic="NAV_CONTROLLER_OUTPUT", output_topic="NAV_WITH_ALT")
```

Output fields retain their requested names when unique. If a name occurs in
more than one input, every occurrence is prefixed with its configured topic
name and `_` (for example, `GPS_alt`); a remaining duplicate after prefixing is
an error. Snapshot inputs must resolve within the same raw source. Live merge
state is also isolated per raw source, and secondary batches update held
values. Ordinarily output is emitted when a base-topic batch arrives.

In default `both` mode, the last typed value of every snapshot secondary field
seeds the live previous-sample history, preserving continuity across the
snapshot/live boundary. In live-only mode, base batches that arrive before all
selected secondary fields have supplied type and unit metadata are retained
without counting as operation errors. When the final secondary schema arrives,
the buffered base rows are emitted in timestamp order (arrival order breaks
ties), with typed NaN or empty-string gaps before each secondary's first
sample. This pending storage is isolated per raw source and grows with early
base traffic until all selected secondary schemas arrive. A secondary sample
that arrives late can be sorted into the history and affect a future base row,
but already emitted base rows are never rewritten.

### Errors and stable live output

Declaration errors (invalid modes, empty selections, bad templates, invalid
merge shapes, or conflicting unit options) fail the script without registering
a partial operation. Snapshot preparation is all-or-nothing: a missing topic in
`mode="snapshot"`, ambiguity, missing field, incompatible source selection, or
unsupported schema prevents the derived source from opening. In `"live"` and
`"both"`, a topic absent at run time may appear later.

Once a live output topic is emitted, its field order, names, data types, and
units are pinned. A later batch that would change that schema is an error. As
with callback transforms, three consecutive live processing errors disable only
the failing operation and report it in the Scripting Console.

---

## Live transforms

The manual read/emit APIs describe **snapshot scripts**: they run once against
the data that exists the moment you run them. Declarative operations can also
continue live. For custom live processing, a **live transform** registers a
callback that runs for *future* live batches, continuously appending derived
fields while telemetry arrives.

Register one with the `@delog.live_transform(...)` decorator:

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

- **`topic`** - the incoming topic to transform.
- **`fields`** - the fields of that topic the callback needs. Each is exposed
  on the `batch` object as a numpy array: numeric fields as `float64` (e.g.
  `batch.nav_roll`), Utf8/string fields as a numpy unicode array (e.g.
  `batch.name`), with null string cells read as `""` - the string analogue of
  the NaN gap convention. A batch whose topic doesn't carry every requested
  field is skipped.
- **`output_topic`** *(optional)* - the derived topic name the returned fields
  are published under, on an appendable `script:<name>` source. Omitting it
  selects **dynamic mode**, where the callback picks its own output topic(s)
  per batch - see [Dynamic output topics](#dynamic-output-topics) below.

**The `batch` object** has `batch.t` (the incoming `int64` microsecond
timestamps) and one numpy array attribute per requested field (`float64` for
numeric fields, a unicode array for string fields).

**The return value** in static mode (`output_topic` set) is a `dict` mapping
each output field name to one of:

- `values` - a length-N array; unit unset, timestamps reuse the input batch's.
- `(values, unit)` - same, with an explicit unit string.
- `(times, values, unit)` - with explicit timestamps. For same-topic transforms
  these **must equal** the input batch's timestamps (a mismatch is an error).

All output arrays must have the same length as the incoming batch.

### Dynamic output topics

Omit `output_topic` to put the callback in **dynamic mode**. Instead of a flat
`{field: values}` dict, it returns:

```
{topic: {field: values | (values, unit) | (times, values, unit)}}
```

One nested `{field: ...}` dict per derived topic, with topic names chosen at
run time (typically one topic per distinct value found in the batch):

```python
import numpy as np

@delog.live_transform(topic="NAMED_VALUE_FLOAT", fields=["name", "value"])
def split_named_floats(batch):
    out = {}
    for name in np.unique(batch.name):
        mask = batch.name == name
        out[f"NAMED_VALUE_FLOAT/{name}"] = {"value": (batch.t[mask], batch.value[mask], None)}
    return out
```

Rules:

- Each field entry uses the same three forms as static mode (`values`,
  `(values, unit)`, `(times, values, unit)`), and every field within one
  topic must resolve to **identical times**.
- The bare/2-tuple forms reuse the **input batch's** times, so their array
  length must equal the batch - they don't implicitly follow a mask. To emit a
  row subset (as in the example above), use the 3-tuple form with explicit
  `times`.
- Explicit `times` (the 3-tuple form) must be an `int64` array, **sorted
  ascending** (equal adjacent values are allowed - e.g. several fields sharing
  a timestamp), typically a mask-subset of `batch.t`.
- An **empty outer dict** (`{}`) means "emit nothing for this batch" - not an
  error.
- A topic whose fields resolve to **zero rows** is silently skipped.
- Each inner `{field: ...}` dict must be **non-empty** - a topic with no
  fields is an error.
- Topic names (the outer dict's keys) must be non-empty strings.
- New topic names appear in the browser the first time they're emitted; keep a
  topic's field set and units consistent across batches so it reads as one
  continuous derived signal.

Key behaviors:

- **Re-running replaces.** Re-running a live script removes the previous
  generation's `script:<name>` source and registers the new callbacks against a
  fresh one - no duplicate live sources.
- **Non-blocking and lossless staging.** Live batches are mirrored to the
  script engine through an unbounded MPSC staging queue. Sending does not wait
  for script execution or registration, so batches captured at that boundary
  are retained instead of being dropped. Sustained input faster than script
  processing can consume memory until the worker catches up.
- **Self-disabling on errors.** If a callback raises on three consecutive
  batches, that transform is disabled and an error is reported to the console;
  other transforms keep running. Error messages identify the transform as
  `<script>.<function>` (e.g. `named_values_live_split.split_named_floats`).

**`@delog.live_transform` callbacks receive one matching topic batch per
invocation.** Registered Python callables persist, so module or closure mutable
state can retain rolling-window history between batches. An invocation cannot
directly read, join, or align another topic input, however. Use declarative
`delog.merge(...)` for cross-topic previous-sample joins; for other custom
cross-topic logic, use a snapshot script after capture.

---

## Runtime variables

Scripts can declare **runtime-tweakable variables** that appear in a dedicated
**Scripts ▸ Variables** window. This is useful for live transforms (and
snapshot scripts) where a single numeric or boolean parameter changes the
behavior - you edit it in the UI and the script responds immediately without
re-running.

### Declaration

Declare variables at the top level of a script (outside any function or decorator
scope) using one of four methods:

```python
delog.slider("alpha", 0.5, min=0.0, max=1.0, step=0.01,
             label="Smoothing factor")
delog.checkbox("enabled", True, label="Enable processing")
delog.combo("mode", ["fast", "accurate", "debug"], default="fast",
            label="Algorithm mode")
delog.text("suffix", "_processed", label="Output field suffix")
```

- **`delog.slider(name, default, *, min, max, step=None, label=None)` → float or int**
  - If `default` is a Python `int`, it is an integer slider and returns an `int`; otherwise it returns `float`. (`min`/`max` are always read as floats.)
  - `step` is optional. With `step=None`, an integer slider steps by `1`; a float slider is continuous (no fixed step).
  - `label` is optional; defaults to `name` if unset.

- **`delog.checkbox(name, default, *, label=None)` → bool**
  - Returns the checked state.

- **`delog.combo(name, options, *, default=None, label=None)` → str**
  - Returns the selected option string.
  - `options` is a list of strings.
  - `default` must be in `options`; defaults to `options[0]` if unset.

- **`delog.text(name, default, *, label=None)` → str**
  - Returns the text field value.

Each declaration **returns its current value** immediately; that value is frozen at
registration time. To read the *live-updated* value inside a callback (e.g. a live
transform), use `delog.param(name)`.

### Reading live values in callbacks

Inside a **live transform callback**, the top-level variable declaration is
executed only once (at registration). To pick up slider/checkbox/combo/text
edits **without re-running** the entire script, call `delog.param(name)`:

```python
delog.slider("alpha", 0.2, min=0.01, max=1.0, step=0.01)

@delog.live_transform(topic="IMU", fields=["AccX"], output_topic="IMU_LPF")
def lowpass(batch):
    alpha = delog.param("alpha")  # read the current value each batch
    x = batch.AccX
    # ... filter using alpha ...
    return {"AccX_lpf": (result, "m/s^2")}
```

Moving the slider updates `delog.param("alpha")` for the next batch, with no
re-run of the script.

### Behavior: live vs. snapshot

- **Live transforms**: edits apply to the **next batch**, with **no re-run** of the
  script (only the callback re-executes with the new parameter values).
- **Snapshot scripts**: edits trigger an **automatic re-run** of the script if it
  is a named library script (one in **Scripts ▸ Run**). Scratch scripts
  do not auto-rerun.
- **Persistence**: all variable values are stored per-script in `script_params.json`
  in the DéLOG config directory, and restored when you load or run the script again.

### The Scripts ▸ Variables window

The **Scripts ▸ Variables** panel displays
all variables declared by the currently running script (or the most recently run
script if nothing is active). Edits take effect immediately:

- Slider release, checkbox toggle, combo selection, or text Enter all apply the
  new value.
- For live transforms, the next batch sees the new value via `delog.param(...)`.
- For snapshot scripts, auto-rerun is triggered (if the script is a named library
  script).

---

## API reference

A single global object, **`delog`**, is injected into every script and REPL
session. You never import or construct it.

| Call | Returns | Purpose |
| --- | --- | --- |
| `delog.slider(name, default, *, min, max, step=None, label=None)` | `float` or `int` | Declare a slider variable; returns its current value. |
| `delog.checkbox(name, default, *, label=None)` | `bool` | Declare a checkbox variable; returns its current value. |
| `delog.combo(name, options, *, default=None, label=None)` | `str` | Declare a combo-box variable; returns its current value. |
| `delog.text(name, default, *, label=None)` | `str` | Declare a text-field variable; returns its current value. |
| `delog.param(name)` | `float`/`int`/`bool`/`str` | Read the current value of a variable inside a live callback. |
| `delog.add_marker(time_us, label, *, color=None, note=None)` | `None` | Add a runtime marker after the current script or callback succeeds. |
| `delog.transform(topic, *, multiplier=1.0, offset=0.0, fields=None, unit=None, units=None, output_topic=None, source=None, instance=None, mode="both")` | `None` | Scale/offset selected numeric fields and pass through the topic. |
| `delog.split_by(topic, field, *, fields=None, output_topic=None, source=None, instance=None, mode="both")` | `None` | Split a topic into stable per-key output topics. |
| `delog.merge(topics, *, base_topic, output_topic, source=None, mode="both")` | `None` | Previous-sample align selected fields onto a base topic. |
| `delog.live_transform(*, topic, fields, output_topic=None)` | decorator | Register custom processing for future batches. |
| `delog.catalog()` | `Catalog` | Structured source/topic/field catalogue. |
| `delog.topic(name, *, source=None, instance=None)` | `TopicRef` | Find one topic by name, source, and optional instance. |
| `delog.find(topic, field=None, *, source=None, instance=None)` | `TopicRef`/`FieldRef` | Find one topic or field, raising on ambiguity. |
| `delog.find_all(topic=None, field=None, *, source=None, instance=None)` | `list` | Return all matching topic or field refs. |
| `FieldRef.read()` | `DelogField` | Read a referenced field. |
| `TopicRef.read(*fields)` | `DelogTable` | Read several fields from one topic on a shared timeline. |
| `DelogField.align(base, mode="prev")` | `np.ndarray[float64]` | Align this field onto another field, table, or timeline. |
| `delog.emit(name, times_us, fields)` | `None` | Emit one derived topic from a dict of field entries. |

### Structured snapshot lookup and reads

Snapshot scripts discover data through structured references rather than
hardcoded source paths:

```python
imu = delog.topic("IMU", instance=0)
accx = imu.field("AccX").read()
```

`delog.topic(...)` returns one `TopicRef` or raises if the match is missing or
ambiguous. Use `source=` when several logs or derived sources contain the same
topic, and `instance=` for topics named like `IMU[0]`.

`delog.find(...)` provides the same unambiguous lookup when the topic or field
name is supplied dynamically. `delog.catalog()` exposes the complete structured
source/topic/field hierarchy, and `delog.find_all(...)` returns every matching
reference when more than one result is expected.

`FieldRef.read()` materializes a `DelogField` with these attributes:

| Attribute | Type | Meaning |
| --- | --- | --- |
| `.t` | `np.ndarray[int64]` | Timestamps, microseconds (raw log time). |
| `.v` | `np.ndarray[float64]` | Numeric values as `float64`; all-`NaN` for string fields. |
| `.s` | `np.ndarray[str]` or `None` | String values for Utf8 fields; `None` for numeric fields. |

Numeric reads preserve NaN gaps. Utf8 reads use `""` for null cells. A missing
reference raises `KeyError`, and reading a field with no data raises
`ValueError`.

`TopicRef.read(*fields)` reads several fields from the same topic into a
`DelogTable`:

```python
imu = delog.topic("IMU").read("AccX", "AccY", "AccZ")
print(imu.t[:3], imu.AccX[:3], imu["AccY"][:3])
```

Calling `read()` with no field names reads every field in the topic. Numeric
columns are `float64`; string columns are numpy unicode arrays.

Use `TopicRef.fields()` for topic-local field discovery:

```python
for field in delog.topic("IMU").fields():
    print(field.path, field.unit)
```

Use `delog.find_all(...)` when the result may span several sources or topics:

```python
for field in delog.find_all(field="AccX"):
    print(field.path)
```

### `DelogField.align(base, mode="prev") -> np.ndarray[float64]`

Aligns a numeric `DelogField` onto another timeline. `base` may be a
`DelogField`, a `DelogTable`, or a one-dimensional `int64` numpy array of
microsecond timestamps. Base timestamps do not need to be sorted: the returned
`float64` array preserves their original order.

```python
gps = delog.topic("GPS", instance=0).field("Alt").read()
baro = delog.topic("BARO", instance=0).field("Alt").read()
gps_prev = gps.align(baro)                    # default: mode="prev"
gps_near = gps.align(baro, mode="nearest")
gps_interp = gps.align(baro, mode="linear")
```

The modes have these exact endpoint rules:

- `"prev"` selects the latest source sample at or before each base timestamp.
  Before the first source sample it returns `NaN`; at and after the last sample
  it holds the last value.
- `"nearest"` selects the source sample with the smallest timestamp distance.
  An equal-distance tie selects the earlier timestamp. Outside the source range
  it selects the nearest endpoint.
- `"linear"` returns exact samples at matching timestamps and otherwise
  interpolates between the distinct samples immediately before and after the
  base timestamp. It returns `NaN` outside the source range and never
  extrapolates.

If a source contains duplicate timestamps, every mode uses the last sample at
that timestamp. Source `NaN` values propagate through selection or interpolation.
Alignment is numeric-only; a string-backed field's numeric lane is all `NaN`.
An unsupported mode raises `ValueError`.

Use `align` whenever a calculation combines fields from different topics with
independent timelines. Passing a `DelogTable` uses its shared `.t`; passing a
numpy timeline uses that array directly.

### `delog.add_marker(time_us, label, *, color=None, note=None) -> None`

Stages a labeled marker at an `int64` microsecond timestamp. The marker becomes
visible only after the enclosing script, console evaluation, or live callback
has completely succeeded:

```python
delog.add_marker(event_t_us, "motor armed")
delog.add_marker(landing_t_us, "landing", color="#2ECC71", note="detected")
```

`label` must be a non-empty string. `color` accepts `None`, `"#RRGGBB"`, or
`"#RRGGBBAA"`; the six-digit form is opaque and the final byte of the
eight-digit form is alpha. With `None`, DéLOG chooses a deterministic color
from its automatic palette based on marker call order. `note` is an optional
string. Duplicate timestamps are retained as distinct markers. There is no
bulk `add_markers` API.

Markers from a named run are owned by that script. A successful rerun atomically
replaces its previous marker set, including clearing it when the rerun adds no
markers. If execution or snapshot/output preparation fails, newly staged
markers are discarded and the previous successful set remains visible. A live
callback likewise appends its markers only after both the callback and its
output validation succeed; an exception, invalid result, or schema error rolls
back that invocation's markers. Replaced callbacks cannot append stale markers.
Unregistering the live script removes all markers owned by that script without
touching manual markers or markers owned by other scripts.

Successful console evaluations append their staged markers to the existing
console marker history rather than replacing it. Every successful evaluation
advances the console generation, even when it adds no marker; this preserves
the history while preventing callbacks from older console generations from
appending stale markers. A failed evaluation does not advance the generation
and discards only the markers staged by that evaluation.

Script markers are runtime-only UI state. They are not written to layouts or
sessions and do not persist after the application runtime ends.

### `delog.emit(name, times_us, fields) -> None`

Emits one derived topic in a single call:

```python
delog.emit("imu_derived", imu.t, {
    "Acc_norm": (norm, "m/s^2"),
    "AccX_smooth": smooth_x,
})
```

Each field entry is either `values` or `(values, unit)`. Values must be a
1-D numeric, `float64`-compatible numpy array with the same length as
`times_us`. You may call `emit` more than once to publish several topics under
the same `script:<name>` source. Output buffers until the script finishes
successfully, so snapshot output remains all-or-nothing.

---

## The `delog` object

Putting the calls together, the canonical shape of a derived-field script is:

```python
import numpy as np

# 1. read inputs through a structured reference
f = delog.topic("IMU", instance=0).field("AccX").read()

# 2. compute with numpy
smoothed = np.convolve(f.v, np.ones(5) / 5, mode="same")

# 3. emit numeric output on a chosen timeline
delog.emit("imu_derived", f.t, {
    "AccX_smooth": (smoothed, "m/s^2"),
})

print(f"emitted {len(f.t)} samples")
```

---

## Data model & conventions

- **Time is `int64` microseconds**, end to end. Field `.t` and `emit(...)`
  `times_us` are both raw log-time microseconds.
- **Numeric values are `float64`.** Numeric reads use `.v`; manual snapshot
  fields emitted by `delog.emit` and live-transform callback result values are
  numeric Float64. Structured reads and live-transform batch inputs can expose
  Utf8 strings, and declarative operations preserve pass-through Utf8 fields.
- **NaN means "gap"** - it is never interpolated away. Reads preserve NaN;
  emit preserves NaN; plots render NaN as a line break. Propagate it naturally
  (most numpy ops do).
- **One timeline per output topic.** Every field in one `delog.emit(..., t, ...)`
  call shares `t`. To combine signals with different rates, pick a base
  timeline and align the others with `DelogField.align`.
- **Output source naming.** A run named `foo` publishes a source `script:foo`;
  its topics and fields are whatever you created via `emit` or declarative operations.

---

## The script library

Saved scripts are plain `.py` files in DéLOG's config directory:

| Platform | Location |
| --- | --- |
| Linux | `~/.local/share/delog/scripts/` |
| macOS | `~/Library/Application Support/DeLOG/scripts/` |
| Windows | `%APPDATA%\DeLOG\data\scripts\` |

- The file **stem** is the script name shown in **Scripts ▸ Run**.
- Files are editable with any external editor; new/changed files appear in the
  menu without restarting (the list is read fresh each time the menu opens).
- In-app: **Save** writes the editor buffer; the editor drawer loads, duplicates,
  and removes saved scripts. To rename, edit the name field and save.
- Scripts are a **global library** - reusable across any loaded log. Write them
  to look up fields by name (see the [examples](#worked-examples)) so the same
  script works on any flight.

---

## Console, errors, and cancellation

- **`print(...)`** and anything written to `stdout`/`stderr` is captured to the
  Scripting Console scrollback.
- **Errors** print the Python traceback to the console; the run emits no source.
- **Cancel**: while a script runs, the toolbar toggle shows ⏹ - click it to
  raise `KeyboardInterrupt` in the script (like Ctrl-C). This is cooperative:
  it fires at the next Python bytecode boundary, so a script stuck inside a
  single long C call (e.g. one huge numpy op) can't be interrupted mid-call.
- **Clear**: the Scripting Console dock's **Clear** button clears the scrollback.

---

## Worked examples

### 1. Vector magnitude (single topic)

All three accel axes live in one topic, so they share a timeline - no resampling.

```python
import numpy as np

imu = delog.topic("IMU", instance=0).read("AccX", "AccY", "AccZ")
mag = np.sqrt(imu.AccX**2 + imu.AccY**2 + imu.AccZ**2)

delog.emit("accel_mag", imu.t, {"mag": (mag, "m/s^2")})
```

### 2. Quaternion → Euler angles (log-agnostic lookup)

Uses topic name and instance rather than a source path, so the same library
script runs on any unambiguous PX4 log. (PX4 stores `q = [w, x, y, z]`.)

```python
import numpy as np

att = delog.topic("vehicle_attitude", instance=0).read(
    "q[0]", "q[1]", "q[2]", "q[3]"
)
w = att["q[0]"]
x = att["q[1]"]
y = att["q[2]"]
z = att["q[3]"]

n = np.sqrt(w*w + x*x + y*y + z*z)
n[n == 0] = 1.0
w, x, y, z = w/n, x/n, y/n, z/n

roll  = np.arctan2(2*(w*x + y*z), 1 - 2*(x*x + y*y))
pitch = np.arcsin(np.clip(2*(w*y - z*x), -1.0, 1.0))
yaw   = np.arctan2(2*(w*z + x*y), 1 - 2*(y*y + z*z))

delog.emit("vehicle_attitude_euler", att.t, {
    "roll": (np.degrees(roll), "deg"),
    "pitch": (np.degrees(pitch), "deg"),
    "yaw": (np.degrees(yaw), "deg"),
})
```

### 3. Combining two topics (alignment)

```python
baro = delog.topic("BARO", instance=0).field("Alt").read()
gps = delog.topic("GPS", instance=0).field("Alt").read()
gps_on_baro = gps.align(baro)

delog.emit("alt_compare", baro.t, {
    "baro": (baro.v, "m"),
    "gps": (gps_on_baro, "m"),
    "diff": (baro.v - gps_on_baro, "m"),
})
```

### 4. Explore in the REPL

```python
>>> [field.path for field in delog.find_all(field="AccX")]
>>> f = delog.topic("IMU", instance=0).field("AccX").read()
>>> import numpy as np
>>> float(np.nanmax(f.v))
9.81
```

---

## Bundled example scripts

The [`scripts/`](../scripts/) directory ships runnable examples you can copy into
your [script library](#the-script-library) or open in the Console. Snapshot
examples run once against current data; live examples register work for future
batches. The declarative scripts set `mode="snapshot"` or `mode="live"`
explicitly.

| Script | Kind | What it does |
| --- | --- | --- |
| [`snapshot/vehicle_attitude_euler.py`](../scripts/snapshot/vehicle_attitude_euler.py) | snapshot-only | Converts a PX4 `vehicle_attitude[0]` quaternion to roll/pitch/yaw with structured reads and `delog.emit(...)`. |
| [`snapshot/nav_controller_output_radians.py`](../scripts/snapshot/nav_controller_output_radians.py) | snapshot-only | Converts ArduPilot `NAV_CONTROLLER_OUTPUT` angle fields to radians with a declarative transform in `mode="snapshot"`. |
| [`live/nav_controller_live_rad.py`](../scripts/live/nav_controller_live_rad.py) | live-only | Converts future `NAV_CONTROLLER_OUTPUT` angle fields to radians with a declarative transform in `mode="live"`. |
| [`live/named_values_live_split.py`](../scripts/live/named_values_live_split.py) | live-only | Splits future `NAMED_VALUE_FLOAT` and `NAMED_VALUE_INT` rows into one topic per `name` with a declarative split. |
| [`live/param_value_live_split.py`](../scripts/live/param_value_live_split.py) | live-only | Splits future `PARAM_VALUE` rows into one topic per `param_id` with a declarative split. |
| [`live/tunable_lowpass.py`](../scripts/live/tunable_lowpass.py) | live-only callback | Applies a slider-controlled exponential low-pass filter to future IMU batches. |

`tunable_lowpass.py` remains the callback escape hatch for algorithms that need
custom per-batch state or math.

---

## Limitations & gotchas

- **Not sandboxed.** Embedded CPython runs with your full user privileges
  (filesystem, network). Only run scripts you trust. This is a deliberate
  trade-off for the power of real CPython + numpy.
- **Manual snapshot and callback string output is unsupported.** `DelogField.s`
  and live-transform string batch attributes let you *read* Utf8 fields, but
  manual snapshot output through `emit` and callback return values are numeric-only.
  Declarative operations are the exception: they preserve pass-through Utf8
  fields and missing strings as `""`. Materialized `DelogField` reads do not
  carry `.unit` or `.dtype`; use a `FieldRef` when you need unit metadata.
- **Numeric output is `float64`.** Even if a source field was integer/bool,
  derived numeric output columns are stored as `Float64`.
- **Length must match.** Every `emit` value must match the length of its
  `times_us`. Combine differently sampled inputs with `DelogField.align`.
- **Print-only scripts do not emit a source.** A run that neither calls `emit`
  nor declares an operation only writes console output.
- **Cancellation is cooperative** (see above) - long single C calls can't be
  interrupted mid-call.
- **Long loops block that script.** Each run/eval executes on the interpreter
  thread; the UI stays responsive, but a `while True:` will run until cancelled.
