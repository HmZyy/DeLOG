//! Bundled SVG icons (Lucide, ISC License) used by the toolbars and overlays.
//!
//! The SVGs are embedded at compile time via [`egui::include_image!`] and
//! rendered by the `egui_extras` SVG loader (installed in `main`). Each icon
//! is authored with a white stroke so egui's multiply tint colors it at
//! runtime: `white * tint == tint`.

use egui::ImageSource;

/// Streaming / live-telemetry link (opens the connection dialog).
pub fn satellite_dish() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/satellite-dish.svg")
}

/// 3D scene view toggle.
pub fn cube() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/box.svg")
}

/// Settings gear (opens the vehicle configuration dialog).
pub fn gear() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/settings.svg")
}

/// Trash (clear all traces / remove vehicle).
pub fn trash() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/trash-2.svg")
}

/// Plus (add vehicle).
pub fn plus() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/plus.svg")
}

/// Ban / prohibited (remove a single trace).
pub fn ban() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/ban.svg")
}

/// Pencil (edit trace style).
pub fn pencil() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/pencil.svg")
}

/// Maximize / fit-to-view (timeline auto-zoom toggle).
pub fn maximize() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/maximize.svg")
}

/// Play (transport and script run).
pub fn play() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/play.svg")
}

/// Pause transport playback.
pub fn pause() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/pause.svg")
}

/// Jump to the start of the timeline.
pub fn skip_back() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/skip-back.svg")
}

/// Jump to the end of the timeline.
pub fn skip_forward() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/skip-forward.svg")
}

/// Square (stop / cancel a running script). Used only by the scripting Console.
#[cfg_attr(not(feature = "scripting"), allow(dead_code))]
pub fn square() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/square.svg")
}

/// Save (write the editor buffer to the script library). Scripting Console only.
#[cfg_attr(not(feature = "scripting"), allow(dead_code))]
pub fn save() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/save.svg")
}

/// Unplug (unregister the current script's live transform). Scripting Console only.
#[cfg_attr(not(feature = "scripting"), allow(dead_code))]
pub fn unplug() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/unplug.svg")
}

/// Two columns (split into side-by-side panes — horizontal split).
pub fn columns() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/columns-2.svg")
}

/// Two rows (split into stacked panes — vertical split).
pub fn rows() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/rows-2.svg")
}

/// Info (open the Plot Info window).
pub fn info() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/info.svg")
}

/// Clock (source time offset editor).
pub fn clock() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/clock-3.svg")
}

/// X (close the pane).
pub fn close() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/x.svg")
}

/// Hide the left data browser panel.
pub fn panel_left_close() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/panel-left-close.svg")
}

/// Restore the left data browser panel.
pub fn panel_left_open() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/panel-left-open.svg")
}
