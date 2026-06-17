# ANA-05 Manual Markers / Bookmarks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add user-placed session bookmarks (time + label + color + note) with an `M` shortcut, a bottom Markers dock, timeline flags, and faint labeled verticals on every plot, persisted with the session.

**Architecture:** Markers live on `DelogApp` as a `Markers` collection. The dock and timeline render from a read-only view and report interactions via action data; the app is the single mutator (mirrors the existing `DiagnosticsAction`/`TimelineAction` pattern). Plots draw read-only verticals via a new `PlotServices` slice. Persistence rides the existing `LayoutDoc` (session autosave + named layouts), like the ANA-10 `marker_us`.

**Tech Stack:** Rust, egui/eframe, `delog-app` crate. Spec: `docs/superpowers/specs/2026-06-17-ana-05-manual-markers-design.md`.

## Global Constraints

- Time is `i64` microseconds end to end; floats only for screen mapping (PLAN.md invariants).
- `cargo clippy -p delog-app --no-default-features -- -D warnings` must be clean (Definition of Done).
- Tests run with `cargo test -p delog-app --no-default-features` (no Python toolchain needed).
- `cargo fmt --all` before each commit.
- Marker colors stored as sRGB straight RGBA `[f32; 4]`, like `TraceRef`.
- These manual markers are **distinct** from the ANA-10 measurement cursor (`marker_us`) and from auto-markers (ANA-06, out of scope).
- Commit messages end with the repo trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

### Task 1: Markers data model

**Files:**
- Create: `crates/delog-app/src/markers.rs`
- Modify: `crates/delog-app/src/main.rs` (add `mod markers;`)

**Interfaces:**
- Produces: `struct Marker { id: u64, t_us: i64, label: String, color: [f32;4], note: String }` with `fn color32(&self) -> egui::Color32`; `struct Markers` with `fn new() -> Self`, `fn add_at(&mut self, t_us: i64) -> u64`, `fn remove(&mut self, id: u64)`, `fn get_mut(&mut self, id: u64) -> Option<&mut Marker>`, `fn by_time(&self) -> Vec<&Marker>`, `fn as_slice(&self) -> &[Marker]`, `fn push_loaded(&mut self, t_us: i64, label: String, color: [f32;4], note: String)`, `fn len(&self) -> usize`, `fn is_empty(&self) -> bool`.

- [ ] **Step 1: Confirm the module-declaration site**

Run: `grep -n "^mod \|^pub mod " crates/delog-app/src/main.rs | head`
Expected: a list of `mod hover;`, `mod legend;`, `mod layout;`, … (alphabetical-ish). Note where `mod markers;` belongs (after `mod legend;` / before `mod models;`).

- [ ] **Step 2: Write `markers.rs` with the data model**

