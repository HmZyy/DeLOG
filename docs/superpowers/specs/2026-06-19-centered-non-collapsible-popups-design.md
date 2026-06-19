# Centered Non-Collapsible Popups

**Checklist:** UIX-14

## Goal

Make every floating `egui::Window` in `delog-app` non-collapsible and place it in
the center of the viewport when it first appears. After spawning, each window
must remain freely draggable and may reuse egui's remembered position when it is
reopened.

## Scope

The behavior applies to all floating windows, including transient dialogs and
persistent utility windows:

- time offset and marker generation;
- plot information and MAVLink connection;
- settings, scripts, and script deletion confirmation;
- save, load, manage, and map-layout dialogs;
- source metadata and field statistics; and
- vehicle configuration.

Dock panels, menus, tooltips, context menus, native file dialogs, and anchored
overlays are not `egui::Window` popups and are outside this change.

## Design

Each `egui::Window` builder will explicitly include:

```rust
.collapsible(false)
.default_pos(ctx.screen_rect().center())
.pivot(egui::Align2::CENTER_CENTER)
```

`default_pos` supplies only the initial position. The center pivot makes that
position refer to the window's center rather than its top-left corner. Egui's
persistent window state then controls subsequent frames, preserving dragging
and remembered positions. Existing IDs, open state, sizes, resizability, and
window contents remain unchanged.

The builder calls stay explicit at each window definition. A helper abstraction
would add indirection without reducing behavioral complexity and would obscure
per-window builder configuration.

## Validation

- Search all Rust sources under `crates/delog-app/src` to ensure every
  `egui::Window` has the three required builder settings.
- Run `cargo fmt --all`.
- Run focused `delog-app` tests, workspace compilation, and
  `cargo clippy --workspace -- -D warnings`.
- Manually verify representative resizable and fixed-size windows when a GUI
  environment is available: first spawn is centered, no collapse control is
  shown, and dragging remains persistent.

No sample data, hot path, dependency direction, or zero-copy invariant is
affected, so no benchmark is required.
