//! DeLOG application shell: eframe window, widgets, docks, layouts, glue.

// Hide the Windows console window in release builds: a default Rust binary is a
// console subsystem app, so Windows spawns a terminal alongside the GUI. Debug
// builds keep the console so `tracing` output stays visible during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod axes;
mod browser;
mod camera;
mod diagnostics;
mod field_stats;
mod generate_markers;
mod geo;
mod gpu;
mod hover;
mod icons;
mod layout;
mod legend;
mod live;
mod markers;
mod models;
#[cfg(feature = "scripting")]
mod parsers;
mod performance;
mod plot;
#[cfg(feature = "scripting")]
mod scripts;
mod session;
mod settings;
mod text_overlay;
mod theme;
mod timeline;
mod vehicle;
mod vehicle_dialog;
mod workspace;

use app::DelogApp;

fn main() -> eframe::Result {
    init_tracing();

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
        Box::new(|cc| {
            // SVG icon loader for the toolbar/overlay icons (see `icons`).
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(DelogApp::new(cc)))
        }),
    )
}

/// Initialize the tracing subscriber and a panic hook that records the
/// panic through tracing before the default hook prints the backtrace.
///
/// Filter via `RUST_LOG` (default `info`). The fmt writer goes to stderr
/// and is flushed per event, so panic messages are never lost in a buffer.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| l.to_string())
            .unwrap_or_else(|| "unknown location".to_owned());
        tracing::error!(target: "panic", %location, "{info}");
        default_hook(info);
    }));

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "DeLOG starting");
}
