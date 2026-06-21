# DéLOG Custom Parsers

DéLOG's built-in parsers read PX4 ULog (`.ulg`), ArduPilot (`.BIN`), and QGroundControl
MAVLink telemetry (`.tlog`). When you have a file format that none of those handle, you can
add a **custom Python parser**: a small `.py` file that turns raw bytes into topics and
fields, which then plot exactly like built-in log data.

Custom parsers use the same embedded-Python environment as [scripting](scripting.md) and are
part of the optional `scripting` feature (on by default).

- [Where parsers live](#where-parsers-live)
- [The Parsers UI](#the-parsers-ui)
- [Writing a parser](#writing-a-parser)
- [The input: a float32 array](#the-input-a-float32-array)
- [The output: field triples](#the-output-field-triples)
- [Topics, fields, and clocks](#topics-fields-and-clocks)
- [Supported dtypes and NaN](#supported-dtypes-and-nan)
- [Worked example](#worked-example)
- [Execution model & safety](#execution-model--safety)
- [Limitations & gotchas](#limitations--gotchas)

---

## Where parsers live

Custom parsers are global `parsers/*.py` files in DéLOG's application config directory -
the same kind of global library as saved [scripts](scripting.md#the-script-library):

| Platform | Location |
| --- | --- |
| Linux | `~/.local/share/delog/parsers/` |
| macOS | `~/Library/Application Support/DeLOG/parsers/` |
| Windows | `%APPDATA%\DeLOG\data\parsers\` |

The file **stem** is the parser name shown in the menu. Files are editable with any external
editor; new or changed files appear without restarting (the list is read fresh each time the
menu opens).

Custom parsers are **never auto-sniffed**. DéLOG only auto-detects the built-in Rust formats;
a custom Python parser is always chosen explicitly, and the Tools menu lists custom Python
parsers only (not the built-in parsers).

---

## The Parsers UI

**Tools ▸ Parsers** manages your Python parsers.

| Control | Action |
| --- | --- |
| **Add new parser...** | Open the editor on a fresh parser. |
| **parser name** | (in the editor) the filename stem; change it and **Save** to add or rename. |
| ✎ (pencil) | Load a saved parser into the editor. |
| 📁 (folder) | Explicitly pick a file and open it with that parser. |
| **Save** | Write the editor buffer to the library under the current name. |
| **Delete** | Remove a saved parser (with a confirmation dialog). |

To use a parser, click its folder icon, choose the file, and DéLOG opens it through that
parser.

---

## Writing a parser

A parser is a `.py` file that defines a single function, `Parse`:

```python
import numpy as np

def Parse(raw_data):
    t = np.arange(raw_data.size, dtype=np.float64) * 0.01
    return [
        ("DATA.rtc",   t,        "time in seconds"),
        ("DATA.value", raw_data, "raw float32 values"),
    ]
```

`Parse(raw_data)` receives the selected file's bytes and returns a list of
`(field_name, values, tooltip)` triples. That's the whole contract.

---

## The input: a float32 array

`raw_data` is the selected file materialized as an **exact, one-dimensional, native-endian
NumPy `float32` array** - equivalent to `numpy.fromfile(path, dtype=numpy.float32)`. An
incomplete trailing 1–3 bytes (a partial final `float32`) is silently ignored.

If your format isn't naturally a stream of `float32` values, reinterpret the array's bytes
inside `Parse`. For example, to read the same bytes as `uint16`:

```python
words = raw_data.view(np.uint16)   # reinterpret the underlying bytes
```

---

## The output: field triples

`Parse` must return an iterable of triples, each **exactly** `(field_name, values, tooltip)`:

- **`field_name`** - a string. The **first dot** splits topic from field: `"gps.main.lat"`
  becomes topic `gps`, field `main.lat`. A name with no dot is its own topic.
- **`values`** - a one-dimensional NumPy-compatible array (see [dtypes](#supported-dtypes-and-nan)).
  All fields **within the same topic** must have equal length.
- **`tooltip`** - a string shown when hovering the field in the data browser.

---

## Topics, fields, and clocks

- Fields are grouped into **topics** by the part of the name before the first dot.
- Each topic uses its **`.rtc`** field as its clock when present, otherwise its **`.index`**
  field. Both clock fields remain visible in the browser.
- Clock values are interpreted as **seconds** and rounded to canonical signed `i64`
  **microseconds** (DéLOG's internal time unit).
- Exact duplicate full field names are accepted for legacy compatibility: the **last value
  wins**, and DéLOG records a warning.

So in the example above, the `DATA` topic is clocked by `DATA.rtc` (seconds → µs), and
`DATA.value` is plotted against it.

---

## Supported dtypes and NaN

`values` arrays may be any of:

- Boolean
- Signed integers: `int8`, `int16`, `int32`, `int64`
- Unsigned integers: `uint8`, `uint16`, `uint32`, `uint64`
- Floating point: `float32`, `float64`

Numeric **NaN is an ordinary gap value** - it is preserved and rendered as a line break in
plots, never interpolated away.

---

## Worked example

A telemetry blob of interleaved `int16` channels at 100 Hz: `[ax, ay, az, ax, ay, az, ...]`.

```python
import numpy as np

CHANNELS = ("ax", "ay", "az")
RATE_HZ = 100.0

def Parse(raw_data):
    # Reinterpret the float32 bytes as int16 and de-interleave into channels.
    samples = raw_data.view(np.int16)
    n_frames = samples.size // len(CHANNELS)
    frame = samples[: n_frames * len(CHANNELS)].reshape(n_frames, len(CHANNELS))

    t = np.arange(n_frames, dtype=np.float64) / RATE_HZ   # seconds

    out = [("imu.rtc", t, "time (s)")]
    for i, name in enumerate(CHANNELS):
        out.append((f"imu.{name}", frame[:, i], f"raw {name} counts"))
    return out
```

This produces an `imu` topic clocked by `imu.rtc`, with fields `ax`, `ay`, `az`.

---

## Execution model & safety

- **Full privileges, must be trusted.** Parser code runs in a fresh namespace with your full
  user privileges (filesystem, network). Only run parsers you trust.
- **Shared Python environment.** Imports use the same embedded interpreter as scripts -
  NumPy always, plus Bottleneck, CFFI, and SciPy when installed there.
- **One job at a time.** Parsers share the single serialized Python worker with scripts, the
  REPL, and live transforms, so only one of these runs at any moment.
- **Transactional.** Opening is all-or-nothing: an error or cancellation publishes **no
  partial source**.
- **Cancellation is cooperative.** Cancelling raises `KeyboardInterrupt` at Python
  boundaries; a long NumPy/SciPy/CFFI/native call may not stop until it returns.
- **Memory.** The compatibility layer holds and copies the raw input, the Python result
  arrays, and the converted Arrow arrays, so large files can temporarily need substantial
  extra memory.

---

## Limitations & gotchas

- **Input is always `float32` bytes.** You get `numpy.fromfile(..., dtype=float32)` semantics
  and must reinterpret with `.view(...)` for other layouts; a partial trailing `float32` is
  dropped.
- **Equal length per topic.** Every field in a topic must match its clock's length.
- **Clocks are seconds.** `.rtc`/`.index` values are read as seconds and converted to µs;
  scale accordingly.
- **Not auto-detected.** You always pick a custom parser explicitly via its folder icon.
- **Not sandboxed.** See *Execution model & safety* above.

See also: **[Scripting](scripting.md)** for derived-field scripts and live transforms that
build on parsed (built-in or custom) data.
