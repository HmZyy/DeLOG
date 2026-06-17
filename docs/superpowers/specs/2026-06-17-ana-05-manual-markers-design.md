# ANA-05 — Manual markers / bookmarks (design)

Date: 2026-06-17 · PLAN.md item **ANA-05** · spec §17.4 · milestone M5

## Summary

Session bookmarks: user-placed time markers with a label, color, and note.
Add one at the playhead with `M`; manage them in a bottom **Markers** dock
(click-to-jump, edit, delete); see them as **flags on the timeline** and
**faint labeled verticals on every plot pane**. Persisted with the session.

These are distinct from the **ANA-10 measurement marker** (a single transient
delta cursor for ΔT/ΔY) and from §17.4 **auto-markers** (ANA-06, out of scope
here). Manual markers are multiple, labeled, navigable, and persisted.

## Decisions (from brainstorming)

- **Panel:** a bottom dock, alongside Diagnostics/Performance, toggled from the
  View menu.
- **Add flow:** pressing `M` drops a marker instantly at the playhead with an
  auto label (`Marker N`) and the next palette color; rename/recolor/note later.
- **Timeline flag interactions:** click = jump, hover = label/note tooltip,
  drag = move the marker's time, right-click = context menu (rename, recolor,
  delete).
- **Plot appearance:** a faint full-height vertical in the marker's color on
  every plot pane, with the label at the top.
- **Approach:** markers owned by `DelogApp`; the dock and timeline return action
  data, the app is the single mutator — mirroring the existing
  `DiagnosticsAction` / `TimelineAction` pattern. (Rejected: `Rc<RefCell>`
  shared state — breaks the action pattern, risks borrows across paint;
  folding into `marker_us` — wrong concept.)

## Data model (`crates/delog-app/src/markers.rs`, new)

```rust
/// A user bookmark at a canonical time (§17.4, ANA-05). Distinct from the
/// ANA-10 measurement cursor.
pub struct Marker {
    pub id: u64,        // stable identity across time-sorted reordering
    pub t_us: i64,      // canonical microseconds
    pub label: String,
    pub color: [f32; 4],// sRGB straight RGBA, like TraceRef
    pub note: String,
}

pub struct Markers {
    items: Vec<Marker>,
    next_id: u64,
}
```

`Markers` operations (pure, unit-tested):

- `add_at(t_us) -> u64` — pushes `Marker { id, t_us, label: "Marker {n}",
  color: palette::trace_color(count), note: "" }`, returns the new id. `n` is a
  monotonically increasing count (does not reuse numbers after deletes).
- `remove(id)`.
- `get_mut(id) -> Option<&mut Marker>`.
- `iter_by_time()` — markers sorted ascending by `t_us` (for display + flags);
  internal storage order is irrelevant.
- `is_empty()`, `len()`.

`id` gives stable identity so the dock and timeline can address a marker for
edit/delete/drag even though the displayed order is time-sorted and shifts as a
marker is dragged. Color cycles `delog_render::palette::trace_color`.

Lives on `DelogApp` as `markers: Markers`.

## `M` key

In the existing keymap block in `app.rs` (already guarded by
`!ui.ctx().egui_wants_keyboard_input()`, inside the `if let Some(range)` scope),
add `M`: `self.markers.add_at(self.playback.t_us)`. The guard means `M` never
fires while a text field (browser filter, marker rename) has focus.

## Markers dock (`MarkersDock` in `markers.rs`)

```rust
pub struct MarkersDock { pub open: bool }
impl MarkersDock {
    // Returns a jump target time when a row's jump control is clicked.
    pub fn ui(&mut self, ui: &mut egui::Ui, markers: &mut Markers) -> Option<i64>;
}
```

- A bottom `egui::Panel::bottom("markers")`, rendered next to the existing docks
  in `app.rs`; toggled by a `View ▸ ☑ Markers` checkbox.
- Rows sorted by `t_us`: editable color swatch · time (relative, e.g. `0:03.21`)
  · inline-editable label `TextEdit` · note `TextEdit` (single line) · a
  jump button (returns the time) · a delete button.
- Label/color/note edits mutate `markers` in place via `get_mut`. Delete removes
  by id. Empty state: a muted "No markers — press M to add one at the playhead."
- The returned `Option<i64>` is applied by the app as `playback.scrub(t, range)`.