```rust
//! Manual markers / bookmarks (PLAN.md §17.4, ANA-05): user-placed time
//! markers with a label, colour and note. Distinct from the ANA-10 measurement
//! cursor (a single transient delta cursor) — these are multiple, labelled,
//! navigable, and persisted with the session.

/// One bookmark at a canonical time. `id` is a stable identity so the dock and
/// timeline can address a marker for edit/delete/drag even as the time-sorted
/// display order shifts.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub id: u64,
    pub t_us: i64,
    pub label: String,
    /// sRGB straight RGBA, like `TraceRef`.
    pub color: [f32; 4],
    pub note: String,
}

impl Marker {
    pub fn color32(&self) -> egui::Color32 {
        let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        egui::Color32::from_rgba_unmultiplied(
            u(self.color[0]),
            u(self.color[1]),
            u(self.color[2]),
            u(self.color[3]),
        )
    }
}

/// The session's marker collection. Monotonic `next_id` never reuses numbers,
/// so labels and ids stay stable across deletions.
#[derive(Debug, Default)]
pub struct Markers {
    items: Vec<Marker>,
    next_id: u64,
}

impl Markers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a marker at `t_us` with an auto label (`Marker N`) and the next
    /// palette colour. Returns the new id.
    pub fn add_at(&mut self, t_us: i64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let color = delog_render::palette::trace_color(id as usize).to_srgb_f32();
        self.items.push(Marker {
            id,
            t_us,
            label: format!("Marker {}", id + 1),
            color,
            note: String::new(),
        });
        id
    }

    /// Re-add a marker loaded from persistence, assigning a fresh id.
    pub fn push_loaded(&mut self, t_us: i64, label: String, color: [f32; 4], note: String) {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Marker {
            id,
            t_us,
            label,
            color,
            note,
        });
    }

    pub fn remove(&mut self, id: u64) {
        self.items.retain(|m| m.id != id);
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Marker> {
        self.items.iter_mut().find(|m| m.id == id)
    }

    /// Markers sorted ascending by time (display order, flags, verticals).
    pub fn by_time(&self) -> Vec<&Marker> {
        let mut v: Vec<&Marker> = self.items.iter().collect();
        v.sort_by_key(|m| m.t_us);
        v
    }

    pub fn as_slice(&self) -> &[Marker] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_assigns_increasing_ids_labels_and_distinct_colors() {
        let mut m = Markers::new();
        let a = m.add_at(100);
        let b = m.add_at(50);
        assert_eq!((a, b), (0, 1));
        assert_eq!(m.as_slice()[0].label, "Marker 1");
        assert_eq!(m.as_slice()[1].label, "Marker 2");
        assert_ne!(m.as_slice()[0].color, m.as_slice()[1].color);
    }

    #[test]
    fn by_time_sorts_ascending_regardless_of_insertion_order() {
        let mut m = Markers::new();
        m.add_at(100);
        m.add_at(50);
        m.add_at(75);
        let times: Vec<i64> = m.by_time().iter().map(|x| x.t_us).collect();
        assert_eq!(times, [50, 75, 100]);
    }

    #[test]
    fn remove_by_id_and_labels_do_not_reuse_numbers() {
        let mut m = Markers::new();
        let a = m.add_at(10);
        m.add_at(20);
        m.remove(a);
        assert_eq!(m.len(), 1);
        // Next add keeps counting up — no reuse of "Marker 1".
        m.add_at(30);
        let labels: Vec<&str> = m.by_time().iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["Marker 2", "Marker 3"]);
    }

    #[test]
    fn get_mut_edits_in_place() {
        let mut m = Markers::new();
        let id = m.add_at(10);
        m.get_mut(id).unwrap().label = "Takeoff".to_string();
        assert_eq!(m.as_slice()[0].label, "Takeoff");
        assert!(m.get_mut(999).is_none());
    }
}
```

- [ ] **Step 3: Declare the module**

Add `mod markers;` to `crates/delog-app/src/main.rs` at the site found in Step 1.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p delog-app --no-default-features markers::`
Expected: 4 tests pass.

- [ ] **Step 5: Clippy + fmt + commit**

```bash
cargo fmt --all
cargo clippy -p delog-app --no-default-features -- -D warnings
git add crates/delog-app/src/markers.rs crates/delog-app/src/main.rs
git commit -m "ANA-05: Marker / Markers data model

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Layout persistence

**Files:**
- Modify: `crates/delog-app/src/layout.rs`

**Interfaces:**
- Consumes: `Markers::as_slice()`, `Marker` fields (Task 1).
- Produces: `struct MarkerLayout { t_us: i64, label: String, color: [f32;4], note: String }`; `LayoutDoc.markers: Vec<MarkerLayout>`; `CurrentLayout.markers: Vec<MarkerLayout>`; `LayoutApply.markers: Vec<MarkerLayout>`.

- [ ] **Step 1: Add the `MarkerLayout` type**

In `crates/delog-app/src/layout.rs`, after the `TraceLayout` struct (near line 108), add:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarkerLayout {
    pub t_us: i64,
    pub label: String,
    pub color: [f32; 4],
    pub note: String,
}
```

- [ ] **Step 2: Add the field to `LayoutDoc`**

In the `LayoutDoc` struct, after the `vehicles` field, add:

```rust
    /// Manual markers / bookmarks for this session (§17.4, ANA-05).
    #[serde(default)]
    pub markers: Vec<MarkerLayout>,
