# App Settings Persisted Separately From Layouts — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `AppSettings` out of `LayoutDoc` into its own `settings.json` so loading a layout never changes theme / FPS counter / render mode, and user preferences persist independently.

**Architecture:** Remove the `settings` field from the layout model (`LayoutDoc`, `CurrentLayout`, `LayoutApply`) and add `load_app_settings()` / `save_app_settings()` to `layout.rs` that read/write `storage_dir/settings.json` via the existing `serde_json` + atomic-write helpers. `app.rs` loads settings at startup, persists them when the dialog changes them and on exit, and no longer applies settings during `apply_layout`. Existing layout JSON keeps decoding because serde ignores the now-unknown `settings` key.

**Tech Stack:** Rust, serde / serde_json, egui/eframe.

---

### Task 1: Add `settings.json` persistence functions to `layout.rs`

**Files:**
- Modify: `crates/delog-app/src/layout.rs`
- Test: `crates/delog-app/src/layout.rs` (inline `#[cfg(test)] mod tests`)

The public functions (`load_app_settings`/`save_app_settings`) resolve the path
via `storage_dir`, which reads env vars — not test-friendly. So the real logic
lives in path-taking helpers (`load_app_settings_at`/`save_app_settings_at`) that
the public wrappers delegate to, and the tests exercise the helpers with a temp
path.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `layout.rs` (it already has `use super::*;`):

```rust
    #[test]
    fn app_settings_round_trip_through_settings_json() {
        let path = std::env::temp_dir().join(format!(
            "delog-settings-rt-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("settings")
        ));
        let mut settings = AppSettings::default();
        settings.show_fps = true;
        settings.render_mode = crate::settings::RenderMode::Continuous;
        settings.theme = crate::theme::ThemeChoice::Light;

        save_app_settings_at(&path, &settings).expect("save settings");
        let loaded = load_app_settings_at(&path);
        let _ = fs::remove_file(&path);

        assert_eq!(loaded, settings);
    }

    #[test]
    fn load_app_settings_defaults_when_file_missing() {
        let missing = std::env::temp_dir().join(format!(
            "delog-settings-missing-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("settings")
        ));
        let _ = fs::remove_file(&missing); // ensure absent
        assert_eq!(load_app_settings_at(&missing), AppSettings::default());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p delog-app layout::tests 2>&1 | tail -20`
Expected: FAIL to compile — `save_app_settings_at` / `load_app_settings_at` are not defined yet.

- [ ] **Step 3: Add the persistence functions**

In `layout.rs`, after the existing `load_session_doc` function (around line 377), add:

```rust
/// Path to the app-wide settings file (LAY-08). Separate from layouts and from
/// `session.json` so loading a layout never changes user preferences.
fn settings_path() -> Result<PathBuf, LayoutError> {
    let Some(base) = storage_dir(APP_ID) else {
        return Err(LayoutError::NoStorageDir);
    };
    Ok(base.join("settings.json"))
}

/// Load app settings, falling back to defaults if the file is absent or
/// unreadable (first run, or written by a newer version).
pub fn load_app_settings() -> AppSettings {
    match settings_path() {
        Ok(path) => load_app_settings_at(&path),
        Err(_) => AppSettings::default(),
    }
}

/// Persist app settings to `settings.json` atomically.
pub fn save_app_settings(settings: &AppSettings) -> Result<(), LayoutError> {
    save_app_settings_at(&settings_path()?, settings)
}

/// Read app settings from an explicit path, defaulting on any failure.
fn load_app_settings_at(path: &Path) -> AppSettings {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write app settings to an explicit path atomically.
fn save_app_settings_at(path: &Path, settings: &AppSettings) -> Result<(), LayoutError> {
    let json =
        serde_json::to_string_pretty(settings).map_err(|e| LayoutError::Json(e.to_string()))?;
    write_json_atomic(path, &json)
}
```

`AppSettings` already derives `Serialize`/`Deserialize`, so it serializes directly.

- [ ] **Step 4: Run tests to verify they pass + clippy**

Run: `cargo test -p delog-app layout::tests 2>&1 | tail -20`
Expected: PASS (the two new tests green, existing layout tests still green).
Run: `cargo clippy -p delog-app -- -D warnings 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/delog-app/src/layout.rs
git commit -m "Add settings.json persistence functions (LAY-08)"
```

---

### Task 2: Remove `settings` from the layout model

**Files:**
- Modify: `crates/delog-app/src/layout.rs`

