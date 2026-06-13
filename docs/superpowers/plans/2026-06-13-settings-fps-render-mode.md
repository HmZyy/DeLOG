# Settings: FPS Counter Toggle + Render Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two persisted user settings — an FPS-counter visibility toggle (default off) and a Reactive/Continuous render mode (default Reactive) — surfaced in the existing Settings dialog's General tab.

**Architecture:** Extend `AppSettings` in `delog-app/src/settings.rs` with a `show_fps: bool` and a new `RenderMode` enum, both serde-defaulted so old configs load unchanged. Surface them as two new rows in the General tab. In `app.rs`, gate the FPS badge on `show_fps` and extend the repaint condition to force continuous repaints when `render_mode == Continuous`.

**Tech Stack:** Rust, egui/eframe, serde. Mirrors the existing `ThemeChoice` enum pattern (`ALL` + `label()`).

---

### Task 1: Add `RenderMode` enum + `AppSettings` fields with tests

**Files:**
- Modify: `crates/delog-app/src/settings.rs`
- Test: `crates/delog-app/src/settings.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block at the bottom of `settings.rs`:

```rust
    #[test]
    fn default_settings_hide_fps_and_render_reactively() {
        let s = AppSettings::default();
        assert!(!s.show_fps);
        assert_eq!(s.render_mode, RenderMode::Reactive);
    }

    #[test]
    fn old_config_without_new_fields_uses_defaults() {
        // A config written before these fields existed.
        let json = r#"{"theme":"CatppuccinMocha"}"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert!(!s.show_fps);
        assert_eq!(s.render_mode, RenderMode::Reactive);
    }

    #[test]
    fn render_mode_labels_are_stable() {
        let labels: Vec<_> = RenderMode::ALL.into_iter().map(RenderMode::label).collect();
        assert_eq!(labels, ["Reactive", "Continuous"]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p delog-app --lib settings::tests 2>&1 | tail -20`
Expected: FAIL — `RenderMode` not found, `show_fps` not a field.

- [ ] **Step 3: Add the enum and fields**

In `settings.rs`, add the enum after the `RenderTuning` impl block (before `SettingsTab`):

```rust
/// Frame-pacing policy (PRF-09). `Reactive` is event-driven and idles at 0% GPU
/// (§11 / TLN-06); `Continuous` repaints every frame regardless of activity.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RenderMode {
    #[default]
    Reactive,
    Continuous,
}

impl RenderMode {
    pub const ALL: [Self; 2] = [Self::Reactive, Self::Continuous];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Reactive => "Reactive",
            Self::Continuous => "Continuous",
        }
    }
}
```

Add the two fields to `AppSettings` (after `render`):

```rust
    /// Show the corner FPS badge (PRF-08). Default off.
    #[serde(default)]
    pub show_fps: bool,
    /// Frame-pacing policy (PRF-09). Default `Reactive`.
    #[serde(default)]
    pub render_mode: RenderMode,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p delog-app --lib settings::tests 2>&1 | tail -20`
Expected: PASS (all settings tests green).

- [ ] **Step 5: Commit**

```bash
git add crates/delog-app/src/settings.rs
git commit -m "Add show_fps + RenderMode settings (PRF-08, PRF-09)"
```

---

### Task 2: Surface both controls in the General tab

**Files:**
- Modify: `crates/delog-app/src/settings.rs` (`general_tab` function)

- [ ] **Step 1: Add the two grid rows**

In `general_tab`, inside the `Grid::show` closure, after the existing Theme `ui.end_row();` and before the closure ends, add:

```rust
            ui.label("Show FPS counter")
                .on_hover_text("Show a frame-rate badge in the top-right corner.");
            ui.checkbox(&mut settings.show_fps, "");
            ui.end_row();

            ui.label("Render mode")
                .on_hover_text(
                    "Reactive: event-driven, idles at 0% GPU when nothing changes. \
                     Continuous: repaints every frame (smoother for debugging, higher GPU).",
                );
            egui::ComboBox::from_id_salt("settings-render-mode")
                .selected_text(settings.render_mode.label())
                .show_ui(ui, |ui| {
                    for mode in RenderMode::ALL {
                        ui.selectable_value(&mut settings.render_mode, mode, mode.label());
                    }
                });
            ui.end_row();
```

These are read live from `self.settings`; no `SettingsChange` flag is needed
(only `theme` requires a post-change side effect).

- [ ] **Step 2: Verify it compiles + clippy clean**

Run: `cargo clippy -p delog-app -- -D warnings 2>&1 | tail -20`
Expected: no warnings/errors.

- [ ] **Step 3: Commit**

```bash
git add crates/delog-app/src/settings.rs
git commit -m "Surface FPS toggle + render mode in General tab"
```

---

### Task 3: Gate the FPS badge on `show_fps` (app.rs)

**Files:**
- Modify: `crates/delog-app/src/app.rs` (FPS badge block, ~line 1013–1031)

- [ ] **Step 1: Wrap the badge block**

The current block is:

```rust
                // FPS badge pinned to the far right (PRF-05).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match self.fps_ema {
                        Some(fps) => {
                            // ... colored_label ...
                        }
                        None => {
                            ui.weak("idle");
                        }
                    }
                });
