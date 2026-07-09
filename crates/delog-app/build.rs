use std::path::PathBuf;

// Repo-relative path from this crate's directory (`crates/delog-app`).
const LOGO: &str = "../../docs/logo.png";
// Multiple of 4 as egui wants.
const ICON_SIZE: u32 = 256;

fn main() {
    println!("cargo:rerun-if-changed={LOGO}");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));

    let logo = image::open(LOGO).unwrap_or_else(|e| panic!("failed to read {LOGO}: {e}"));

    let rgba = logo
        .resize(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    std::fs::write(out_dir.join("icon.rgba"), rgba.into_raw()).expect("write icon.rgba");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_icon(&logo, &out_dir);
    }
}

// The runtime window icon (icon.rgba, above) only covers the live window's
// caption. Windows draws the taskbar, Alt+Tab, Explorer, and shortcut icons
// from the executable's embedded icon resource, so emit one here.
fn embed_windows_icon(logo: &image::DynamicImage, out_dir: &std::path::Path) {
    const ICON_SIZES: &[u32] = &[16, 32, 48, 64, 128, 256];

    let frames: Vec<image::codecs::ico::IcoFrame> = ICON_SIZES
        .iter()
        .map(|&s| {
            let rgba = logo
                .resize_exact(s, s, image::imageops::FilterType::Lanczos3)
                .to_rgba8();
            image::codecs::ico::IcoFrame::as_png(rgba.as_raw(), s, s, image::ExtendedColorType::Rgba8)
                .expect("encode ico frame")
        })
        .collect();

    let ico_path = out_dir.join("delog.ico");
    let file = std::fs::File::create(&ico_path).expect("create delog.ico");
    image::codecs::ico::IcoEncoder::new(file)
        .encode_images(&frames)
        .expect("encode delog.ico");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico_path.to_str().expect("ico path is utf-8"));
    res.compile().expect("embed windows icon resource");
}
