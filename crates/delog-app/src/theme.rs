//! Color palettes and egui visuals for the app's dark themes.
//!
//! Each palette maps a well-known scheme onto a small set of semantic slots
//! that [`Palette::visuals`] and the accent helpers consume. Background slots
//! run darkest → lightest as `crust < mantle < base < surface0 < surface1 <
//! surface2`; `base` is the panel fill, the surfaces are widget fills, and the
//! two darker shades back text fields and inset areas.

/// A theme palette in semantic slots, independent of any one scheme's naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Darkest background - extreme/inset areas (e.g. scroll troughs).
    pub crust: egui::Color32,
    /// Slightly-dark background - text edit and code block fills.
    pub mantle: egui::Color32,
    /// Main window/panel fill.
    pub base: egui::Color32,
    /// Resting widget fill (and faint row striping).
    pub surface0: egui::Color32,
    /// Active/pressed widget fill.
    pub surface1: egui::Color32,
    /// Hovered widget fill.
    pub surface2: egui::Color32,
    /// Mid-grey used for strokes and neutral status.
    pub overlay1: egui::Color32,
    /// Dimmed (weak) text.
    pub subtext0: egui::Color32,
    /// Primary text.
    pub text: egui::Color32,
    /// Hyperlink color.
    pub link: egui::Color32,
    /// Warning foreground inside egui visuals (typically the scheme's orange).
    pub warn_fg: egui::Color32,
    /// Accent - selection tint, primary highlights.
    pub accent: egui::Color32,
    /// Success / OK status.
    pub success: egui::Color32,
    /// Warning status.
    pub warning: egui::Color32,
    /// Error status.
    pub error: egui::Color32,
}

/// Catppuccin Mocha - https://catppuccin.com
pub const MOCHA: Palette = Palette {
    crust: egui::Color32::from_rgb(17, 17, 27),
    mantle: egui::Color32::from_rgb(24, 24, 37),
    base: egui::Color32::from_rgb(30, 30, 46),
    surface0: egui::Color32::from_rgb(49, 50, 68),
    surface1: egui::Color32::from_rgb(69, 71, 90),
    surface2: egui::Color32::from_rgb(88, 91, 112),
    overlay1: egui::Color32::from_rgb(127, 132, 156),
    subtext0: egui::Color32::from_rgb(166, 173, 200),
    text: egui::Color32::from_rgb(205, 214, 244),
    link: egui::Color32::from_rgb(245, 224, 220),
    warn_fg: egui::Color32::from_rgb(250, 179, 135),
    accent: egui::Color32::from_rgb(137, 180, 250),
    success: egui::Color32::from_rgb(166, 227, 161),
    warning: egui::Color32::from_rgb(249, 226, 175),
    error: egui::Color32::from_rgb(243, 139, 168),
};

/// Gruvbox Dark - https://github.com/morhetz/gruvbox (medium contrast).
pub const GRUVBOX: Palette = Palette {
    crust: egui::Color32::from_rgb(29, 32, 33),     // bg0_hard
    mantle: egui::Color32::from_rgb(40, 40, 40),    // bg0
    base: egui::Color32::from_rgb(50, 48, 47),      // bg0_soft
    surface0: egui::Color32::from_rgb(60, 56, 54),  // bg1
    surface1: egui::Color32::from_rgb(80, 73, 69),  // bg2
    surface2: egui::Color32::from_rgb(102, 92, 84), // bg3
    overlay1: egui::Color32::from_rgb(146, 131, 116), // gray
    subtext0: egui::Color32::from_rgb(189, 174, 147), // fg3
    text: egui::Color32::from_rgb(235, 219, 178),   // fg1
    link: egui::Color32::from_rgb(142, 192, 124),   // bright_aqua
    warn_fg: egui::Color32::from_rgb(254, 128, 25), // bright_orange
    accent: egui::Color32::from_rgb(131, 165, 152), // bright_blue
    success: egui::Color32::from_rgb(184, 187, 38), // bright_green
    warning: egui::Color32::from_rgb(250, 189, 47), // bright_yellow
    error: egui::Color32::from_rgb(251, 73, 52),    // bright_red
};