This task removes the `settings` field from three structs and their wiring, then
fixes the now-broken tests. Existing on-disk JSON keeps decoding because serde
ignores unknown fields.

- [ ] **Step 1: Remove the field from the structs and wiring**

1. `LayoutDoc` (around line 38-39) — delete:
```rust
    #[serde(default)]
    pub settings: AppSettings,
```

2. `LayoutApply` (around line 210) — delete the `pub settings: AppSettings,` line.

3. `CurrentLayout` (around line 256) — delete the `pub settings: &'a AppSettings,` line.

4. `current_doc` (around line 461) — delete `settings: input.settings.clone(),` from the `LayoutDoc { ... }` literal.

5. `apply_doc` (around line 650) — delete `settings: doc.settings,` from the `LayoutApply { ... }` literal.

6. Remove the now-unused import if the compiler flags it: `use crate::settings::AppSettings;` (line 19) — but Task 1's functions still use `AppSettings`, so **keep** this import. Verify with the build in Step 3.

- [ ] **Step 2: Fix the broken tests in `layout.rs`**

a. `empty_doc` helper (around line 1333) — delete the `settings: AppSettings::default(),` line from the `LayoutDoc { ... }` literal.

b. Delete the test `missing_settings_default_to_catppuccin_mocha` entirely (it asserted `doc.settings`, which no longer exists).

c. Delete the test `layout_json_persists_theme_settings` entirely (asserted layout JSON contains `settings`/`theme`; settings are no longer in the layout).

d. In `export_import_doc_round_trips_through_json_file`, delete the settings assertion block:
```rust
        assert_eq!(
            imported.settings.theme,
            crate::theme::ThemeChoice::CatppuccinMocha
        );
```
(keep the rest of the test — version, name, and the no-`source` JSON assertion).

- [ ] **Step 3: Add a test that legacy JSON with a `settings` key still decodes**

Add to `mod tests`:

```rust
    #[test]
    fn legacy_layout_with_settings_key_still_decodes_ignoring_it() {
        // Layouts written before LAY-08 embedded a `settings` object. Decoding
        // must succeed and simply ignore the unknown key (serde default).
        let doc = decode_doc(
            r#"{
                "delog_layout": 1,
                "name": "legacy",
                "playback": {"speed": 1.0, "follow_live": false},
                "workspace": {
                    "root": {
                        "plot": {"traces": [], "show_legend": true, "show_tooltip": true}
                    }
                },
                "vehicles": [],
                "settings": {"theme": "light", "show_fps": true, "render_mode": "continuous"}
            }"#,
        )
        .expect("legacy layout with settings key should decode");
        assert_eq!(doc.name, "legacy");
    }
```

- [ ] **Step 4: Build, test, clippy**

Run: `cargo test -p delog-app layout 2>&1 | tail -25`
Expected: all layout tests PASS (the two deleted tests are gone; the new legacy test passes).
Run: `cargo clippy -p delog-app -- -D warnings 2>&1 | tail -10`
Expected: clean (no unused-import warning for `AppSettings`, since Task 1 functions use it).

> Note: `app.rs` will NOT compile yet — it still passes `settings:` to `CurrentLayout` and reads `layout.settings`. That is fixed in Task 3. If you want a green build at this checkpoint, do Task 2 and Task 3 before running a full `cargo build`; `cargo test -p delog-app layout` compiles the test target which includes `app.rs`, so it may fail to compile until Task 3. In that case, proceed to Task 3 and run the combined build there. Commit this task's source changes regardless once Task 3 compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/delog-app/src/layout.rs
git commit -m "Remove settings from layout model; keep legacy-JSON tolerance (LAY-08)"
```

---

### Task 3: Wire `app.rs` to the separate settings store

**Files:**
- Modify: `crates/delog-app/src/app.rs`

- [ ] **Step 1: Load settings from the new store at startup**

In `DelogApp::new` (around line 124-126), replace:
```rust
        let settings = crate::layout::load_session_doc()
            .map(|doc| doc.settings)
            .unwrap_or_default();
```
with:
```rust
        let settings = crate::layout::load_app_settings();
```

- [ ] **Step 2: Stop `apply_layout` from overwriting settings**

In `apply_layout` (around line 622-625), delete the block:
```rust
        if self.settings != layout.settings {
            self.settings = layout.settings;
            self.theme_needs_apply = true;
        }