```

- [ ] **Step 3: Add the field to `CurrentLayout` and `LayoutApply`**

In `CurrentLayout<'a>` add `pub markers: Vec<MarkerLayout>,` and in `LayoutApply` add `pub markers: Vec<MarkerLayout>,` (both after their `vehicles`/`follow_live` neighbours).

- [ ] **Step 4: Wire `current_doc` and `apply_doc`**

In `current_doc`, set the new field (after `vehicles: …`):

```rust
        markers: input.markers,
```

In `apply_doc`, in the returned `LayoutApply { … }` (after `follow_live`/`marker_us`):

```rust
        markers: doc.markers,
```

- [ ] **Step 5: Fix the `empty_doc` test helper and add a round-trip test**

In `empty_doc` (test module), add `markers: Vec::new(),` to the `LayoutDoc { … }` literal (after `vehicles`).

Add this test next to `global_marker_us_round_trips_through_json_and_apply`:

```rust
    #[test]
    fn manual_markers_round_trip_through_json_and_apply() {
        let mut doc = empty_doc("markers");
        doc.markers = vec![
            MarkerLayout {
                t_us: 1_000,
                label: "Takeoff".into(),
                color: [1.0, 0.0, 0.0, 1.0],
                note: "rotate".into(),
            },
            MarkerLayout {
                t_us: 9_000,
                label: "Land".into(),
                color: [0.0, 1.0, 0.0, 1.0],
                note: String::new(),
            },
        ];

        let decoded = decode_doc(&doc_json(&doc).unwrap()).expect("decode");
        assert_eq!(decoded.markers.len(), 2);
        assert_eq!(decoded.markers[0].label, "Takeoff");

        let LoadOutcome::Applied(layout) =
            load_doc(decoded, &StoreSnapshot::empty()).expect("apply")
        else {
            panic!("no sources → no mapping");
        };
        assert_eq!(layout.markers.len(), 2);
        assert_eq!(layout.markers[1].t_us, 9_000);
    }
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p delog-app --no-default-features layout::`
Expected: all layout tests pass, including `manual_markers_round_trip_through_json_and_apply`.

- [ ] **Step 7: Clippy + fmt + commit**

```bash
cargo fmt --all
cargo clippy -p delog-app --no-default-features -- -D warnings
git add crates/delog-app/src/layout.rs
git commit -m "ANA-05: persist manual markers in LayoutDoc

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: App state, `M` key, and persistence wiring

**Files:**
- Modify: `crates/delog-app/src/app.rs`

**Interfaces:**
- Consumes: `Markers` (Task 1); `LayoutDoc.markers`, `CurrentLayout.markers`, `LayoutApply.markers`, `MarkerLayout` (Task 2).
- Produces: `DelogApp.markers: Markers` used by Tasks 4-6.

- [ ] **Step 1: Add the field to `DelogApp`**

In the `DelogApp` struct (near the `hover_mode` field), add:

```rust
    /// Manual markers / bookmarks (§17.4, ANA-05).
    markers: crate::markers::Markers,
```

- [ ] **Step 2: Initialise it in `DelogApp::new`**

In the struct-literal in `new` (near `hover_mode: …`), add:

```rust
            markers: crate::markers::Markers::new(),
```

- [ ] **Step 3: Serialize markers when building the layout doc**

In `current_layout_doc` (the `CurrentLayout { … }` literal, after `vehicles: &self.vehicles,`), add:

```rust
            markers: self
                .markers
                .as_slice()
                .iter()
                .map(|m| crate::layout::MarkerLayout {
                    t_us: m.t_us,
                    label: m.label.clone(),
                    color: m.color,
                    note: m.note.clone(),
                })
                .collect(),