/// Tokyo Night - https://github.com/folke/tokyonight.nvim (night variant).
pub const TOKYO_NIGHT: Palette = Palette {
    crust: egui::Color32::from_rgb(12, 14, 20),     // bg_dark1
    mantle: egui::Color32::from_rgb(22, 22, 30),    // bg_dark
    base: egui::Color32::from_rgb(26, 27, 38),      // bg
    surface0: egui::Color32::from_rgb(41, 46, 66),  // bg_highlight
    surface1: egui::Color32::from_rgb(59, 66, 97),  // fg_gutter
    surface2: egui::Color32::from_rgb(65, 72, 104), // terminal bright black
    overlay1: egui::Color32::from_rgb(86, 95, 137), // comment
    subtext0: egui::Color32::from_rgb(169, 177, 214), // fg_dark
    text: egui::Color32::from_rgb(192, 202, 245),   // fg
    link: egui::Color32::from_rgb(125, 207, 255),   // cyan
    warn_fg: egui::Color32::from_rgb(255, 158, 100), // orange
    accent: egui::Color32::from_rgb(122, 162, 247), // blue
    success: egui::Color32::from_rgb(158, 206, 106), // green
    warning: egui::Color32::from_rgb(224, 175, 104), // yellow
    error: egui::Color32::from_rgb(247, 118, 142),  // red
};

/// Nord - https://www.nordtheme.com (darker base shades extend Polar Night).
pub const NORD: Palette = Palette {
    crust: egui::Color32::from_rgb(36, 41, 51), // darkened Polar Night
    mantle: egui::Color32::from_rgb(41, 46, 57), // darkened Polar Night
    base: egui::Color32::from_rgb(46, 52, 64),  // nord0
    surface0: egui::Color32::from_rgb(59, 66, 82), // nord1
    surface1: egui::Color32::from_rgb(67, 76, 94), // nord2
    surface2: egui::Color32::from_rgb(76, 86, 106), // nord3
    overlay1: egui::Color32::from_rgb(97, 110, 136), // nord comment grey
    subtext0: egui::Color32::from_rgb(216, 222, 233), // nord4
    text: egui::Color32::from_rgb(236, 239, 244), // nord6
    link: egui::Color32::from_rgb(136, 192, 208), // nord8
    warn_fg: egui::Color32::from_rgb(208, 135, 112), // nord12 orange
    accent: egui::Color32::from_rgb(129, 161, 193), // nord9 blue
    success: egui::Color32::from_rgb(163, 190, 140), // nord14 green
    warning: egui::Color32::from_rgb(235, 203, 139), // nord13 yellow
    error: egui::Color32::from_rgb(191, 97, 106), // nord11 red
};

/// Kanagawa - https://github.com/rebelot/kanagawa.nvim (wave variant).
pub const KANAGAWA: Palette = Palette {
    crust: egui::Color32::from_rgb(22, 22, 29),     // sumiInk0
    mantle: egui::Color32::from_rgb(24, 24, 32),    // sumiInk1
    base: egui::Color32::from_rgb(31, 31, 40),      // sumiInk3
    surface0: egui::Color32::from_rgb(42, 42, 55),  // sumiInk4
    surface1: egui::Color32::from_rgb(54, 54, 70),  // sumiInk5
    surface2: egui::Color32::from_rgb(84, 84, 109), // sumiInk6
    overlay1: egui::Color32::from_rgb(114, 113, 105), // fujiGray
    subtext0: egui::Color32::from_rgb(200, 192, 147), // oldWhite
    text: egui::Color32::from_rgb(220, 215, 186),   // fujiWhite
    link: egui::Color32::from_rgb(127, 180, 202),   // springBlue
    warn_fg: egui::Color32::from_rgb(255, 160, 102), // surimiOrange
    accent: egui::Color32::from_rgb(126, 156, 216), // crystalBlue
    success: egui::Color32::from_rgb(152, 187, 108), // springGreen
    warning: egui::Color32::from_rgb(230, 195, 132), // carpYellow
    error: egui::Color32::from_rgb(228, 104, 118),  // waveRed
};

