//! Bundles `docs/logo.png` into the binary at build time.
//!
//! The logo is decoded once and resized to 256x256 (a sane icon size, and a
//! width that is a multiple of 4 as egui wants) and emitted as:
//!
//! * `icon.rgba` — raw RGBA pixels, embedded by `main.rs` via `include_bytes!`
//!   and handed to eframe as the window/taskbar icon. Cross-platform.
//!
//! The logo is intentionally *not* compiled into the `.exe` as a resource icon;
//! it only appears in the running app's window/taskbar.
//!
//! `docs/logo.png` is the single source of truth; nothing pre-generated is
//! committed, so CI and local builds always carry the current logo.

use std::path::PathBuf;

// Repo-relative path from this crate's directory (`crates/delog-app`).
const LOGO: &str = "../../docs/logo.png";
const ICON_SIZE: u32 = 256;

fn main() {
    println!("cargo:rerun-if-changed={LOGO}");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));

    let logo = image::open(LOGO)
        .unwrap_or_else(|e| panic!("failed to read {LOGO}: {e}"))
        .resize(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3);

    // Runtime window icon: raw RGBA, included verbatim by `main.rs`.
    std::fs::write(out_dir.join("icon.rgba"), logo.to_rgba8().into_raw()).expect("write icon.rgba");
}