```

- [ ] **Step 4: Rebuild markers on layout apply**

In `apply_layout`, after `self.playback.follow_live = layout.follow_live;` (and the `marker_us` line), add:

```rust
        let mut markers = crate::markers::Markers::new();
        for m in layout.markers {
            markers.push_loaded(m.t_us, m.label, m.color, m.note);
        }
        self.markers = markers;
```

- [ ] **Step 5: Add the `M` shortcut**

In the keymap block (the `ui.ctx().input(|i| { … })` tuple guarded by `!ui.ctx().egui_wants_keyboard_input()`), add `i.key_pressed(egui::Key::M)` to the captured tuple as `add_marker`, then after the `space`/`home`/`end` handlers add:

```rust
    if add_marker {
        self.markers.add_at(self.playback.t_us);
    }
```

(The existing `!egui_wants_keyboard_input()` guard means `M` never fires while a text field is focused.)

- [ ] **Step 6: Build (compile check — full UI wiring lands in later tasks)**

Run: `cargo build -p delog-app --no-default-features`
Expected: builds clean. (Markers are stored and persisted but not yet rendered.)

- [ ] **Step 7: Clippy + fmt + commit**

```bash
cargo fmt --all
cargo clippy -p delog-app --no-default-features -- -D warnings
git add crates/delog-app/src/app.rs
git commit -m "ANA-05: app marker state, M shortcut, layout round-trip wiring

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Markers dock

**Files:**
- Modify: `crates/delog-app/src/markers.rs` (add `MarkersDock`)
- Modify: `crates/delog-app/src/app.rs` (field, View-menu toggle, render)

**Interfaces:**
- Consumes: `Markers`, `Marker` (Task 1); `DelogApp.markers` (Task 3).
- Produces: `struct MarkersDock { pub open: bool }` with `fn ui(&mut self, ui: &mut egui::Ui, markers: &mut Markers, origin_us: i64) -> Option<i64>`.

- [ ] **Step 1: Add `MarkersDock` to `markers.rs`**

Append to `crates/delog-app/src/markers.rs` (before the `#[cfg(test)]` module):

```rust
/// Bottom dock listing the session's markers (§17.4, ANA-05): per row a colour
/// swatch, relative time, editable label + note, and jump / delete controls.
/// Returns a jump target time when a row's jump button is clicked.
pub struct MarkersDock {
    pub open: bool,
}

impl Default for MarkersDock {
    fn default() -> Self {
        Self { open: false }
    }
}

impl MarkersDock {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        markers: &mut Markers,
        origin_us: i64,
    ) -> Option<i64> {
        if markers.is_empty() {
            ui.weak("No markers — press M to add one at the playhead.");
            return None;
        }
        let mut jump = None;
        let mut to_remove = None;
        let ids: Vec<u64> = markers.by_time().iter().map(|m| m.id).collect();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for id in ids {
                let Some(m) = markers.get_mut(id) else {
                    continue;
                };
                ui.horizontal(|ui| {
                    let mut color = m.color32();
                    if egui::color_picker::color_edit_button_srgba(
                        ui,
                        &mut color,
                        egui::color_picker::Alpha::Opaque,
                    )
                    .changed()
                    {
                        m.color = crate::legend::color32_to_srgb(color);
                    }
                    ui.monospace(fmt_rel(m.t_us, origin_us));
                    ui.add(
                        egui::TextEdit::singleline(&mut m.label)
                            .desired_width(140.0)
                            .hint_text("label"),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut m.note)
                            .desired_width(180.0)
                            .hint_text("note"),
                    );
                    if ui.button("⤓").on_hover_text("Jump to marker").clicked() {
                        jump = Some(m.t_us);
                    }
                    if ui.button("✕").on_hover_text("Delete marker").clicked() {
                        to_remove = Some(id);
                    }
                });
            }
        });
        if let Some(id) = to_remove {
            markers.remove(id);
        }
        jump
    }
}

/// Format a canonical time relative to the log origin as `m:ss.cc`.
fn fmt_rel(t_us: i64, origin_us: i64) -> String {
    let secs = (t_us - origin_us) as f64 * 1e-6;
    let sign = if secs < 0.0 { "-" } else { "" };
    let s = secs.abs();
    let m = (s / 60.0).floor() as i64;
    let rem = s - (m as f64) * 60.0;
    format!("{sign}{m}:{rem:05.2}")
}
```