```

Wrap the entire `ui.with_layout(...)` call in a `show_fps` guard so nothing
(not even `"idle"`) is drawn when disabled:

```rust
                // FPS badge pinned to the far right (PRF-05), shown only when
                // enabled in settings (PRF-08).
                if self.settings.show_fps {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        match self.fps_ema {
                            Some(fps) => {
                                // ... keep the existing colored_label body verbatim ...
                            }
                            None => {
                                ui.weak("idle");
                            }
                        }
                    });
                }
```

Keep the inner body exactly as-is — only add the `if self.settings.show_fps {` /
closing `}` around the `ui.with_layout` call.

- [ ] **Step 2: Verify it compiles + clippy clean**

Run: `cargo clippy -p delog-app -- -D warnings 2>&1 | tail -20`
Expected: no warnings/errors.

- [ ] **Step 3: Commit**

```bash
git add crates/delog-app/src/app.rs
git commit -m "Gate FPS badge on show_fps setting (PRF-08)"
```

---

### Task 4: Honor `render_mode` in the repaint policy (app.rs)

**Files:**
- Modify: `crates/delog-app/src/app.rs` (repaint policy, ~line 907–914)

- [ ] **Step 1: Extend the repaint condition**

The current block is:

```rust
            // Idle-aware repaint policy (§11, TLN-06): continuous frames only
            // while playing (later: or a link is Connected, M7). Everything
            // else is event-driven — ingest progress, epoch changes and
            // diagnostics each request their own repaint — so a static plot
            // idles at 0% GPU.
            if self.playback.playing || self.session.has_connected_live() {
                ui.ctx().request_repaint();
            }
```

Replace the comment + condition with (note: this lives in the `if let Some(range)`
branch — also update the `else` empty-session branch the same way if it repaints;
it does not, so only this branch changes):

```rust
            // Idle-aware repaint policy (§11, TLN-06): continuous frames only
            // while playing (later: or a link is Connected, M7). Everything
            // else is event-driven — ingest progress, epoch changes and
            // diagnostics each request their own repaint — so a static plot
            // idles at 0% GPU. The Continuous render mode (PRF-09) overrides
            // this and forces a repaint every frame.
            if self.settings.render_mode == crate::settings::RenderMode::Continuous
                || self.playback.playing
                || self.session.has_connected_live()
            {
                ui.ctx().request_repaint();
            }
```

- [ ] **Step 2: Handle the empty-session branch**

The empty-session `else` branch (~line 915) does not currently request a repaint.
In Continuous mode the app should still repaint continuously even with no log
loaded. After the `if let Some(range) ... else { ... }` block closes, add a
fallback so Continuous always repaints regardless of branch. Locate the line
`self.caches.begin_frame(self.frame);` (just after the if/else) and insert
immediately before it:

```rust
        if self.settings.render_mode == crate::settings::RenderMode::Continuous {
            ui.ctx().request_repaint();
        }
```

Then revert the Step 1 condition to NOT include the Continuous check (to avoid a
redundant double `request_repaint` in the data branch). Final Step 1 condition
stays as the original:

```rust
            if self.playback.playing || self.session.has_connected_live() {
                ui.ctx().request_repaint();
            }
```

> Rationale: a single Continuous-mode repaint placed after the if/else covers
> both the data and empty-session branches with no duplication. The comment from
> Step 1 still documents the override.

- [ ] **Step 3: Verify it compiles + clippy clean**

Run: `cargo clippy -p delog-app -- -D warnings 2>&1 | tail -20`
Expected: no warnings/errors.

- [ ] **Step 4: Commit**

```bash
git add crates/delog-app/src/app.rs
git commit -m "Honor Continuous render mode in repaint policy (PRF-09)"
```

---

### Task 5: Update the PLAN.md checklist

**Files:**
- Modify: `PLAN.md` (§22 checklist, perf area near PRF-05..PRF-07)

- [ ] **Step 1: Add the two new checklist items**

After the `PRF-07` line (`- [ ] **PRF-07** — Export profiling snapshot JSON`),
add:

```markdown
- [x] **PRF-08** — FPS-counter visibility toggle (default off) — `AppSettings.show_fps`, surfaced in the Settings → General tab; gates the corner FPS badge (extends PRF-05)
- [x] **PRF-09** — Reactive/Continuous render mode (default Reactive) — `AppSettings.render_mode`; `Continuous` overrides the §11 idle policy (TLN-06) to repaint every frame
```

- [ ] **Step 2: Verify the full workspace builds, tests, and is clippy-clean**

Run: `cargo clippy --workspace -- -D warnings && cargo test -p delog-app --lib settings 2>&1 | tail -20`
Expected: clippy clean; settings tests PASS.

- [ ] **Step 3: Commit**

```bash
git add PLAN.md
git commit -m "Mark PRF-08, PRF-09 done in checklist"
```

---

## Notes for the implementer

- `serde_json` is a regular dependency of `delog-app`
  (`crates/delog-app/Cargo.toml:29`), so the Task 1 test compiles as-is.
- GUI behavior (badge hidden when off; smooth continuous repaint) needs a manual
  run on the user's machine — per the standing note that GUI changes require a
  manual run. Suggest `cargo run -p delog-app` after the code lands.