```
Leave the rest of `apply_layout` unchanged. (`theme_needs_apply` is still used by
the settings dialog path, so keep the field.)

- [ ] **Step 3: Drop the `settings` argument from `current_layout_doc`**

In `current_layout_doc` (around line 418-427), delete the line:
```rust
            settings: &self.settings,
```
from the `CurrentLayout { ... }` literal.

- [ ] **Step 4: Persist settings when the dialog changes them**

In the `ui` method, locate (around line 1343):
```rust
        let settings_change = self.settings_dialog.show(ui.ctx(), &mut self.settings);
        if settings_change.theme_changed || self.theme_needs_apply {
            self.settings.theme.apply(ui.ctx());
            self.theme_needs_apply = false;
        }
```
Replace with:
```rust
        let settings_before = self.settings.clone();
        let settings_change = self.settings_dialog.show(ui.ctx(), &mut self.settings);
        if settings_change.theme_changed || self.theme_needs_apply {
            self.settings.theme.apply(ui.ctx());
            self.theme_needs_apply = false;
        }
        if self.settings != settings_before {
            if let Err(err) = crate::layout::save_app_settings(&self.settings) {
                self.session
                    .push_diagnostic(delog_core::diagnostics::Diag::error(
                        "settings-save",
                        err.to_string(),
                    ));
            }
        }
```

- [ ] **Step 5: Save settings on exit**

In `on_exit` (around line 850-853), after the existing autosave line, add a
settings save:
```rust
    fn on_exit(&mut self) {
        let snapshot = self.session.snapshot();
        let _ = self.autosave_session(&snapshot, true);
        let _ = crate::layout::save_app_settings(&self.settings);
    }
```

- [ ] **Step 6: Build, test, clippy (full workspace)**

Run: `cargo build -p delog-app 2>&1 | tail -15`
Expected: compiles cleanly.
Run: `cargo test -p delog-app 2>&1 | tail -15`
Expected: all tests PASS.
Run: `cargo clippy --workspace -- -D warnings 2>&1 | tail -15`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/delog-app/src/app.rs
git commit -m "Persist app settings via settings.json; layouts no longer apply settings (LAY-08)"
```

---

### Task 4: Update the PLAN.md checklist

**Files:**
- Modify: `PLAN.md` (§22, after `LAY-07` at line ~969; amend `LAY-02` line ~964 and `LAY-06` line ~968)

- [ ] **Step 1: Add LAY-08**

After the `LAY-07` checklist line, add:

```markdown
- [x] **LAY-08** — App settings (theme, render tuning, `show_fps`, `render_mode`) persisted in their own `settings.json`, separate from layouts and `session.json`; loading any layout (named/imported/session) never mutates app settings. `AppSettings` removed from `LayoutDoc`/`LayoutApply`/`CurrentLayout`; legacy layout JSON with a `settings` key still decodes (the key is ignored)
```

- [ ] **Step 2: Amend LAY-02 wording**

In the `LAY-02` line, change the trailing clause `vehicle configs, 3D camera/tracked vehicle, and app settings including theme.` to:
`vehicle configs, 3D camera/tracked vehicle. App settings (incl. theme) are persisted separately in settings.json (LAY-08), not in the layout.`

- [ ] **Step 3: Amend LAY-06 wording**

In the `LAY-06` line, change `the current source-agnostic layout document, including settings/theme, to the app data session.json` to `the current source-agnostic layout document to the app data session.json` and change the trailing `startup restores the saved theme setting` to `app settings/theme are persisted separately (LAY-08), not in session.json`.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p delog-app 2>&1 | tail -5`
Expected: PASS (sanity that nothing regressed).

```bash
git add PLAN.md
git commit -m "Mark LAY-08 done; amend LAY-02/LAY-06 for separate settings (LAY-08)"
```

---

## Notes for the implementer

- Do NOT bump `LAYOUT_VERSION`. Removing the `settings` field relies on serde's
  default behavior of ignoring unknown fields, so existing v1 files
  (`session.json`, `layouts/*.json`) continue to decode. The
  `legacy_layout_with_settings_key_still_decodes_ignoring_it` test pins this.
- `delog-app` is a **binary** crate — there is no `--lib` target. Use
  `cargo test -p delog-app <filter>` (not `--lib`).
- GUI behavior (loading a layout leaves theme/FPS/render-mode untouched; settings
  survive a restart) needs a manual `cargo run -p delog-app` on the user's
  machine, per the standing GUI-run note.
- Keep the `use crate::settings::AppSettings;` import in `layout.rs` — Task 1's
  persistence functions still reference `AppSettings`.