/// Everforest - https://github.com/sainnhe/everforest (dark, medium contrast).
pub const EVERFOREST: Palette = Palette {
    crust: egui::Color32::from_rgb(30, 35, 38),    // hard bg0
    mantle: egui::Color32::from_rgb(35, 42, 46),   // bg_dim
    base: egui::Color32::from_rgb(45, 53, 59),     // bg0
    surface0: egui::Color32::from_rgb(52, 63, 68), // bg1
    surface1: egui::Color32::from_rgb(61, 72, 77), // bg2
    surface2: egui::Color32::from_rgb(71, 82, 88), // bg3
    overlay1: egui::Color32::from_rgb(122, 132, 120), // grey0
    subtext0: egui::Color32::from_rgb(157, 169, 160), // grey2
    text: egui::Color32::from_rgb(211, 198, 170),  // fg
    link: egui::Color32::from_rgb(131, 192, 146),  // aqua
    warn_fg: egui::Color32::from_rgb(230, 152, 117), // orange
    accent: egui::Color32::from_rgb(127, 187, 179), // blue
    success: egui::Color32::from_rgb(167, 192, 128), // green
    warning: egui::Color32::from_rgb(219, 188, 127), // yellow
    error: egui::Color32::from_rgb(230, 126, 128), // red
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    Light,
    Dark,
    #[default]
    CatppuccinMocha,
    Gruvbox,
    TokyoNight,
    Nord,
    Kanagawa,
    Everforest,
}

impl ThemeChoice {
    pub const ALL: [Self; 8] = [
        Self::Light,
        Self::Dark,
        Self::CatppuccinMocha,
        Self::Gruvbox,
        Self::TokyoNight,
        Self::Nord,
        Self::Kanagawa,
        Self::Everforest,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::Gruvbox => "Gruvbox",
            Self::TokyoNight => "Tokyo Night",
            Self::Nord => "Nord",
            Self::Kanagawa => "Kanagawa",
            Self::Everforest => "Everforest",
        }
    }

    /// The palette backing this choice, or `None` for egui's built-in
    /// Light/Dark visuals.
    pub const fn palette(self) -> Option<Palette> {
        match self {
            Self::Light | Self::Dark => None,
            Self::CatppuccinMocha => Some(MOCHA),
            Self::Gruvbox => Some(GRUVBOX),
            Self::TokyoNight => Some(TOKYO_NIGHT),
            Self::Nord => Some(NORD),
            Self::Kanagawa => Some(KANAGAWA),
            Self::Everforest => Some(EVERFOREST),
        }
    }

    pub fn apply(self, ctx: &egui::Context) {
        match self.palette() {
            Some(palette) => {
                ctx.set_theme(egui::ThemePreference::Dark);
                set_theme(ctx, palette);
            }
            None => match self {
                Self::Light => {
                    ctx.set_theme(egui::ThemePreference::Light);
                    ctx.set_visuals(egui::Visuals::light());
                }
                // Dark, and any future built-in.
                _ => {
                    ctx.set_theme(egui::ThemePreference::Dark);
                    ctx.set_visuals(egui::Visuals::dark());
                }
            },
        }
    }

    pub const fn accent(self) -> egui::Color32 {
        match self.palette() {
            Some(p) => p.accent,
            None => match self {
                Self::Light => egui::Color32::from_rgb(30, 102, 245),
                _ => egui::Color32::from_rgb(90, 170, 255),
            },
        }
    }

    pub const fn success(self) -> egui::Color32 {
        match self.palette() {
            Some(p) => p.success,
            None => match self {
                Self::Light => egui::Color32::from_rgb(64, 160, 43),
                _ => egui::Color32::from_rgb(80, 200, 120),
            },
        }
    }

    pub const fn warning(self) -> egui::Color32 {
        match self.palette() {
            Some(p) => p.warning,
            None => match self {
                Self::Light => egui::Color32::from_rgb(223, 142, 29),
                _ => egui::Color32::from_rgb(230, 170, 60),
            },
        }
    }

    pub const fn error(self) -> egui::Color32 {
        match self.palette() {
            Some(p) => p.error,
            None => match self {
                Self::Light => egui::Color32::from_rgb(210, 15, 57),
                _ => egui::Color32::from_rgb(230, 80, 95),
            },
        }
    }

    pub const fn neutral(self) -> egui::Color32 {
        match self.palette() {
            Some(p) => p.overlay1,
            None => match self {
                Self::Light => egui::Color32::from_rgb(124, 127, 147),
                _ => egui::Color32::from_rgb(140, 143, 161),
            },
        }
    }
}

