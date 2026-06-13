# Settings: FPS counter toggle + render mode

**Date:** 2026-06-13
**Checklist IDs:** `PRF-08` (FPS-counter visibility toggle), `PRF-09` (Reactive/Continuous render-mode setting)
**Related:** `PRF-05` (idle-aware FPS badge), `TLN-06` (idle-aware repaint policy, §11)

## Goal

Add two user-facing settings to the existing Settings dialog:

1. **Show FPS counter** — enable/disable the corner FPS badge. Default **off**.
2. **Render mode** — `Reactive` (event-driven, idles at 0% GPU per §11) or
   `Continuous` (repaint every frame). Default **Reactive**.

## Current state

- `crates/delog-app/src/settings.rs` defines `AppSettings { theme, render }` and
  `SettingsDialog` with two tabs (`General`, `Rendering`). `AppSettings` is
  serde-serialized and persisted in the session config.
- `crates/delog-app/src/app.rs`:
  - The FPS badge (PRF-05) is **always** drawn at the right of the menu bar
    (~line 1013), showing the EMA rate or `"idle"`.
  - The repaint policy (TLN-06, ~line 912) is:
    `if self.playback.playing || self.session.has_connected_live() { request_repaint() }`.

## Design

### 1. `AppSettings` fields (settings.rs)

Add two persisted fields with serde defaults so existing configs load unchanged:

```rust
#[serde(default)]
pub show_fps: bool,          // default false (bool::default)
#[serde(default)]
pub render_mode: RenderMode, // default Reactive
```

New enum, derived `Default = Reactive`, serde-serializable:

```rust
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderMode {
    #[default]
    Reactive,
    Continuous,
}
```

with `ALL: [Self; 2]` and a `label()` for the ComboBox, matching the existing
`ThemeChoice` pattern.

### 2. General tab UI (settings.rs)

Two new rows in `general_tab`'s grid, below Theme:

- **Show FPS counter** → `ui.checkbox(&mut settings.show_fps, "")`.
  Hover: "Show a frame-rate badge in the top-right corner."
- **Render mode** → ComboBox over `RenderMode::ALL`.
  Hover on Reactive/Continuous explaining: Reactive = event-driven, idles at
  0% GPU (§11); Continuous = repaints every frame (higher GPU, smoother for
  debugging).

Neither requires a `SettingsChange` flag (theme is the only field needing a
post-change side effect today); both are read live from `self.settings`.

### 3. FPS badge gate (app.rs)

Wrap the entire right-to-left badge block in `if self.settings.show_fps { … }`
so nothing (not even `"idle"`) is drawn when disabled.

### 4. Repaint policy (app.rs)

```rust
if self.settings.render_mode == RenderMode::Continuous
    || self.playback.playing
    || self.session.has_connected_live()
{
    ui.ctx().request_repaint();
}
```

The existing comment block is updated to note the Continuous override.

## Testing

Unit tests in `settings.rs`:

- `AppSettings::default()` has `show_fps == false` and `render_mode == Reactive`.
- An older config JSON without the new fields deserializes to those defaults.
- `RenderMode::ALL` labels are stable (navigation/serialization safety),
  mirroring the existing `settings_tabs_are_named_for_stable_navigation` test.

The two `app.rs` changes are one-line gates; behavior is covered by the existing
manual GUI-run note (see memory: GUI changes need a manual run on the user's box).

## Out of scope

- F12 debug overlay (PRF-06), perf dock (PRF-02..04), profiling export (PRF-07).
- No new tab; both controls live in the existing General tab.
