//! Bundled SVG icons (Lucide, ISC License).
//!
//! Each icon is authored with a white stroke so egui's multiply tint colors it
//! at runtime: `white * tint == tint`.

use egui::ImageSource;

pub fn satellite_dish() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/satellite-dish.svg")
}

pub fn cube() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/box.svg")
}

pub fn gear() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/settings.svg")
}

pub fn route() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/route.svg")
}

pub fn trash() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/trash-2.svg")
}

pub fn plus() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/plus.svg")
}

pub fn copy() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/copy.svg")
}

pub fn ban() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/ban.svg")
}

pub fn pencil() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/pencil.svg")
}

pub fn maximize() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/maximize.svg")
}

pub fn play() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/play.svg")
}

pub fn pause() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/pause.svg")
}

pub fn skip_back() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/skip-back.svg")
}

pub fn skip_forward() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/skip-forward.svg")
}

#[cfg_attr(not(feature = "scripting"), allow(dead_code))]
pub fn square() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/square.svg")
}

#[cfg_attr(not(feature = "scripting"), allow(dead_code))]
pub fn save() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/save.svg")
}

#[cfg_attr(not(feature = "scripting"), allow(dead_code))]
pub fn unplug() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/unplug.svg")
}

pub fn columns() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/columns-2.svg")
}

pub fn rows() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/rows-2.svg")
}

pub fn info() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/info.svg")
}

pub fn clock() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/clock-3.svg")
}

pub fn ruler() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/ruler.svg")
}

pub fn crosshair() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/crosshair.svg")
}

pub fn close() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/x.svg")
}

pub fn panel_left_close() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/panel-left-close.svg")
}

pub fn panel_left_open() -> ImageSource<'static> {
    egui::include_image!("../assets/icons/panel-left-open.svg")
}
