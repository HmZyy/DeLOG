# ANA-11 — Generate markers from field values (design)

Date: 2026-06-17 · PLAN.md item **ANA-11** · spec §17.4 · builds on ANA-05

## Summary

A **Generate markers** action on a field's right-click menu (next to **Field
stats**, in the data browser field rows and the plot trace submenu). It scans a
discrete field's unique values and opens a popup listing each value with an
include checkbox, an editable name (default `Value <v>`) and a color swatch.
Clicking generate appends ANA-05 session markers at every point the field
*transitions into* that value, named and coloured per the popup.

Primary use case: flight modes. Select the ArduPilot mode field, get the unique
mode codes, name them (or keep the numbers), and get a marker at every mode
change — which, combined with ANA-05 region shading, yields coloured mode bands.

## Decisions (from brainstorming)

- **Placement:** a marker at **every transition into a value** (the start of
  each contiguous run). A value entered N times yields N markers.
- **Selection:** the popup lists **all** unique values, each with an include
  checkbox (default on), editable name and color.
- **Eligible fields:** discrete only — integer/unsigned/bool/string. Floats are
  excluded (the menu item is hidden/disabled). A cardinality cap of **64**
  distinct values; above it, refuse with a "too many values" message.
- **Re-run:** **append**. Re-generating duplicates; the user deletes unwanted
  markers manually (ANA-05 dock/timeline). No source tracking in v1.
- **Color is stable per value:** the default color is derived deterministically
  from the value, so the same value is always the same color across
  regenerations and across logs. Editable per row.
- **Approach:** scan once when the popup opens, in `delog-core` (Arrow stays out
  of the app, per §3.2). The popup edits a draft; generate appends to `Markers`.

## Core (`crates/delog-core/src/analysis.rs`)

```rust
/// One distinct value of a field and the canonical times at which the field
/// transitions *into* it (start of each contiguous run). `value_label` is the
/// canonical display of the value ("4", "true", "Auto").
pub struct ValueTransitions {
    pub value_label: String,
    pub transitions: Vec<i64>, // canonical µs, ascending
}

pub enum TransitionsError {
    FieldView(FieldViewError),
    /// More than `max_distinct` distinct values — refuse (continuous field?).
    TooManyValues(usize),
}

/// Walk the field's samples in effective-time order and group the transition
/// times by distinct value. A transition is a row whose value differs from the
/// previous non-null value (the first non-null sample counts). Null/missing is
/// a gap, not a value — it ends a run and is not itself marked. Errors with
/// `TooManyValues` once distinct count exceeds `max_distinct`.
pub fn field_value_transitions(
    snapshot: &StoreSnapshot,
    field: FieldId,
    max_distinct: usize,
) -> Result<Vec<ValueTransitions>, TransitionsError>;
```

Implementation walks `FieldView::chunks_overlapping(full_range)` (or the topic
store's chunk spine) in order; for each row reads the canonical time
(`chunk.t[row] + source_offset`) and the value (`value_at`), formats a stable
`value_label`, and records a transition when the label differs from the running
previous label. Distinct labels accumulate into an order-preserving map; exceed
the cap → `TooManyValues`. Returned groups are sorted by `value_label` for a
stable popup order.

Result ordering across chunks must be by effective time; reuse the existing
monotonic-spine handling. (Discrete fields like mode are low-rate, so a single
inline scan is acceptable; if a high-rate discrete field janks the UI, moving
this to a §19.6 job is a backlog follow-up.)

## Menu + action plumbing (`delog-app`)

- **Browser field rows** (`browser.rs`): add a **Generate markers** button next
  to **Field stats**, shown only when the field's `FieldSchema` dtype is
  discrete (int/uint/bool/string). Carry it on `BrowserResponse` as
  `generate_markers: Option<FieldId>`.
- **Plot trace submenu** (`workspace.rs`): same item in the per-trace menu next
  to **Field stats**; carry it on `WorkspaceActions` as
  `generate_markers: Option<FieldId>`.
- **App** (`app.rs`): both actions set `self.generate_markers_dialog`. Opening
  runs `field_value_transitions(snapshot, field, 64)` and builds the draft (or
  stores the error to show the cap message).

## Popup (`crates/delog-app/src/markers.rs` or a sibling)

```rust
struct ValueRow {
    label: String,        // canonical value display, e.g. "4"
    transitions: Vec<i64>,
    include: bool,        // default true
    name: String,         // default format!("Value {label}")
    color: [f32; 4],      // default value_color(&label); editable
}

pub struct GenerateMarkersDialog {
    field: FieldId,
    title: String,        // topic.field
    rows: Vec<ValueRow>,  // empty + error message if TooManyValues
    error: Option<String>,
}
```

Rendered as `egui::Window` keyed by `("generate_markers", field.0)` (like Field
Stats). Per row: include checkbox · the raw value (monospace) · a name
`TextEdit` · an editable color swatch (`color_edit_button_srgba`). Footer: a
"Generate N markers" button (N = total transitions across included rows) and the
count of unique values; if `error` is set, show it instead and disable generate.

On generate: for each included row, for each transition `t_us`, append a marker
via `Markers::push_loaded(t_us, row.name.clone(), row.color, String::new())`.
Then close the dialog. No glyphs — icons are SVGs and labels are plain text
(per the project icon/glyph rule).

## Color = stable per value (`delog-app`)

```rust
/// Deterministic palette colour for a value label (ANA-11): FNV-1a hash of the
/// label, indexed into the trace palette. Same label → same colour, every time.
fn value_color(label: &str) -> [f32; 4] {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in label.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    delog_render::palette::trace_color(h as usize).to_srgb_f32()
}
```

`DefaultHasher` is avoided (its seeds are fixed today but not guaranteed); a
fixed FNV-1a guarantees stability across processes and versions.

## Generated markers

Generated markers are ordinary ANA-05 `Marker`s — they appear in the Markers
dock, on the timeline as flags, and as plot verticals, are styleable via the
ANA-05 settings, and persist in the layout. No new marker subtype.

## Testing

- `field_value_transitions`: run-start detection (`0 0 4 4 0 4` → value 4 at
  rows 2 and 5; value 0 at rows 0 and 4); null handling (null ends a run, is not
  marked); ascending transition times across multiple chunks; `TooManyValues`
  past the cap; non-discrete field handled by the caller (menu gating).
- `value_color`: deterministic (same label twice → equal), and distinct common
  labels land on palette indices (smoke test).
- Popup generate logic (append count = sum of included transitions) — unit-test
  the pure "rows → marker specs" step if extracted; GUI verified by manual run.

## Out of scope (YAGNI / backlog)

- **Enum-name autofill** — ArduPilot/PX4 mode names are not captured in the
  schema today; defaults stay `Value <n>`. Auto-naming from parser-captured
  enums is a future PAR-* + ANA enhancement.
- Replace-on-regenerate (source-tagged markers).
- Off-thread scan for high-rate discrete fields (§19.6 job).

## Checklist

Add **ANA-11** to PLAN.md §22 (ANA area) and a §17.4 paragraph. Mark `[~]` on
start, `[x]` on completion in the same commit with a one-line summary.