- [ ] **Step 2: Add a unit test for `fmt_rel`**

Add to the `#[cfg(test)] mod tests` in `markers.rs`:

```rust
    #[test]
    fn fmt_rel_formats_minutes_seconds_centis() {
        assert_eq!(super::fmt_rel(3_210_000, 0), "0:03.21");
        assert_eq!(super::fmt_rel(62_000_000, 0), "1:02.00");
        assert_eq!(super::fmt_rel(0, 1_000_000), "-0:01.00");
    }
```

- [ ] **Step 3: Run the markers tests**

Run: `cargo test -p delog-app --no-default-features markers::`
Expected: 5 tests pass.

- [ ] **Step 4: Add the dock field + init to `DelogApp`**

In `app.rs`, add to the struct (near `diagnostics_dock`/`performance_dock`):

```rust
    markers_dock: crate::markers::MarkersDock,
```

and in `new` (near the other dock inits):

```rust
            markers_dock: crate::markers::MarkersDock::default(),
```

- [ ] **Step 5: Add the View-menu toggle**

In the View menu (next to the Diagnostics/Performance checkboxes), add:

```rust
                if ui
                    .checkbox(&mut self.markers_dock.open, "Markers")
                    .clicked()
                {
                    ui.close();
                }
```

- [ ] **Step 6: Render the dock**

In `app.rs`, immediately after the `performance_dock` render block, add:

```rust
        if self.markers_dock.open {
            egui::Panel::bottom("markers")
                .resizable(true)
                .default_size(200.0)
                .show_inside(ui, |ui| {
                    ui.heading("Markers");
                    if let Some(t_us) = self.markers_dock.ui(ui, &mut self.markers, self.origin_us)
                    {
                        if let Some(range) = snapshot.global_time_range() {
                            self.playback.scrub(t_us, range);
                        }
                    }
                });
        }
```

(If `snapshot`/`range` are not in scope at that exact line, place the block inside the same scope the other docks use — they already have `snapshot` available.)

- [ ] **Step 7: Build + clippy + fmt + commit**

```bash
cargo build -p delog-app --no-default-features
cargo test -p delog-app --no-default-features markers::
cargo fmt --all
cargo clippy -p delog-app --no-default-features -- -D warnings
git add crates/delog-app/src/markers.rs crates/delog-app/src/app.rs
git commit -m "ANA-05: Markers dock (list, edit, jump, delete)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Timeline flags

**Files:**
- Modify: `crates/delog-app/src/timeline.rs`
- Modify: `crates/delog-app/src/app.rs` (pass markers in; apply actions)

**Interfaces:**
- Consumes: `Markers`, `Marker` (Task 1); `bar_x_at`/`bar_time_at` (existing).
- Produces: extended `TimelineAction` with `marker_jump: Option<i64>`, `marker_move: Option<(u64,i64)>`, `marker_delete: Option<u64>`, `marker_edit: Option<(u64, MarkerEdit)>`; `struct MarkerEdit { label: Option<String>, color: Option<[f32;4]> }`.

- [ ] **Step 1: Extend `TimelineAction` and add `MarkerEdit`**

In `timeline.rs`, derive `Default` on `TimelineAction` (add `#[derive(Default)]` if absent) and add the fields:

```rust
#[derive(Default)]
pub struct TimelineAction {
    pub lock_live: bool,
    pub manual_scrub: bool,
    pub view_changed: bool,
    /// Click a timeline flag → scrub the playhead here (ANA-05).
    pub marker_jump: Option<i64>,
    /// Drag a flag → set that marker's time (ANA-05).
    pub marker_move: Option<(u64, i64)>,
    /// Right-click → delete that marker (ANA-05).
    pub marker_delete: Option<u64>,
    /// Right-click → rename/recolour that marker (ANA-05).
    pub marker_edit: Option<(u64, MarkerEdit)>,
}

#[derive(Default, Clone)]
pub struct MarkerEdit {
    pub label: Option<String>,
    pub color: Option<[f32; 4]>,
}
```

