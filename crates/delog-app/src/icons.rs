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
