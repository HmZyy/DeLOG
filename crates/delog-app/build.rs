use std::path::PathBuf;

// Repo-relative path from this crate's directory (`crates/delog-app`).
const LOGO: &str = "../../docs/logo.png";
// Multiple of 4 as egui wants.
const ICON_SIZE: u32 = 256;

fn main() {
    println!("cargo:rerun-if-changed={LOGO}");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));

    let logo = image::open(LOGO)
        .unwrap_or_else(|e| panic!("failed to read {LOGO}: {e}"))
        .resize(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3);

    std::fs::write(out_dir.join("icon.rgba"), logo.to_rgba8().into_raw()).expect("write icon.rgba");
}