Update the place where `TimelineAction` is constructed in `ui` to `let mut action = TimelineAction::default();` (replacing any explicit literal).

- [ ] **Step 2: Thread markers into `timeline::ui` and `scrubber`**

Change `pub fn ui(...)` to take `markers: &crate::markers::Markers` (add the parameter after `view`). Change `fn scrubber(...)` signature to:

```rust
fn scrubber(
    ui: &mut egui::Ui,
    playback: &mut Playback,
    range: TimeRange,
    any_live: bool,
    markers: &crate::markers::Markers,
    action: &mut TimelineAction,
) -> bool {
```

and pass `markers, &mut action` where `ui` calls `scrubber(...)`.

- [ ] **Step 3: Draw flags + interactions in `scrubber`**

Inside `scrubber`, replace the click/drag handling and add flag drawing. Use this structure (adapt to the existing variable names `rect`, `response`, `scrubbed`, `painter`):

```rust
    // Hit-test the nearest flag to the pointer at press time.
    let hit_radius = 6.0;
    let nearest_flag = |x: f32| -> Option<u64> {
        markers
            .by_time()
            .iter()
            .map(|m| (m.id, (bar_x_at(m.t_us, rect, range) - x).abs()))
            .filter(|(_, d)| *d <= hit_radius)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    };

    // Track which marker (if any) is being dragged across frames.
    let drag_key = ui.id().with("marker_drag");
    let mut dragging: Option<u64> = ui.data(|d| d.get_temp::<Option<u64>>(drag_key).flatten());

    if response.drag_started() {
        dragging = response.interact_pointer_pos().and_then(|p| nearest_flag(p.x));
        ui.data_mut(|d| d.insert_temp(drag_key, dragging));
    }

    let mut scrubbed = false;
    if let Some(id) = dragging {
        // Move the marker, do not scrub the playhead.
        if let Some(p) = response.interact_pointer_pos() {
            action.marker_move = Some((id, bar_time_at(p.x, rect, range)));
        }
        if response.drag_stopped() {
            ui.data_mut(|d| d.insert_temp::<Option<u64>>(drag_key, None));
        }
    } else if response.clicked() {
        if let Some(p) = response.interact_pointer_pos() {
            if let Some(id) = nearest_flag(p.x) {
                // Jump to the clicked marker.
                if let Some(m) = markers.by_time().into_iter().find(|m| m.id == id) {
                    action.marker_jump = Some(m.t_us);
                }
            } else {
                playback.scrub(bar_time_at(p.x, rect, range), range);
                scrubbed = true;
            }
        }
    } else if response.dragged() {
        // Plain drag on the bar (not on a flag) scrubs.
        if let Some(p) = response.interact_pointer_pos() {
            playback.scrub(bar_time_at(p.x, rect, range), range);
            scrubbed = true;
        }
    }
```

After the existing playhead handle is drawn, draw the flags:

```rust
    for m in markers.by_time() {
        let fx = bar_x_at(m.t_us, rect, range);
        let color = m.color32();
        painter.vline(fx, rect.y_range(), egui::Stroke::new(1.0, color));
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(fx - 4.0, rect.top()),
                egui::pos2(fx + 4.0, rect.top()),
                egui::pos2(fx, rect.top() + 6.0),
            ],
            color,
            egui::Stroke::NONE,
        ));
    }
```

Add hover + right-click via an `interact` over each flag's small rect (after drawing):

