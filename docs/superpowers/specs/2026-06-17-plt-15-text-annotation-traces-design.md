# PLT-15 — Text-annotation traces (string fields in plots) (design)

Date: 2026-06-17 · PLAN.md item **PLT-15** · §10

## Summary

A string field added to a plot pane renders as **text annotations**: one label
per sample drawn at its timestamp's x, overlaid in screen space. Used to read
ArduPilot `MSG` text, param-change strings, etc. in context over the numeric
data. Each label is auto-staggered top-down so they don't collide, and is
draggable **vertically only** (x is locked to the sample time) to declutter;
manual positions persist with the layout.

It is a pure egui overlay — no GPU line, no Y-axis contribution — so a string
trace coexists with numeric traces in the same pane.

## Decisions (from brainstorming)

- **Visual:** a faint full-height vertical line at each sample's timestamp, with
  the text anchored to it, so the exact time is clear even after dragging up.
- **Default placement:** greedy **top-down row packing** by label width so no two
  labels overlap horizontally within a row.
- **Drag:** vertical only; per-label offset **persisted in the layout**.
- **Mixing:** string traces overlay numeric traces; they do not affect the Y
  axis or the GPU draw.

## Current state (no gate to relax)

`FieldSchema::is_plottable()` has no callers in `delog-app`, and
`PlotPane::add_trace` doesn't gate by dtype — so string fields can already be
dropped; today they just produce an all-NaN f32 cache that draws nothing. This
feature adds the text rendering on top; the NaN cache is harmless (the GPU draws
nothing, and `visible_y_range` skips a non-finite cache), so string traces stay
out of the line path and the auto-Y range with no extra work.

## Components

### 1. Detect string traces

A helper `field_is_string(snapshot, field) -> bool` (delog-app) via the topic
schema (`FieldSchema::is_string`). Used to: skip string traces in the GPU /
y-range paths is already automatic (NaN cache), and to drive the text overlay.

### 2. `text_overlay` module (`crates/delog-app/src/text_overlay.rs`)

```rust
/// Draw a pane's string traces as text annotations and apply vertical drags.
/// `offsets` holds per-label manual y-fractions (0 = top .. 1 = bottom); only
/// manually-moved labels are present. Returns nothing; mutates `offsets`.
pub fn draw(
    ui: &egui::Ui,
    view: PaneView,
    origin_us: i64,
    snapshot: &StoreSnapshot,
    pane: &PlotPane,
    offsets: &mut HashMap<(FieldId, i64), f32>,
);
```

Per visible string trace (in `pane.visible_traces()` where `field_is_string`):

1. Collect visible samples: iterate `FieldView::chunks_overlapping(view_range)`
   and `value_at`, keeping `(t_us, label: String)` whose effective time maps
   into the pane's x-range. Cap at `MAX_LABELS = 256`; if exceeded, draw the
   first 256 (earliest) and a muted "+N more" note in the corner.
2. Lay out: sort by x. Greedy row-pack — each label's screen width is its galley
   width; place it in the first row whose last label's right edge is left of
   this label's x (+ a small gap), else start a new row. Row `r` → default
   `y_frac = top_margin_frac + r * row_height_frac` where `row_height_frac =
   (line_height + pad) / rect.height()`.
3. Resolve y: `offsets.get(&(field, t_us))` overrides the packed `y_frac`
   (manual labels lift out of packing; auto labels pack around the remainder).
4. Draw: a faint full-height `vline` at x in the trace colour (low alpha); the
   text at `(x + 3, rect.top() + y_frac*rect.height())`, `LEFT_TOP`, trace
   colour.
5. Interact: allocate the galley rect, `ui.interact(.., Sense::drag())`; on
   vertical drag, set `offsets[(field,t_us)] = (current_frac + drag_delta.y /
   rect.height()).clamp(0,1)`. Horizontal delta ignored.

The function reads samples directly (independent of the trace cache), so it
works regardless of the NaN cache.

### 3. Per-label offsets on `PlotPane`

```rust
// canonical µs key per label; y-fraction 0=top..1=bottom; only manual overrides.
pub text_offsets: HashMap<(FieldId, i64), f32>,
```

Pruning: entries whose field is no longer a trace are dropped opportunistically
(cheap; on save and on access).

### 4. Render integration (`workspace.rs` `plot_body`)

Call `text_overlay::draw(...)` in the `pane_overlay` scope (after `render_pane`,
near the playhead/marker draws), passing `&mut pane.text_offsets`. String traces
are already excluded from `render_pane` and `visible_y_range` via the NaN cache.
They appear in the legend (name + colour, toggle hides them).

### 5. Persistence (`layout.rs`)

```rust
pub struct TextOffsetLayout { pub field: FieldRef, pub t_us: i64, pub y_frac: f32 }
// LayoutNode::Plot gains:  #[serde(default)] text_offsets: Vec<TextOffsetLayout>
```

`node_to_layout` serializes `pane.text_offsets` (FieldId → topic.field via
`field_ref`); `insert_node` rebuilds the map (resolve topic.field → FieldId,
dropping unresolved). Keyed by field + absolute `t_us`, like marker times.

## Testing

- Pure unit test of the row-packer: given `(x, width)` inputs it produces
  non-overlapping rows, packed top-down (lower row index used first), and a
  label wider than the gap to its predecessor drops to a new row.
- `layout.rs`: `text_offsets` round-trips through JSON + `apply_doc`.
- Rendering/drag is GUI — manual run (drop a MSG/string field, see staggered
  labels with timestamp lines, drag one up, reload to confirm persistence).

## Out of scope (YAGNI / backlog)

Rotated/vertical text; per-label font/colour; collision avoidance against
manually-placed labels; hover tooltip for string traces (the text is already on
screen); skipping the NaN cache build for string fields (harmless; revisit if a
huge string field wastes memory).

## Checklist

Add **PLT-15** to PLAN.md §22 (PLT area) + a §10 note. Mark `[~]`/`[x]` in the
implementing commit with a one-line summary.
