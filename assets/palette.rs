// Keep this file `include!`-safe — no leading `//!` inner docs, since the
// contents are spliced into the middle of an enclosing module.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    #[must_use]
    pub const fn hex(rgb: u32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xff) as u8,
            g: ((rgb >> 8) & 0xff) as u8,
            b: (rgb & 0xff) as u8,
            a: 0xff,
        }
    }

    #[must_use]
    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    #[must_use]
    pub fn to_srgb_f32(self) -> [f32; 4] {
        [
            f32::from(self.r) / 255.0,
            f32::from(self.g) / 255.0,
            f32::from(self.b) / 255.0,
            f32::from(self.a) / 255.0,
        ]
    }

    /// Blending and shading must be done in linear space; alpha is not
    /// transfer-encoded, so it is copied through unchanged.
    #[must_use]
    pub fn to_linear_f32(self) -> [f32; 4] {
        [
            srgb_to_linear(self.r),
            srgb_to_linear(self.g),
            srgb_to_linear(self.b),
            f32::from(self.a) / 255.0,
        ]
    }
}

fn srgb_to_linear(c: u8) -> f32 {
    let s = f32::from(c) / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// The `#7aa2f7` blue at index 0 is pinned by the layout schema.
pub const TRACE_PALETTE: [Rgba8; 10] = [
    Rgba8::hex(0x7aa2f7),
    Rgba8::hex(0xff9e64),
    Rgba8::hex(0x9ece6a),
    Rgba8::hex(0xbb9af7),
    Rgba8::hex(0xe0af68),
    Rgba8::hex(0x2ac3de),
    Rgba8::hex(0xf7768e),
    Rgba8::hex(0x73daca),
    Rgba8::hex(0xff79c6),
    Rgba8::hex(0xc0caf5),
];

#[must_use]
pub fn trace_color(i: usize) -> Rgba8 {
    TRACE_PALETTE[i % TRACE_PALETTE.len()]
}

#[cfg(test)]
mod palette_tests {
    use super::*;

    #[test]
    fn palette_is_ten_distinct_opaque_colors() {
        assert_eq!(TRACE_PALETTE.len(), 10);
        for c in TRACE_PALETTE {
            assert_eq!(c.a, 0xff, "palette colors are opaque");
        }
        // All distinct: a duplicate would make two traces indistinguishable.
        for (i, a) in TRACE_PALETTE.iter().enumerate() {
            for b in &TRACE_PALETTE[i + 1..] {
                assert_ne!(a, b, "duplicate palette color {a:?}");
            }
        }
    }

    #[test]
    fn hex_unpacks_channels_in_rgb_order() {
        let c = Rgba8::hex(0x7aa2f7);
        assert_eq!((c.r, c.g, c.b, c.a), (0x7a, 0xa2, 0xf7, 0xff));
    }

    #[test]
    fn with_alpha_only_changes_alpha() {
        let c = Rgba8::hex(0x9ece6a).with_alpha(0x40);
        assert_eq!((c.r, c.g, c.b, c.a), (0x9e, 0xce, 0x6a, 0x40));
    }

    #[test]
    fn trace_color_cycles_after_exhaustion() {
        assert_eq!(trace_color(0), TRACE_PALETTE[0]);
        assert_eq!(trace_color(9), TRACE_PALETTE[9]);
        assert_eq!(trace_color(10), TRACE_PALETTE[0]);
        assert_eq!(trace_color(13), TRACE_PALETTE[3]);
    }

    #[test]
    fn srgb_f32_maps_endpoints() {
        assert_eq!(Rgba8::hex(0x000000).to_srgb_f32(), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(Rgba8::hex(0xffffff).to_srgb_f32(), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn linear_conversion_is_monotonic_with_known_endpoints() {
        let black = srgb_to_linear(0);
        let mid = srgb_to_linear(128);
        let white = srgb_to_linear(255);
        assert_eq!(black, 0.0);
        assert!((white - 1.0).abs() < 1e-6, "white -> {white}");
        // 50% sRGB is ~0.215 linear; checks the gamma curve, not just clamping.
        assert!((mid - 0.215).abs() < 0.01, "mid -> {mid}");
        assert!(black < mid && mid < white);
        assert_eq!(Rgba8::hex(0xffffff).with_alpha(0x80).to_linear_f32()[3], 128.0 / 255.0);
    }
}