```rust
    for m in markers.by_time() {
        let fx = bar_x_at(m.t_us, rect, range);
        let flag_rect = egui::Rect::from_min_max(
            egui::pos2(fx - hit_radius, rect.top()),
            egui::pos2(fx + hit_radius, rect.bottom()),
        );
        let fr = ui.interact(flag_rect, ui.id().with(("flag", m.id)), egui::Sense::click());
        if fr.hovered() {
            let text = if m.note.is_empty() {
                m.label.clone()
            } else {
                format!("{}\n{}", m.label, m.note)
            };
            egui::show_tooltip_text(ui.ctx(), ui.layer_id(), ui.id().with(("flag_tip", m.id)), text);
        }
        fr.context_menu(|ui| {
            let mut label = m.label.clone();
            if ui.add(egui::TextEdit::singleline(&mut label).hint_text("label")).changed() {
                action.marker_edit = Some((m.id, MarkerEdit { label: Some(label), color: None }));
            }
            let mut color = m.color32();
            if egui::color_picker::color_edit_button_srgba(ui, &mut color, egui::color_picker::Alpha::Opaque).changed() {
                action.marker_edit = Some((
                    m.id,
                    MarkerEdit { label: None, color: Some(crate::legend::color32_to_srgb(color)) },
                ));
            }
            if ui.button("Delete").clicked() {
                action.marker_delete = Some(m.id);
                ui.close();
            }
        });
    }
    scrubbed
```

(If the existing `scrubber` already declared `let mut scrubbed` / `painter`, reuse them rather than redeclaring. Keep the function returning `scrubbed`.)

- [ ] **Step 4: Pass markers from the app and apply the actions**

In `app.rs`, update the `crate::timeline::ui(...)` call to pass `&self.markers` (add the argument in the same position as the new `ui` parameter). After the call, where `action.lock_live`/`view_changed` are handled, add:

```rust
            if let Some(t_us) = action.marker_jump {
                self.playback.scrub(t_us, range);
            }
            if let Some((id, t_us)) = action.marker_move {
                if let Some(m) = self.markers.get_mut(id) {
                    m.t_us = t_us.clamp(range.min_us, range.max_us);
                }
            }
            if let Some(id) = action.marker_delete {
                self.markers.remove(id);
            }
            if let Some((id, edit)) = action.marker_edit {
                if let Some(m) = self.markers.get_mut(id) {
                    if let Some(label) = edit.label {
                        m.label = label;
                    }
                    if let Some(color) = edit.color {
                        m.color = color;
                    }
                }
            }
```

- [ ] **Step 5: Build + clippy + fmt + commit**

```bash
cargo build -p delog-app --no-default-features
cargo fmt --all
cargo clippy -p delog-app --no-default-features -- -D warnings
git add crates/delog-app/src/timeline.rs crates/delog-app/src/app.rs
git commit -m "ANA-05: timeline marker flags (jump/drag/hover/right-click)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Plot verticals

**Files:**
- Modify: `crates/delog-app/src/hover.rs` (add `draw_session_markers`)
- Modify: `crates/delog-app/src/workspace.rs` (`PlotServices.markers`, call in `plot_body`)
- Modify: `crates/delog-app/src/app.rs` (pass the slice into `PlotServices`)

**Interfaces:**
- Consumes: `Marker` (Task 1); `PaneView`, `PlotServices` (existing).
- Produces: `pub fn draw_session_markers(ui: &egui::Ui, view: PaneView, origin_us: i64, markers: &[Marker])`.

- [ ] **Step 1: Implement `draw_session_markers` in `hover.rs`**

Add (near `draw_marker`):

```rust
/// Manual session markers (§17.4, ANA-05): a faint full-height vertical in each
/// marker's colour on every plot pane, with the label at the top. Read-only;
/// distinct from the amber playhead and the ANA-10 dashed delta cursor.
pub fn draw_session_markers(
    ui: &egui::Ui,
    view: PaneView,
    origin_us: i64,
    markers: &[crate::markers::Marker],
) {
    let rect = view.rect;
    let (x0, x1) = view.x_range;
    if x1 <= x0 {
        return;
    }
    let painter = ui.painter();
    for m in markers {
        let t_sec = ((m.t_us - origin_us) as f64 * 1e-6) as f32;
        let frac = (t_sec - x0) / (x1 - x0);
        if !(0.0..=1.0).contains(&frac) {
            continue;
        }
        let x = rect.left() + frac * rect.width();
        let color = m.color32();
        painter.vline(x, rect.y_range(), egui::Stroke::new(1.0, color.gamma_multiply(0.4)));
        painter.text(
            egui::pos2(x + 3.0, rect.top() + 2.0),
            egui::Align2::LEFT_TOP,
            &m.label,
            egui::FontId::proportional(11.0),
            color,
        );
    }
}
```

- [ ] **Step 2: Add the `PlotServices` field**

In `workspace.rs`, add to `PlotServices<'a>` (near `plot_display`):

