# Custom Python File Parsers Design

Date: 2026-06-20  
Checklist: SCR-11

## Purpose

Add user-managed Python file parsers without changing the existing parser files. A parser receives the selected file as a one-dimensional native-endian NumPy `float32` array and returns the legacy list of `(field_name, values, tooltip)` tuples. Its output enters DeLOG through the canonical ingest pipeline and behaves like output from a built-in parser.

This feature is explicit rather than sniffed: a user chooses a saved Python parser and then chooses the file to open with it. Built-in Rust parsers are not shown in the custom parser list.

## User Interface

Add **Tools ▸ Parsers** beside **Tools ▸ Scripts**. Its submenu contains:

1. **Add new parser...**
2. A separator
3. One row per saved Python parser, sorted by filename

Each parser row shows its filename followed by aligned pencil and open-file icon buttons. The pencil opens the editor. The open-file icon opens a native picker with all files visible and, after selection, immediately runs that parser on the selected file. Both icon buttons have tooltips.

Add and edit use one centered, non-collapsible **Custom Python Parser** popup consistent with the application's popup policy. The top row contains the parser filename, Save, and Delete. The rest of the popup is a Python code editor using the existing `egui_code_editor` integration. Delete is disabled for a new unsaved parser and otherwise requires the standard centered confirmation dialog.

The default new filename is `new_parser.py`. The default source is a minimal `Parse(raw_data)` example documenting the three-item return tuple. Saving enforces one `.py` suffix. Editing the filename and saving renames the parser rather than creating an implicit copy. Save validates both the filename and Python syntax but does not execute the parser.

## Persistence

`delog-script` gains a `ParserLibrary` separate from `ScriptLibrary`. It stores global reusable parser files in the application config directory under `parsers/*.py`. Parser filenames are the display names; there is no separate name or manifest.

Names must be non-empty local filenames and must reject path separators, `..`, and non-`.py` extensions. Library operations cover list, load, save/rename, and delete. Rename must not leave the old file behind after the new file is written successfully.

## Execution Architecture

The existing embedded CPython worker gains a parser command carrying the parser filename, source text, and selected data-file path. The parser is not registered as a `delog-parsers::LogParser`: Python belongs in `delog-script`, and `delog-app` explicitly coordinates this path. This preserves the dependency direction in PLAN.md section 3.2 and leaves native sniffing unchanged.

File reading and Python execution occur off the UI thread. Loading must match:

```python
numpy.fromfile(path, dtype=numpy.float32)
```

That means a contiguous, one-dimensional, native-endian `float32` array and silent omission of an incomplete trailing 1-3 bytes. The worker executes the saved source in a parser-specific namespace, requires a callable named `Parse`, and invokes `Parse(raw_data)` exactly. Existing imports such as NumPy, Bottleneck, CFFI, and SciPy resolve from the same Python environment used by ordinary scripts.

The parser worker is shared with scripts, the REPL, and live transforms, so these jobs execute serially. Parser code runs with full user privileges. A Python or native-extension process crash can terminate DeLOG because execution is deliberately in-process.

## Compatibility Contract

A parser must expose:

```python
def Parse(raw_data):
    return [(field_name, values, tooltip), ...]
```

The supplied legacy parser must run without edits. For every returned entry:

- The entry has exactly three items.
- `field_name` is a non-empty Python string and is preserved, including Unicode and trailing spaces.
- `values` is a one-dimensional numeric or Boolean NumPy-compatible array. NumPy views, slices, and expressions are accepted; materialization happens only during Arrow conversion.
- `tooltip` is a string and may be empty.

The prefix before the first dot is the topic name. The remainder is the field name. For example, `gps.main.lat` maps to topic `gps`, field `main.lat`. A name without a dot cannot establish a topic and is rejected with the offending name.

Each topic uses `<topic>.rtc` as its clock when present. Otherwise it uses `<topic>.index`. RTC values are seconds and are converted to canonical signed 64-bit microseconds. Index values are treated as seconds for canonical storage as well; they remain available as returned data, so users retain the normalized index field. Every field array in a topic must match its selected clock length.