fn set_theme(ctx: &egui::Context, palette: Palette) {
    let old = ctx.style_of(egui::Theme::Dark).visuals.clone();
    ctx.set_visuals(palette.visuals(old));
}

fn make_widget_visual(
    old: egui::style::WidgetVisuals,
    palette: &Palette,
    bg_fill: egui::Color32,
) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals {
        bg_fill,
        weak_bg_fill: bg_fill,
        bg_stroke: egui::Stroke {
            color: palette.overlay1,
            ..old.bg_stroke
        },
        fg_stroke: egui::Stroke {
            color: palette.text,
            ..old.fg_stroke
        },
        ..old
    }
}

impl Palette {
    fn visuals(&self, old: egui::Visuals) -> egui::Visuals {
        let shadow_color = egui::Color32::from_black_alpha(96);
        egui::Visuals {
            hyperlink_color: self.link,
            faint_bg_color: self.surface0,
            extreme_bg_color: self.crust,
            text_edit_bg_color: Some(self.mantle),
            code_bg_color: self.mantle,
            warn_fg_color: self.warn_fg,
            error_fg_color: self.error,
            window_fill: self.base,
            panel_fill: self.base,
            weak_text_color: Some(self.subtext0),
            window_stroke: egui::Stroke {
                color: self.overlay1,
                ..old.window_stroke
            },
            widgets: egui::style::Widgets {
                noninteractive: make_widget_visual(old.widgets.noninteractive, self, self.base),
                inactive: make_widget_visual(old.widgets.inactive, self, self.surface0),
                hovered: make_widget_visual(old.widgets.hovered, self, self.surface2),
                active: make_widget_visual(old.widgets.active, self, self.surface1),
                open: make_widget_visual(old.widgets.open, self, self.surface0),
            },
            selection: egui::style::Selection {
                bg_fill: self.accent.linear_multiply(0.2),
                stroke: egui::Stroke {
                    color: self.text,
                    ..old.selection.stroke
                },
            },
            window_shadow: egui::epaint::Shadow {
                color: shadow_color,
                ..old.window_shadow
            },
            popup_shadow: egui::epaint::Shadow {
                color: shadow_color,
                ..old.popup_shadow
            },
            dark_mode: true,
            ..old
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_choices_have_stable_labels() {
        let labels: Vec<_> = ThemeChoice::ALL
            .into_iter()
            .map(ThemeChoice::label)
            .collect();
        assert_eq!(
            labels,
            [
                "Light",
                "Dark",
                "Catppuccin Mocha",
                "Gruvbox",
                "Tokyo Night",
                "Nord",
                "Kanagawa",
                "Everforest",
            ]
        );
    }

    #[test]
    fn every_palette_text_passes_dense_ui_contrast() {
        for choice in ThemeChoice::ALL {
            let Some(p) = choice.palette() else { continue };
            for fg in [p.text, p.subtext0] {
                for bg in [p.base, p.mantle, p.crust] {
                    assert!(
                        contrast_ratio(fg, bg) >= 4.5,
                        "{} fg {fg:?} on bg {bg:?} fails 4.5:1",
                        choice.label(),
                    );
                }
            }
        }
    }

    #[test]
    fn applied_theme_keeps_dark_mode_and_palette_fills() {
        let visuals = MOCHA.visuals(egui::Visuals::dark());
        assert!(visuals.dark_mode);
        assert_eq!(visuals.panel_fill, MOCHA.base);
        assert_eq!(visuals.window_fill, MOCHA.base);
        assert_eq!(visuals.extreme_bg_color, MOCHA.crust);
        assert_eq!(visuals.widgets.hovered.bg_fill, MOCHA.surface2);
    }

    fn contrast_ratio(fg: egui::Color32, bg: egui::Color32) -> f32 {
        let a = relative_luminance(fg);
        let b = relative_luminance(bg);
        let (lighter, darker) = if a >= b { (a, b) } else { (b, a) };
        (lighter + 0.05) / (darker + 0.05)
    }

    fn relative_luminance(c: egui::Color32) -> f32 {
        0.2126 * channel_luminance(c.r())
            + 0.7152 * channel_luminance(c.g())
            + 0.0722 * channel_luminance(c.b())
    }

    fn channel_luminance(c: u8) -> f32 {
        let s = f32::from(c) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
}
