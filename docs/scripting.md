# DéLOG Python Scripting

DéLOG can run Python scripts that read the loaded dataset and **produce new
fields and topics** - derived signals that appear in the data browser and plot
exactly like parsed log data. Scripts get the full embedded CPython interpreter
(including `numpy`), so derived-field math is just numpy.

This is an **optional, build-time feature**. It is off by default.

- [Enabling scripting](#enabling-scripting)
- [The Scripts UI](#the-scripts-ui)
- [Custom file parsers](#custom-file-parsers)
- [How scripts produce data](#how-scripts-produce-data)
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
objects (`f.` after `f = delog.field(...)`), and the whole `delog` API. A single
match is inserted directly; several open a dropdown - <kbd>Up</kbd>/<kbd>Down</kbd>
(or repeated <kbd>Tab</kbd>) move the highlight, <kbd>Enter</kbd> accepts, and
<kbd>Esc</kbd> dismisses. Because completion introspects real objects, Tab on an
attribute of a call expression (e.g. `delog.field("x").`) evaluates that call.

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

## Live transforms

Everything above describes **snapshot scripts**: they run once against the data
that exists the moment you run them. A **live transform** instead registers a
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
- **Non-blocking.** Live batches are mirrored to the script engine through a
  bounded queue; if it fills, batches are dropped (a diagnostic is logged)
  rather than stalling live ingestion.
- **Self-disabling on errors.** If a callback raises on three consecutive
  batches, that transform is disabled and an error is reported to the console;
  other transforms keep running. Error messages identify the transform as
  `<script>.<function>` (e.g. `named_values_live_split.split_named_floats`).

**Version 1 live transforms are same-topic only.** The callback sees one
incoming topic batch at a time. It cannot join across topics, resample onto
another timeline, or keep rolling-window state between batches - for that, use a
snapshot script after capture.

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
| `delog.sources()` | `list[str]` | All live field paths, `"source/topic/field"`. |
| `delog.catalog()` | `Catalog` | Structured source/topic/field catalogue. |
| `delog.topic(name, *, source=None, instance=None)` | `TopicRef` | Find one topic by name, source, and optional instance. |
| `delog.find(topic, field=None, *, source=None, instance=None)` | `TopicRef`/`FieldRef` | Find one topic or field, raising on ambiguity. |
| `delog.find_all(topic=None, field=None, *, source=None, instance=None)` | `list` | Return all matching topic or field refs. |
| `delog.field(path)` | `DelogField` | Read one field as numpy arrays. |
| `FieldRef.read()` | `DelogField` | Read a referenced field. |
| `TopicRef.read(*fields)` | `DelogTable` | Read several fields from one topic on a shared timeline. |
| `delog.resample_prev(field, base_times)` | `np.ndarray[float64]` | Prev-sample align a field onto another timeline. |
| `DelogField.align_prev(base)` | `np.ndarray[float64]` | Prev-sample align this field onto another field/table/timeline. |
| `delog.output(times_us, name)` | `DelogOutput` | Begin a new derived topic. |
| `DelogOutput.add_field(name, values, unit=None)` | `None` | Add a field to that topic. |
| `delog.emit(name, times_us, fields)` | `None` | Emit one derived topic from a dict of field entries. |

### `delog.sources() -> list[str]`

Returns every live (non-removed) field as a string path
`"<source>/<topic>/<field>"`. This is the catalogue you pass to
`delog.field(...)`. Example entries:

```
flight_42/IMU[0]/AccX
flight_42/vehicle_attitude[0]/q[0]
```

### `delog.field(path) -> DelogField`

Reads one field, materialized as numpy arrays. `path` is a string exactly as it
appears in `delog.sources()`.

A `DelogField` has three attributes:

| Attribute | Type | Meaning |
| --- | --- | --- |
| `.t` | `np.ndarray[int64]` | Timestamps, microseconds (raw log time). |
| `.v` | `np.ndarray[float64]` | Values, as `float64`. All-`NaN` for string fields. |
| `.s` | `np.ndarray[str]` or `None` | Numpy unicode array of values, for string (Utf8) fields; `None` for numeric fields. |

```python
f = delog.field("flight_42/IMU[0]/AccX")
print(f.t[:3], f.v[:3])   # int64 µs, float64 values
```

- Raises `KeyError` if the path doesn't resolve, `ValueError` if the field has
  no data.
- Numeric fields are always widened to `float64` (ints/bools included). **NaN
  is preserved** - gaps in the source remain NaN, so you can detect and
  propagate them.
- **String fields** (Utf8) additionally populate `.s` with a numpy unicode
  array; null cells read as `""` - the string analogue of the NaN gap
  convention. `.v` is still present but all-`NaN` for these fields.
- All fields **within the same topic** share identical timestamps, so you can
  read several of them and operate element-wise without aligning.

### Structured snapshot lookup

For reusable scripts, prefer structured lookup over hardcoded source paths:

```python
imu = delog.topic("IMU", instance=0)
accx = imu.field("AccX").read()
```

`delog.topic(...)` returns one `TopicRef` or raises if the match is missing or
ambiguous. Use `source=` when several logs or derived sources contain the same
topic, and `instance=` for topics named like `IMU[0]`.

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

### `delog.resample_prev(field, base_times) -> np.ndarray[float64]`

Resamples a `DelogField`'s values onto a different timeline using
**previous-sample hold** (zero-order hold): for each time in `base_times`, take
the field's value at the latest sample at or before that time. Times before the
field's first sample become `NaN`.

```python
gps  = delog.field("flight_42/GPS[0]/Alt")
baro = delog.field("flight_42/BARO[0]/Alt")
# put GPS altitude onto the BARO timeline so they can be compared element-wise:
gps_on_baro = delog.resample_prev(gps, baro.t)
diff = baro.v - gps_on_baro
```

`base_times` is any `int64` numpy array of microsecond timestamps (typically
another field's `.t`). Use this whenever you combine fields from **different
topics** (which have independent timelines).

The method form is equivalent and is usually easier to read:

```python
gps_alt = delog.topic("GPS").field("Alt").read()
baro_alt = delog.topic("BARO").field("Alt").read()
gps_on_baro = gps_alt.align_prev(baro_alt)
```

### `delog.output(times_us, name) -> DelogOutput`

Begins a new derived **topic** called `name`. `times_us` is the `int64`
microsecond timeline shared by **every field** you add to this topic. Returns a
`DelogOutput` builder.

```python
out = delog.output(some_field.t, "my_topic")
```

You can call `delog.output(...)` more than once in a script to emit several
topics; they all ship together under the one `script:<name>` source.

### `DelogOutput.add_field(name, values, unit=None) -> None`

Adds one field to the topic. `values` is a `float64` numpy array that must be
**the same length as the topic's `times_us`** (raises `ValueError` otherwise).
`unit` is an optional display-unit string.

```python
out = delog.output(f.t, "derived")
out.add_field("speed", v_speed, unit="m/s")
out.add_field("accel", v_accel)            # unit optional
```

The fields buffer until the script finishes successfully, then publish as
`script:<name>/<topic>/<field>`.

### `delog.emit(name, times_us, fields) -> None`

Emits one derived topic in a single call:

```python
delog.emit("imu_derived", imu.t, {
    "Acc_norm": (norm, "m/s^2"),
    "AccX_smooth": smooth_x,
})
```

Each field entry is either `values` or `(values, unit)`. Values must be a
1-D `float64`-compatible numpy array with the same length as `times_us`. Like
the builder API, `emit` buffers until the script finishes successfully, so
snapshot output remains all-or-nothing.

---

## The `delog` object

Putting the calls together, the canonical shape of a derived-field script is:

```python
import numpy as np

# 1. read inputs (same topic -> shared timeline; else resample_prev)
f = delog.field("flight_42/IMU[0]/AccX")

# 2. compute with numpy
smoothed = np.convolve(f.v, np.ones(5) / 5, mode="same")

# 3. emit on a chosen timeline
out = delog.output(f.t, "imu_derived")
out.add_field("AccX_smooth", smoothed, unit="m/s^2")

print(f"emitted {len(f.t)} samples")
```

---

## Data model & conventions

- **Time is `int64` microseconds**, end to end. Field `.t` and `output(...)`
  `times_us` are both raw log-time microseconds.
- **Values are `float64`** on the way in (`.v`) and on the way out (`add_field`
  values). The emitted columns are stored as `Float64`. String fields are the
  one exception on the read side: `.s` on `delog.field(...)` and a live
  transform's numpy unicode `batch.<name>` attribute expose Utf8 fields as
  strings, but output stays `float64`-only.
- **NaN means "gap"** - it is never interpolated away. Reads preserve NaN;
  emit preserves NaN; plots render NaN as a line break. Propagate it naturally
  (most numpy ops do).
- **One timeline per output topic.** Every field in a `delog.output(t, ...)`
  topic shares `t`. To combine signals with different rates, pick a base
  timeline and `resample_prev` the others onto it.
- **Output source naming.** A run named `foo` publishes a source `script:foo`;
  its topics/fields are whatever you created via `output`/`add_field`.

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

base = "flight_42/IMU[0]"
x = delog.field(f"{base}/AccX")
y = delog.field(f"{base}/AccY")
z = delog.field(f"{base}/AccZ")

out = delog.output(x.t, "accel_mag")
out.add_field("mag", np.sqrt(x.v**2 + y.v**2 + z.v**2), unit="m/s^2")
```

### 2. Quaternion → Euler angles (log-agnostic lookup)

Finds the source prefix automatically so the same library script runs on any
PX4 log. (PX4 stores `q = [w, x, y, z]`.)

```python
import numpy as np

TOPIC = "vehicle_attitude[0]"
suffix = f"/{TOPIC}/q[0]"
prefix = next((p[: -len(suffix)] for p in delog.sources() if p.endswith(suffix)), None)
if prefix is None:
    raise RuntimeError(f"{TOPIC}/q[0] not found in this log")

qw = delog.field(f"{prefix}/{TOPIC}/q[0]")
w, t = qw.v, qw.t
x = delog.field(f"{prefix}/{TOPIC}/q[1]").v
y = delog.field(f"{prefix}/{TOPIC}/q[2]").v
z = delog.field(f"{prefix}/{TOPIC}/q[3]").v

n = np.sqrt(w*w + x*x + y*y + z*z)
n[n == 0] = 1.0
w, x, y, z = w/n, x/n, y/n, z/n

roll  = np.arctan2(2*(w*x + y*z), 1 - 2*(x*x + y*y))
pitch = np.arcsin(np.clip(2*(w*y - z*x), -1.0, 1.0))
yaw   = np.arctan2(2*(w*z + x*y), 1 - 2*(y*y + z*z))

out = delog.output(t, "vehicle_attitude_euler")
out.add_field("roll",  np.degrees(roll),  unit="deg")
out.add_field("pitch", np.degrees(pitch), unit="deg")
out.add_field("yaw",   np.degrees(yaw),   unit="deg")
```

### 3. Combining two topics (resample)

```python
import numpy as np

baro = delog.field("flight_42/BARO[0]/Alt")          # base timeline
gps  = delog.field("flight_42/GPS[0]/Alt")
gps_on_baro = delog.resample_prev(gps, baro.t)        # align onto BARO times

out = delog.output(baro.t, "alt_compare")
out.add_field("baro", baro.v, unit="m")
out.add_field("gps",  gps_on_baro, unit="m")
out.add_field("diff", baro.v - gps_on_baro, unit="m")
```

### 4. Explore in the REPL

```python
>>> delog.sources()[:5]
['flight_42/IMU[0]/AccX', 'flight_42/IMU[0]/AccY', ...]
>>> f = delog.field("flight_42/IMU[0]/AccX")
>>> import numpy as np
>>> float(np.nanmax(f.v))
9.81
```

---

## Bundled example scripts

The [`scripts/`](../scripts/) directory ships runnable examples you can copy into
your [script library](#the-script-library) or open in the Console. Snapshot
examples have two versions: `snapshot/v1` keeps the original path-string style,
while `snapshot/v2` uses the structured lookup and emit APIs. Live examples
currently live under `live/v1`.

| Script | Kind | What it does |
| --- | --- | --- |
| [`snapshot/v2/vehicle_attitude_euler.py`](../scripts/snapshot/v2/vehicle_attitude_euler.py) | snapshot | Converts a PX4 `vehicle_attitude[0]` quaternion to roll/pitch/yaw using `delog.topic(...).read(...)` and `delog.emit(...)`. |
| [`snapshot/v2/nav_controller_output_radians.py`](../scripts/snapshot/v2/nav_controller_output_radians.py) | snapshot | Re-emits ArduPilot `NAV_CONTROLLER_OUTPUT` angles in radians using structured topic lookup and one-call emit. |
| [`snapshot/v1/vehicle_attitude_euler.py`](../scripts/snapshot/v1/vehicle_attitude_euler.py) | snapshot | Legacy path-string version of the PX4 attitude conversion example. |
| [`snapshot/v1/nav_controller_output_radians.py`](../scripts/snapshot/v1/nav_controller_output_radians.py) | snapshot | Legacy path-string version of the ArduPilot angle conversion example. |
| [`live/v1/nav_controller_live_rad.py`](../scripts/live/v1/nav_controller_live_rad.py) | live transform | The live-streaming counterpart: a `@delog.live_transform` that converts `NAV_CONTROLLER_OUTPUT` angles to radians as batches arrive. |
| [`live/v1/named_values_live_split.py`](../scripts/live/v1/named_values_live_split.py) | live transform | Splits live `NAMED_VALUE_FLOAT`/`NAMED_VALUE_INT` streams into one derived topic per `name` (dynamic output topics), so named values arrive sorted by category. |
| [`live/v1/param_value_live_split.py`](../scripts/live/v1/param_value_live_split.py) | live transform | Splits a live `PARAM_VALUE` stream into one derived topic per `param_id`, so each parameter's value gets its own trace. |
| [`live/v1/tunable_lowpass.py`](../scripts/live/v1/tunable_lowpass.py) | live transform | An exponential low-pass filter with a slider-controlled smoothing factor. Demonstrates runtime-tweakable variables in a live transform: move the slider to change the filter coefficient live, without re-running. |

The snapshot/live pair (`nav_controller_*`) is a good side-by-side reference for
the difference between the two execution modes.

---

## Limitations & gotchas

- **Not sandboxed.** Embedded CPython runs with your full user privileges
  (filesystem, network). Only run scripts you trust. This is a deliberate
  trade-off for the power of real CPython + numpy.
- **String fields are read-only.** `DelogField.s` and live-transform string
  batch attributes let you *read* Utf8 fields, but script **output** stays
  Float64-only - `add_field` and the numeric forms of a live transform's
  return value always take `float64` arrays. Materialized `DelogField` reads do
  not carry `.unit` or `.dtype`; use a `FieldRef` when you need unit metadata.
- **Output is `float64`.** Even if a source field was integer/bool, derived
  output columns are stored as `Float64`.
- **Length must match.** `add_field` values must match the length of the
  topic's `times_us`. Combine differently-sampled inputs with `resample_prev`.
- **Print-only scripts do not emit a source.** A run that never calls
  `delog.output(...)` or `delog.emit(...)` only writes console output.
- **Cancellation is cooperative** (see above) - long single C calls can't be
  interrupted mid-call.
- **Long loops block that script.** Each run/eval executes on the interpreter
  thread; the UI stays responsive, but a `while True:` will run until cancelled.