Both `.rtc` and `.index` remain visible fields in the topic. Exact duplicate full field names are permitted because the supplied parser returns some duplicates. The last occurrence wins and one warning diagnostic identifies the duplicate. Numeric NaNs remain field gaps. Non-finite clock values, unsupported values, missing topic clocks, and unequal field/clock lengths reject the result with a precise field-level error.

Tooltip strings are stored as optional field description metadata and shown from the browser field hover UI. This requires a backward-compatible addition to `FieldSchema`; existing constructors continue to produce fields without descriptions.

## Ingestion And Source Lifecycle

Parser execution is transactional. DeLOG reads the input, runs Python, validates the complete returned object, and converts it before opening or publishing the source. A failure therefore leaves no partial source in the store.

On success, the app opens a `SourceKind::File` source and emits one `ParsedBatch` per topic through the blocking file ingest sink. Arrays use their compatible Arrow numeric or Boolean dtype when practical; timestamp conversion always produces canonical `Int64Array` microseconds. The source label is the selected data filename and uses the normal `#2`, `#3` collision behavior. The resulting source supports the browser, plots, statistics, layouts, export, and diagnostics without custom downstream branches.

The current parse-progress UI first reports file reading and then an indeterminate **Running Python parser...** state. The return-list API cannot expose incremental Python progress or stream topic results.

## Cancellation And Errors

Cancel uses the scripting engine's existing pending `KeyboardInterrupt`. It is cooperative at Python bytecode boundaries. Long calls inside NumPy, SciPy, CFFI, or another native extension may not stop until that call returns.

Syntax validation errors remain in the editor with filename and line information. Runtime exceptions capture the full Python traceback in the diagnostics hub and show a concise error popup naming the parser and selected data file. Contract validation errors identify the returned entry or field and the expected and actual types or lengths.

No failed or cancelled parse publishes a source. A cancellation or runtime failure increments its metric once and does not poison the worker namespace for subsequent commands.

## Memory And Performance

This compatibility API necessarily holds the raw NumPy input, Python-owned result arrays, and converted Arrow arrays during conversion. It cannot satisfy parser-to-store ZC-1 because the parser produces Python-owned buffers and a complete list. Mark these copies explicitly:

```rust
// ZC-EXCEPTION: custom Python parser compatibility input/output copy; off the UI
// hot path and accounted by python_parser_input_bytes/python_parser_output_bytes.
```

Record input bytes, converted Arrow bytes, read duration, Python duration, validation/conversion duration, total duration, cancellation count, and failure count in the shared metrics registry. All work remains outside UI, render, and cache hot paths. Large-output performance is bounded primarily by the legacy return-list contract rather than by rendering.

## Testing And Benchmarking

Unit tests cover:

- Parser library save, sorted list, load, rename, delete, suffix normalization, and unsafe-name rejection.
- Exact native-endian `float32` loading and omission of trailing 1-3 bytes.
- Syntax validation without execution.
- Topic/field splitting, RTC preference, index fallback, seconds-to-microseconds conversion, visible clock fields, Unicode/trailing-space names, tooltip descriptions, NaNs, Boolean/numeric dtypes, and duplicate last-wins behavior.
- Missing `Parse`, invalid tuple shapes, unsupported arrays, names without topics, missing clocks, non-finite clocks, unequal lengths, syntax/runtime exceptions, and cancellation.
- Transactionality: every failure path leaves no source behind.

An integration test runs an unchanged representative legacy `Parse(raw_data)` parser using NumPy views and verifies the resulting source, topics, fields, clocks, descriptions, and values through the store snapshot. A second end-to-end test exercises the app coordination path through ingestion.

UI policy tests cover the menu actions and centered non-collapsible editor and confirmation windows. Manual GUI verification covers add, save, rename, edit, delete, file selection, progress, successful load, traceback display, and cancellation.

Add a Criterion benchmark using a large synthetic `float32` file and many output fields. Report read, Python, and conversion stages independently so regressions can be localized.

## Documentation

Update `docs/scripting.md` with the Tools ▸ Parsers workflow, exact parser function contract, timestamp grouping rules, environment/import behavior, cancellation limitation, trusted-code warning, and a compact working parser example. The implementation commit marks SCR-11 complete only after build, format, Clippy, workspace tests, scripting-feature tests, and the parser benchmark pass.
