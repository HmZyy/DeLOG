// Release builds: hide the Windows console window that a console-subsystem
// binary would otherwise spawn alongside the GUI. Debug keeps it for `tracing`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod axes;
mod browser;
mod camera;
mod csv_export;
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

    let options = app_native_options();

    eframe::run_native(
        "DeLOG",
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(DelogApp::new(cc)))
        }),
    )
}

fn app_native_options() -> eframe::NativeOptions {
    // VSync is configured once at surface creation, so it must be read from the
    // persisted settings here rather than at app construction.
    let vsync = crate::layout::load_app_settings().vsync;

    let mut options = eframe::NativeOptions {
        viewport: app_viewport(),
        renderer: eframe::Renderer::Wgpu,
        // `NativeOptions.vsync` only affects the glow backend; the wgpu backend
        // ignores it and reads `wgpu_options.present_mode`. Set both so the
        // setting works regardless of renderer. `Fifo` is the canonical, always-
        // supported vsync mode (hard-caps to the monitor refresh rate).
        vsync,
        ..Default::default()
    };
    options.wgpu_options.present_mode = if vsync {
        eframe::wgpu::PresentMode::Fifo
    } else {
        eframe::wgpu::PresentMode::AutoNoVsync
    };

    options
}

/// Filter via `RUST_LOG` (default `info`). The panic hook records the panic
/// through tracing before the default hook runs.
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

fn app_viewport() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title("DeLOG")
        .with_icon(app_icon())
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([1024.0, 640.0])
}

/// 256x256 RGBA decoded from `docs/logo.png` by `build.rs` into `OUT_DIR`.
fn app_icon() -> egui::IconData {
    const RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon.rgba"));
    egui::IconData {
        rgba: RGBA.to_vec(),
        width: 256,
        height: 256,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_icon_is_256x256_rgba() {
        let icon = app_icon();
        assert_eq!(icon.width, 256);
        assert_eq!(icon.height, 256);
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}
