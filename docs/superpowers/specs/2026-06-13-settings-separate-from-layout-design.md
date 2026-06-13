# App settings persisted separately from layouts

**Date:** 2026-06-13
**Checklist ID:** `LAY-08` (amends `LAY-02`, `LAY-06`)
**Related:** `PRF-08`, `PRF-09` (the settings whose persistence this fixes)

## Problem

`AppSettings` (theme, render tuning, `show_fps`, `render_mode`) is embedded in
`LayoutDoc` (`layout.rs:38`). Consequences:

- Loading any named or imported layout overwrites `self.settings` from the
  layout's embedded copy (`app.rs:622`), changing the user's theme / FPS counter
  / render mode unexpectedly.
- Settings load from `session.json` at startup (`app.rs:124`), coupling user
  preferences to the autosaved session document.

App settings are user-global preferences, not per-layout state. They must persist
independently.

## Goal

1. Loading a layout (named, imported, or session restore) **never** mutates
   `AppSettings`.
2. App settings persist in their own `settings.json`, separate from layouts and
   from `session.json`, reusing the existing `serde_json` persistence.

## Design

### 1. `layout.rs` — remove settings from the layout model

- Remove the `settings: AppSettings` field from `LayoutDoc` (`layout.rs:38-39`),
  from `CurrentLayout` (`layout.rs:256`), and from `LayoutApply`
  (`layout.rs:210`). Remove the corresponding wiring in `current_doc`
  (`layout.rs:461`) and `apply_doc` (`layout.rs:650`).
- **No layout version bump.** Serde ignores unknown fields by default, so existing
  `session.json` and `layouts/*.json` files that still carry a `settings` key
  decode without error — the key is silently ignored, which is the desired
  behavior (their settings no longer apply).
- Add two functions, reusing the existing private `storage_dir(APP_ID)` and
  `write_json_atomic`:

```rust
/// Path to the app-wide settings file (separate from layouts; LAY-08).
fn settings_path() -> Result<PathBuf, LayoutError> {
    let Some(base) = storage_dir(APP_ID) else {
        return Err(LayoutError::NoStorageDir);
    };
    Ok(base.join("settings.json"))
}

/// Load app settings, falling back to defaults if the file is absent or
/// unreadable (e.g. first run, or written by a newer version).
pub fn load_app_settings() -> AppSettings {
    let Ok(path) = settings_path() else {
        return AppSettings::default();
    };
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_app_settings(settings: &AppSettings) -> Result<(), LayoutError> {
    let path = settings_path()?;
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| LayoutError::Json(e.to_string()))?;
    write_json_atomic(&path, &json)
}
```

`AppSettings` already derives `Serialize`/`Deserialize`, so it serializes
directly with no wrapper type.

### 2. `app.rs` — wire it up

- **Startup** (`new`, `app.rs:124-126`): replace the `load_session_doc().settings`
  read with `let settings = crate::layout::load_app_settings();`.
- **`apply_layout`** (`app.rs:622-625`): delete the settings-overwrite block
  entirely. Loading a layout no longer touches `self.settings` or
  `theme_needs_apply`.
- **`current_layout_doc`** (`app.rs:413-428`): drop the `settings` argument to
  `CurrentLayout`.
- **Persist on change**: in the `ui` method, snapshot `let before =
  self.settings.clone();` immediately before `settings_dialog.show(...)`
  (`app.rs:1343`); after the call, if `self.settings != before`, call
  `crate::layout::save_app_settings(&self.settings)` (ignoring or
  diagnostic-logging the error consistent with session-save handling).
- **`on_exit`** (`app.rs:850-853`): also `save_app_settings` as a safety net,
  alongside the existing session autosave.

### 3. Migration

None. On first run after this change, settings reset to defaults once (no seed
from the legacy `session.json` `settings` key), then persist normally via
`settings.json`.

### 4. Out of scope

- The stale `~/.config/DeLOG/DeLOG.conf` (old Qt/C++ `QSettings` file) — not read
  or written by the Rust app; user may delete it independently.
- No new persistence format/library; reuses `serde_json`.

## Testing

`layout.rs`:
- Update the `empty_doc` test helper (remove the `settings` field) and rework/
  remove the three settings-coupled layout tests
  (`export_import_doc_round_trips_through_json_file`'s settings assertion,
  `missing_settings_default_to_catppuccin_mocha`, `layout_json_persists_theme_settings`).
- New: a layout JSON that still contains a legacy `settings` key decodes
  successfully (unknown-field tolerance) and `LayoutApply` carries no settings.

`settings` persistence (in `layout.rs` tests, next to the session tests):
- `save_app_settings` → `load_app_settings` round-trips a non-default
  `AppSettings` (e.g. `show_fps = true`, `render_mode = Continuous`, a non-default
  theme) through a temp file.
- `load_app_settings` returns `AppSettings::default()` when the file is absent.

`app.rs` behavior (loading a layout doesn't change settings) is enforced largely
at the type level (no `settings` on `LayoutApply`) plus the manual GUI-run note.

## Checklist updates (PLAN.md §22)

- Add `LAY-08` — App settings persisted separately (`settings.json`); loading a
  layout never mutates app settings.
- Amend `LAY-02` and `LAY-06` descriptions: settings/theme are no longer stored in
  the layout document or `session.json`; they live in `settings.json` (LAY-08).
