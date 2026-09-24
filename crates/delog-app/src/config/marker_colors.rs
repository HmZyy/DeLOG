//! Persistent colors for values used by the marker generator.

use std::collections::{BTreeMap, HashSet};

use delog_render::palette::Rgba8;

const NAMED_COLORS: [Rgba8; 32] = [
    Rgba8::hex(0x3b82f6), // blue
    Rgba8::hex(0xf97316), // orange
    Rgba8::hex(0x22c55e), // green
    Rgba8::hex(0xef4444), // red
    Rgba8::hex(0xa855f7), // purple
    Rgba8::hex(0x06b6d4), // cyan
    Rgba8::hex(0xeab308), // yellow
    Rgba8::hex(0xec4899), // pink
    Rgba8::hex(0x84cc16), // lime
    Rgba8::hex(0x14b8a6), // teal
    Rgba8::hex(0x6366f1), // indigo
    Rgba8::hex(0xffd700), // gold
    Rgba8::hex(0xff7f50), // coral
    Rgba8::hex(0x38bdf8), // sky blue
    Rgba8::hex(0xff00ff), // magenta
    Rgba8::hex(0x40e0d0), // turquoise
    Rgba8::hex(0xc4b5fd), // lavender
    Rgba8::hex(0xfa8072), // salmon
    Rgba8::hex(0x98ff98), // mint
    Rgba8::hex(0xffbf00), // amber
    Rgba8::hex(0x8f00ff), // violet
    Rgba8::hex(0xa3a33a), // olive
    Rgba8::hex(0xffcc99), // peach
    Rgba8::hex(0x4682b4), // steel blue
    Rgba8::hex(0xff007f), // rose
    Rgba8::hex(0xc0ff00), // chartreuse
    Rgba8::hex(0xb87333), // copper
    Rgba8::hex(0xbdb76b), // khaki
    Rgba8::hex(0xdda0dd), // plum
    Rgba8::hex(0xc0c0c0), // silver
    Rgba8::hex(0x7fffd4), // aquamarine
    Rgba8::hex(0xccccff), // periwinkle
];

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct MarkerValueColors(BTreeMap<String, [u8; 3]>);

impl MarkerValueColors {
    pub fn color_for(&mut self, value: &str) -> [f32; 4] {
        let rgb = match self.0.get(value) {
            Some(&rgb) => rgb,
            None => {
                let rgb = self.next_color();
                self.0.insert(value.to_owned(), rgb);
                rgb
            }
        };
        Rgba8 {
            r: rgb[0],
            g: rgb[1],
            b: rgb[2],
            a: 255,
        }
        .to_srgb_f32()
    }

    fn next_color(&self) -> [u8; 3] {
        let used: HashSet<_> = self.0.values().copied().collect();
        let rgb = |color: &Rgba8| [color.r, color.g, color.b];
        if let Some(color) = NAMED_COLORS[..8]
            .iter()
            .map(rgb)
            .find(|color| !used.contains(color))
        {
            return color;
        }

        let separation = |color: &[u8; 3]| {
            used.iter()
                .map(|other| {
                    color
                        .iter()
                        .zip(other)
                        .map(|(&a, &b)| (i32::from(a) - i32::from(b)).pow(2))
                        .sum::<i32>()
                })
                .min()
                .unwrap_or(i32::MAX)
        };
        if let Some(color) = NAMED_COLORS[8..]
            .iter()
            .map(rgb)
            .filter(|color| !used.contains(color))
            .max_by_key(separation)
        {
            return color;
        }

        // Extend beyond the named palette instead of cycling it. This odd
        // multiplier visits every 24-bit RGB value before repeating; choose
        // the most separated of 512 unused, reasonably bright candidates.
        let start = (used.len() as u32).wrapping_mul(512);
        (0u32..)
            .map(|i| Rgba8::hex(i.wrapping_add(start).wrapping_mul(0x9e3779)))
            .map(|color| rgb(&color))
            .filter(|color| color.iter().any(|&channel| channel >= 160))
            .filter(|color| !color.iter().all(|&channel| channel > 224))
            .filter(|color| !used.contains(color))
            .take(512)
            .max_by_key(separation)
            .expect("the RGB space exceeds the marker generator's value limit")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(color: [f32; 4]) -> [u8; 3] {
        [color[0], color[1], color[2]].map(|channel| (channel * 255.0).round() as u8)
    }

    #[test]
    fn values_10_and_18_start_with_distinct_common_colors() {
        let mut colors = MarkerValueColors::default();
        assert_eq!(rgb(colors.color_for("10")), [59, 130, 246]);
        assert_eq!(rgb(colors.color_for("18")), [249, 115, 22]);
        assert_eq!(rgb(colors.color_for("10")), [59, 130, 246]);
    }

    #[test]
    fn uses_32_named_colors_before_generating_additional_shades() {
        let mut colors = MarkerValueColors::default();
        let mut assigned = HashSet::new();
        for value in 0..32 {
            let color = rgb(colors.color_for(&value.to_string()));
            assert!(NAMED_COLORS.iter().any(|c| [c.r, c.g, c.b] == color));
            assert!(assigned.insert(color), "value {value} reused {color:?}");
        }
        let extra = rgb(colors.color_for("extra"));
        assert!(!assigned.contains(&extra));
    }

    #[test]
    fn saved_assignments_survive_subsets_new_values_and_more_than_one_log() {
        let mut settings = crate::config::settings::AppSettings::default();
        let expected: Vec<_> = (0..128)
            .map(|value| settings.marker_value_colors.color_for(&value.to_string()))
            .collect();
        let unique: HashSet<_> = expected.iter().copied().map(rgb).collect();
        assert_eq!(unique.len(), 128);
        let json = serde_json::to_string(&settings).unwrap();
        let mut loaded: crate::config::settings::AppSettings = serde_json::from_str(&json).unwrap();
        let new_color = loaded.marker_value_colors.color_for("AUTO");
        assert!(!unique.contains(&rgb(new_color)));
        for value in [18, 10, 127, 0, 63] {
            assert_eq!(
                loaded.marker_value_colors.color_for(&value.to_string()),
                expected[value]
            );
        }
    }
}
