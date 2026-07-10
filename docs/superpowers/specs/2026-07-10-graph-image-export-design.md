# Graph Image Export Design

## Goal

Add PNG image export and image clipboard copy for rendered graph content.

The feature has two user-facing entry points:

- A toolbar button exports the visible workspace content area as PNG, including plots and the 3D pane, while excluding the browser, menu, toolbar, timeline, and dock panels.
- Each plot pane context menu offers `Export PNG...` and `Copy Image` actions for that plot only.

## Rendering Approach

Use egui's native screenshot request and response flow:

1. Record the target UI rectangle in logical points when the user clicks an export or copy action.
2. Send `egui::ViewportCommand::Screenshot` with typed request metadata.
3. On the returned `egui::Event::Screenshot`, crop the `ColorImage` to the recorded rectangle using the frame's pixels-per-point.
4. For export, encode the cropped image as PNG and write it to the selected path.
5. For copy, send `egui::OutputCommand::CopyImage` with the cropped image.

This captures the composited frame exactly as shown, including egui axes, legends, markers, readouts, WGPU plot traces, and the resolved 3D scene texture. The exported image uses native physical pixels for the current window DPI. It does not synthesize a larger offscreen render size because that would require a separate full renderer for egui overlays, WGPU callbacks, and 3D content.

## UI Placement

Add a compact icon button to the existing toolbar for `Export Workspace PNG...`.

Add two entries to each plot pane's right-click context menu:

- `Copy Image`
- `Export PNG...`

The toolbar action uses a save dialog before requesting the screenshot. The plot export action does the same. The plot copy action requests a screenshot immediately and copies the cropped plot when the screenshot event arrives.

## State And Data Flow

Introduce a small pending image export state in the app layer:

- request id
- action: export path or copy image
- target: workspace rect or plot rect
- pixels-per-point at request time

Workspace rendering should report the visible workspace rect to the app each frame. Plot context actions should report the selected plot rect and desired image action through `WorkspaceActions`.

When a screenshot event arrives, ignore it unless its request id matches the pending state. Clamp the crop rectangle to the screenshot bounds before cropping so stale or slightly changed layouts fail gracefully instead of panicking.

## Errors And Feedback

If the save dialog is cancelled, do nothing.

If PNG writing or clipboard copy setup fails, emit a diagnostic/log status using existing app patterns. On successful export or copy, report a concise status message.

If no valid workspace or plot rectangle is available, disable the related action or report that there is nothing to export.

## Testing

Unit-test the pure crop-rectangle conversion and clamping logic.

Unit-test PNG encoding from an `egui::ColorImage` or equivalent RGBA buffer.

Add focused workspace/app tests where practical for action plumbing:

- plot context menu actions produce the expected pending image request action
- workspace rect reporting excludes non-workspace UI by being sourced from the workspace panel, not the full viewport

Manual verification should cover:

- toolbar export includes visible plot panes and 3D pane, excluding browser/menu/toolbar/timeline/docks
- plot export crops only the chosen plot
- plot copy places an image on the OS clipboard
