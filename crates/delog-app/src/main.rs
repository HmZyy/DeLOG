//! DeLOG application shell: eframe window, widgets, docks, layouts, glue.

mod app;

use app::DelogApp;

fn main() -> eframe::Result {
    let viewport = egui::ViewportBuilder::default()
        .with_title("DeLOG")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([1024.0, 640.0]);

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "DeLOG",
        options,
        Box::new(|cc| Ok(Box::new(DelogApp::new(cc)))),
    )
}