## Timeline flags (`crates/delog-app/src/timeline.rs`)

The `scrubber` function gains the marker list and draws a flag per marker at
`bar_x_at(t_us, rect, range)` (small downward triangle + thin stem in the
marker color). `timeline::ui` takes `markers: &Markers` and extends the return:

```rust
pub struct TimelineAction {
    pub lock_live: bool,
    pub manual_scrub: bool,
    pub view_changed: bool,
    pub marker_jump: Option<i64>,         // click a flag → scrub here
    pub marker_move: Option<(u64, i64)>,  // drag a flag → set marker.t_us
    pub marker_delete: Option<u64>,       // right-click → delete
    pub marker_edit: Option<(u64, MarkerEdit)>, // right-click → rename/recolor
}

pub struct MarkerEdit { pub label: Option<String>, pub color: Option<[f32; 4]> }
```

Interaction, computed in `scrubber` with a hit radius (~6 px) on flag x:

- **drag started within a flag's hit radius** → drag that marker: set
  `marker_move = Some((id, bar_time_at(pointer.x)))` each drag frame, and do
  **not** scrub the playhead (same disambiguation as the plot marker drag).
- **click on a flag** (no drag) → `marker_jump = Some(t_us)`.
- **click elsewhere on the bar** → normal playhead scrub (unchanged).
- **hover a flag** → tooltip with the label and note (if any).
- **right-click a flag** → context menu: a rename `TextEdit`, a recolor picker,
  and Delete. The menu reads the current label/color (cloned for the widgets)
  and reports changes via `marker_edit = Some((id, MarkerEdit { label, color }))`
  / `marker_delete = Some(id)`; the app applies them through
  `markers.get_mut(id)` / `markers.remove(id)` after `timeline::ui` returns.

The app applies the actions against `self.markers` and `self.playback` after
`timeline::ui` returns, next to the existing `lock_live` / `view_changed`
handling.

## Plot verticals (`hover::draw_session_markers`)

A new function rendered in the `pane_overlay` scope of `plot_body`, after the
playhead/measurement-marker draw:

```rust
pub fn draw_session_markers(ui: &egui::Ui, view: PaneView, origin_us: i64, markers: &[Marker]);
```

For each marker whose `t_us` falls in the visible x-range: a full-height
`vline` in the marker color at ~0.3 alpha, with the label drawn at the top in
the marker color (small font). Read-only — no interaction on the plot. Markers
reach the pane via a new `PlotServices` field `markers: &'a [Marker]`.

Only plot panes draw markers; the 3D pane does not. Dense-label overlap at the
top is acceptable for v1 (polish later).

## Persistence (`crates/delog-app/src/layout.rs`)

```rust
pub struct LayoutDoc {
    // …
    #[serde(default)]
    pub markers: Vec<MarkerLayout>,
}

pub struct MarkerLayout { pub t_us: i64, pub label: String, pub color: [f32;4], pub note: String }
```

- `current_doc` serializes `Markers` → `Vec<MarkerLayout>` (ordered by time).
- `LayoutApply` gains `markers: Vec<MarkerLayout>`; `apply_doc` passes them
  through; `apply_layout` rebuilds `self.markers` from them (ids reassigned
  fresh, color/label/note preserved).
- Persists in `session.json` (30 s autosave → restored next launch) and in named
  layouts, consistent with how `marker_us` already persists. Loading a layout
  onto a different log carries marker times as-is (absolute), matching existing
  `marker_us` behavior.

## Testing

- `markers.rs` unit tests: `add_at` assigns increasing ids, cycles palette
  colors, labels `Marker 1/2/…`; `remove` by id; `iter_by_time` is sorted;
  label count does not reuse numbers after a delete.
- `layout.rs`: `LayoutDoc.markers` round-trips through JSON and `apply_doc`
  yields the same set (mirrors the existing `marker_us` round-trip test).
- Flag/vertical rendering and drag/hover/right-click are GUI — verified by a
  manual in-app run (clippy-clean, all tests green is the automated bar).

## Out of scope (YAGNI)

Auto-markers (ANA-06), marker categories/grouping, dedicated import/export
beyond the layout, gap/reset shading (ANA-07), and dense-label de-overlap.

## Checklist

ANA-05 in PLAN.md §22; mark `[~]` when starting, `[x]` on completion in the same
commit, with a one-line implementation summary.