```rust
    /// Manual session markers, drawn as faint verticals on every pane (ANA-05).
    pub markers: &'a [crate::markers::Marker],
```

- [ ] **Step 3: Call it in `plot_body`**

In `workspace.rs` `plot_body`, in the `pane_overlay` scope after the ANA-10 marker block (`let marker_deltas = …;`), add:

```rust
        hover::draw_session_markers(
            ui,
            pview,
            self.services.origin_us,
            self.services.markers,
        );
```

- [ ] **Step 4: Pass the slice from the app**

In `app.rs`, in the `PlotServices { … }` literal (near `plot_display: self.settings.plot,`), add:

```rust
                        markers: self.markers.as_slice(),
```

- [ ] **Step 5: Build + clippy + fmt + commit**

```bash
cargo build -p delog-app --no-default-features
cargo fmt --all
cargo clippy -p delog-app --no-default-features -- -D warnings
git add crates/delog-app/src/hover.rs crates/delog-app/src/workspace.rs crates/delog-app/src/app.rs
git commit -m "ANA-05: faint labelled marker verticals on plot panes

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Checklist, full verification, and manual run

**Files:**
- Modify: `PLAN.md` (ANA-05 checklist line; §17.4 if needed)

- [ ] **Step 1: Mark ANA-05 in progress→done in PLAN.md**

Change the `- [ ] **ANA-05** …` line to `- [x] **ANA-05** …` with a one-line implementation summary (data model `Markers`, `M` add-at-playhead, bottom dock, timeline flags with jump/drag/hover/right-click, faint labelled plot verticals, `LayoutDoc.markers` persistence; pure-logic unit tests + layout round-trip; GUI visuals pending manual run).

- [ ] **Step 2: Full workspace clippy + tests**

```bash
cargo fmt --all
cargo clippy -p delog-app --no-default-features -- -D warnings
cargo test -p delog-app --no-default-features
```
Expected: clippy clean; all tests pass (markers + layout round-trip included).

- [ ] **Step 3: Commit the checklist update**

```bash
git add PLAN.md
git commit -m "ANA-05: mark done in checklist

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 4: Manual in-app verification (user machine)**

Run `cargo run -p delog-app`, load a log, then:
- Press `M` at a few playhead positions → markers appear in the dock, on the timeline (flags), and as faint labelled verticals on plots.
- Click a flag → playhead jumps; drag a flag → marker moves; hover → label/note tooltip; right-click → rename/recolor/delete.
- Edit label/color/note in the dock; jump/delete from the dock.
- Save a layout / reopen → markers restored (also via 30 s session autosave).

## Self-Review notes

- **Spec coverage:** data model (T1), persistence (T2), `M` + app state (T3), dock with click-to-jump/edit/delete (T4), timeline flags click/drag/hover/right-click (T5), faint labelled plot verticals (T6), checklist + verification (T7). All §17.4 deliverables mapped.
- **Type consistency:** `Markers`/`Marker` methods (`add_at`, `push_loaded`, `get_mut`, `by_time`, `as_slice`, `color32`) used consistently in T3-T6; `MarkerLayout` fields match T2↔T3; `TimelineAction`/`MarkerEdit` fields defined in T5 and applied in the same task.
- **Placeholder scan:** every code step contains concrete code; the only adaptation notes flag where to reuse existing local variables (`rect`/`painter`/`scrubbed`) rather than inventing them.
