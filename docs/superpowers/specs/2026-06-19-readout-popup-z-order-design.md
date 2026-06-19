# Readout Popup Z-Order

**Checklist:** PLT-16

## Goal

Keep plot hover and playhead value readouts visible over plot content while
ensuring every floating window paints above them.

## Root Cause

The shared value readout in `hover::show_tooltip` is an `egui::Area` using
`egui::Order::Tooltip`. Floating `egui::Window` instances use
`egui::Order::Middle`. Egui paints layer categories in order, with `Tooltip`
after `Middle`, so the readout necessarily covers windows when they overlap.

## Design

Introduce a private `READOUT_ORDER` constant set to `egui::Order::Background`
and use it for the shared readout area. Egui defines `Background` as painting
behind all floating windows. The readout is still emitted after the plot's GPU
and egui content, so it remains visible over the plot at its current position.

The shared `show_tooltip` function serves both pointer-hover and playhead value
readouts, so one change gives both overlays the same correct policy. Plot cursor
lines and sample circles already use the plot UI's base painter and require no
change. Menus and genuine egui tooltips remain on their existing higher layers.

## Validation

- Add a unit test asserting `READOUT_ORDER == egui::Order::Background`; observe
  it fail while the constant still reflects the current Tooltip behavior.
- Run the focused hover test, all `delog-app` tests, workspace check, and
  warning-denied clippy.
- When a GUI environment is available, overlap a Settings or Manage Layouts
  window with an active plot readout and verify the window fully occludes it.

No data flow, hot path, dependency direction, or zero-copy invariant changes,
so no benchmark is required.
