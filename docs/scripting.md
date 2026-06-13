# DeLOG Python Scripting

DeLOG can run Python scripts that read the loaded dataset and **produce new
fields and topics** — derived signals that appear in the data browser and plot
exactly like parsed log data. Scripts get the full embedded CPython interpreter
(including `numpy`), so derived-field math is just numpy.

This is an **optional, build-time feature**. It is off by default.

- [Enabling scripting](#enabling-scripting)
- [The Scripts UI](#the-scripts-ui)
- [How scripts produce data](#how-scripts-produce-data)
- [API reference](#api-reference)
- [The `delog` object](#the-delog-object)
- [Data model & conventions](#data-model--conventions)
- [The script library](#the-script-library)
- [Console, errors, and cancellation](#console-errors-and-cancellation)
- [Worked examples](#worked-examples)
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
libdir to the rpath. See `CLAUDE.md` → Commands.

---

## The Scripts UI

Everything lives under the **Tools ▸ Scripts** menu.

### Tools ▸ Scripts ▸ Run

A submenu listing every saved script in your [library](#the-script-library).
Each row has:

| Control | Action |
| --- | --- |
| **script name** | Run the script immediately. |
| ✎ (pencil) | Load the script into the Console editor for editing. |
| 🗑 (trash) | Delete the script (with a confirmation dialog). |

Running from here works even with the Console window closed — the derived
source shows up in the data browser, and any `print` output is buffered for the
next time you open the Console.

### Tools ▸ Scripts ▸ Console

Opens the scripting window:

- **Code editor** (center) — write a full script here. Syntax-highlighted,
  25 rows by default.
- **Toolbar** (above the editor): a **name** field, a **Save** button (writes
  the editor buffer to the library under that name), and a single **Run/Cancel**
  toggle (▶ runs the editor buffer; while a script is running it becomes ⏹ and
  interrupts it).
- **REPL** (bottom) — type one line, press <kbd>Enter</kbd> to evaluate it in a
  persistent interpreter session. The console scrollback shows results,
  `print` output, and errors. The 🗑 at the right of the REPL line clears the
  console.

The REPL and the editor share **one persistent interpreter**, so names you
define in the REPL are visible to subsequent REPL lines (and vice-versa) for the
life of the app session.

> Running the editor buffer without a name runs it as **`scratch`** (so output
> still shows); **Save** requires a name.

---

## How scripts produce data

A script reads existing fields, computes new arrays, and **emits** them. Emitted
fields are grouped into one or more *output topics* and published as a single
derived source named **`script:<name>`** (where `<name>` is the script/run
name). That source flows through DeLOG's normal ingestion path, so derived
fields automatically get chunking, statistics, caching, GPU plotting, and layout
persistence — no special handling.

Key behaviors:

- **Re-running replaces.** Running `script:foo` again removes the previous
  `script:foo` source and republishes a fresh one — no duplicates.
- **Live data is snapshotted at run time.** A script sees the data that exists
  the moment it runs.
- **All-or-nothing.** If the script raises, **nothing** is emitted (no partial
  source); the traceback goes to the console.
- **Add-only.** Scripts never mutate the original log — they only add derived
  sources alongside it.

---

## API reference

A single global object, **`delog`**, is injected into every script and REPL
session. You never import or construct it.

| Call | Returns | Purpose |
| --- | --- | --- |
| `delog.sources()` | `list[str]` | All live field paths, `"source/topic/field"`. |
| `delog.field(path)` | `DelogField` | Read one field as numpy arrays. |
| `delog.resample_prev(field, base_times)` | `np.ndarray[float64]` | Prev-sample align a field onto another timeline. |
| `delog.output(times_us, name)` | `DelogOutput` | Begin a new derived topic. |
| `DelogOutput.add_field(name, values, unit=None)` | `None` | Add a field to that topic. |

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

A `DelogField` has two attributes:

| Attribute | Type | Meaning |
| --- | --- | --- |
| `.t` | `np.ndarray[int64]` | Timestamps, microseconds (raw log time). |
| `.v` | `np.ndarray[float64]` | Values, as `float64`. |

```python
f = delog.field("flight_42/IMU[0]/AccX")
print(f.t[:3], f.v[:3])   # int64 µs, float64 values
```

- Raises `KeyError` if the path doesn't resolve, `ValueError` if the field has
  no data.
- Values are always `float64` (ints/bools are widened). **NaN is preserved** —
  gaps in the source remain NaN, so you can detect and propagate them.
- All fields **within the same topic** share identical timestamps, so you can
  read several of them and operate element-wise without aligning.

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
  values). The emitted columns are stored as `Float64`.
- **NaN means "gap"** — it is never interpolated away. Reads preserve NaN;
  emit preserves NaN; plots render NaN as a line break. Propagate it naturally
  (most numpy ops do).
- **One timeline per output topic.** Every field in a `delog.output(t, ...)`
  topic shares `t`. To combine signals with different rates, pick a base
  timeline and `resample_prev` the others onto it.
- **Output source naming.** A run named `foo` publishes a source `script:foo`;
  its topics/fields are whatever you created via `output`/`add_field`.

---

## The script library

Saved scripts are plain `.py` files in DeLOG's config directory:

| Platform | Location |
| --- | --- |
| Linux | `~/.local/share/delog/scripts/` |
| macOS | `~/Library/Application Support/DeLOG/scripts/` |
| Windows | `%APPDATA%\DeLOG\data\scripts\` |

- The file **stem** is the script name shown in **Tools ▸ Scripts ▸ Run**.
- Files are editable with any external editor; new/changed files appear in the
  menu without restarting (the list is read fresh each time the menu opens).
- In-app: **Save** (Console toolbar) writes the editor buffer; **✎** loads a
  script for editing; **🗑** deletes it (with confirmation).
- Scripts are a **global library** — reusable across any loaded log. Write them
  to look up fields by name (see the [examples](#worked-examples)) so the same
  script works on any flight.

---

## Console, errors, and cancellation

- **`print(...)`** and anything written to `stdout`/`stderr` is captured to the
  Console scrollback.
- **Errors** print the Python traceback to the console; the run emits no source.
- **Cancel**: while a script runs, the toolbar toggle shows ⏹ — click it to
  raise `KeyboardInterrupt` in the script (like Ctrl-C). This is cooperative:
  it fires at the next Python bytecode boundary, so a script stuck inside a
  single long C call (e.g. one huge numpy op) can't be interrupted mid-call.
- **Clear**: the 🗑 at the right of the REPL line clears the console scrollback.

---

## Worked examples

### 1. Vector magnitude (single topic)

All three accel axes live in one topic, so they share a timeline — no resampling.

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

## Limitations & gotchas

- **Not sandboxed.** Embedded CPython runs with your full user privileges
  (filesystem, network). Only run scripts you trust. This is a deliberate
  trade-off for the power of real CPython + numpy.
- **`DelogField` exposes only `.t` and `.v`.** There is no `.unit`/`.dtype`
  attribute on reads (units are an output concern via `add_field(..., unit=)`).
- **Output is `float64`.** Even if a source field was integer/bool, derived
  output columns are stored as `Float64`.
- **Length must match.** `add_field` values must match the length of the
  topic's `times_us`. Combine differently-sampled inputs with `resample_prev`.
- **A `print`-only script still emits an (empty) source.** If you never call
  `delog.output(...)`, running still creates an empty `script:<name>` source.
- **Cancellation is cooperative** (see above) — long single C calls can't be
  interrupted mid-call.
- **Long loops block that script.** Each run/eval executes on the interpreter
  thread; the UI stays responsive, but a `while True:` will run until cancelled.
