use std::path::PathBuf;

use image::ImageEncoder;

#[derive(Debug, Clone)]
pub enum ImageCaptureAction {
    Copy,
    Export,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageCaptureKind {
    Workspace,
    Plot,
}

#[derive(Debug, Clone)]
pub struct PendingImageCapture {
    pub id: u64,
    pub action: ImageCaptureAction,
    pub kind: ImageCaptureKind,
    pub rect: egui::Rect,
    pub pixels_per_point: f32,
}

#[derive(Debug, Clone)]
pub struct PngWriteRequest {
    pub path: PathBuf,
    pub png_bytes: Vec<u8>,
}

impl PngWriteRequest {
    pub fn new(path: PathBuf, png_bytes: Vec<u8>) -> Self {
        Self {
            path: png_path(path),
            png_bytes,
        }
    }
}

pub fn screenshot_request_id(user_data: &egui::UserData) -> Option<u64> {
    user_data
        .data
        .as_ref()
        .and_then(|data| data.downcast_ref::<u64>())
        .copied()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

pub fn pixel_rect_for_image(
    rect: egui::Rect,
    pixels_per_point: f32,
    image_size: [usize; 2],
) -> Option<PixelRect> {
    if pixels_per_point <= 0.0 || image_size[0] == 0 || image_size[1] == 0 {
        return None;
    }

    let min_x = (rect.min.x * pixels_per_point).floor().max(0.0) as usize;
    let min_y = (rect.min.y * pixels_per_point).floor().max(0.0) as usize;
    let max_x = (rect.max.x * pixels_per_point).ceil().max(0.0) as usize;
    let max_y = (rect.max.y * pixels_per_point).ceil().max(0.0) as usize;

    let x0 = min_x.min(image_size[0]);
    let y0 = min_y.min(image_size[1]);
    let x1 = max_x.min(image_size[0]);
    let y1 = max_y.min(image_size[1]);

    (x1 > x0 && y1 > y0).then_some(PixelRect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

pub fn crop_color_image(
    image: &egui::ColorImage,
    rect: egui::Rect,
    pixels_per_point: f32,
) -> Option<egui::ColorImage> {
    let px = pixel_rect_for_image(rect, pixels_per_point, image.size)?;
    Some(image.region_by_pixels([px.x, px.y], [px.w, px.h]))
}

pub fn encode_png(image: &egui::ColorImage) -> Result<Vec<u8>, image::ImageError> {
    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_array());
    }

    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    encoder.write_image(
        &rgba,
        image.size[0] as u32,
        image.size[1] as u32,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(out)
}

pub fn png_path(path: PathBuf) -> PathBuf {
    if path.extension().and_then(|ext| ext.to_str()) != Some("png") {
        return path.with_extension("png");
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_rect_clamps_to_image_bounds() {
        let rect = egui::Rect::from_min_max(egui::pos2(-2.0, 1.0), egui::pos2(6.25, 4.75));

        let got = pixel_rect_for_image(rect, 2.0, [10, 8]);

        assert_eq!(got, Some(PixelRect { x: 0, y: 2, w: 10, h: 6 }));
    }

    #[test]
    fn pixel_rect_rejects_empty_after_clamp() {
        let rect = egui::Rect::from_min_max(egui::pos2(9.0, 0.0), egui::pos2(12.0, 2.0));

        let got = pixel_rect_for_image(rect, 1.0, [8, 8]);

        assert_eq!(got, None);
    }

    #[test]
    fn crop_color_image_returns_selected_pixels() {
        let pixels = (0..16)
            .map(|i| egui::Color32::from_rgba_unmultiplied(i, i + 1, i + 2, 255))
            .collect();
        let image = egui::ColorImage::new([4, 4], pixels);
        let rect = egui::Rect::from_min_max(egui::pos2(1.0, 1.0), egui::pos2(3.0, 3.0));

        let crop = crop_color_image(&image, rect, 1.0).expect("crop should be valid");

        assert_eq!(crop.size, [2, 2]);
        assert_eq!(crop.pixels[0], egui::Color32::from_rgba_unmultiplied(5, 6, 7, 255));
        assert_eq!(crop.pixels[3], egui::Color32::from_rgba_unmultiplied(10, 11, 12, 255));
    }

    #[test]
    fn encode_png_writes_png_signature() {
        let image = egui::ColorImage::filled([2, 1], egui::Color32::from_rgb(1, 2, 3));

        let bytes = encode_png(&image).expect("png encoding should succeed");

        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn png_path_adds_png_extension_when_missing() {
        assert_eq!(png_path(PathBuf::from("capture")).as_os_str(), "capture.png");
        assert_eq!(png_path(PathBuf::from("capture.png")).as_os_str(), "capture.png");
    }

    #[test]
    fn png_write_request_normalizes_path_and_preserves_bytes() {
        let request = PngWriteRequest::new(PathBuf::from("capture"), vec![1, 2, 3]);

        assert_eq!(request.path.as_os_str(), "capture.png");
        assert_eq!(request.png_bytes, vec![1, 2, 3]);
    }

    #[test]
    fn screenshot_request_id_reads_u64_userdata() {
        let user_data = egui::UserData::new(42_u64);

        assert_eq!(screenshot_request_id(&user_data), Some(42));
    }
}
