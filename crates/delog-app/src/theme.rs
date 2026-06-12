//! Catppuccin Mocha visuals for the egui shell.
//!
//! `catppuccin-egui` 5.7.0 does not expose an `egui34` feature, so this module
//! mirrors its theme mapping against the workspace-pinned egui 0.34 API.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatppuccinTheme {
    pub rosewater: egui::Color32,
    pub flamingo: egui::Color32,
    pub pink: egui::Color32,
    pub mauve: egui::Color32,
    pub red: egui::Color32,
    pub maroon: egui::Color32,
    pub peach: egui::Color32,
    pub yellow: egui::Color32,
    pub green: egui::Color32,
    pub teal: egui::Color32,
    pub sky: egui::Color32,
    pub sapphire: egui::Color32,
    pub blue: egui::Color32,
    pub lavender: egui::Color32,
    pub text: egui::Color32,
    pub subtext1: egui::Color32,
    pub subtext0: egui::Color32,
    pub overlay2: egui::Color32,
    pub overlay1: egui::Color32,
    pub overlay0: egui::Color32,
    pub surface2: egui::Color32,
    pub surface1: egui::Color32,
    pub surface0: egui::Color32,
    pub base: egui::Color32,
    pub mantle: egui::Color32,
    pub crust: egui::Color32,
}

pub const MOCHA: CatppuccinTheme = CatppuccinTheme {
    rosewater: egui::Color32::from_rgb(245, 224, 220),
    flamingo: egui::Color32::from_rgb(242, 205, 205),
    pink: egui::Color32::from_rgb(245, 194, 231),
    mauve: egui::Color32::from_rgb(203, 166, 247),
    red: egui::Color32::from_rgb(243, 139, 168),
    maroon: egui::Color32::from_rgb(235, 160, 172),
    peach: egui::Color32::from_rgb(250, 179, 135),
    yellow: egui::Color32::from_rgb(249, 226, 175),
    green: egui::Color32::from_rgb(166, 227, 161),
    teal: egui::Color32::from_rgb(148, 226, 213),
    sky: egui::Color32::from_rgb(137, 220, 235),
    sapphire: egui::Color32::from_rgb(116, 199, 236),
    blue: egui::Color32::from_rgb(137, 180, 250),
    lavender: egui::Color32::from_rgb(180, 190, 254),
    text: egui::Color32::from_rgb(205, 214, 244),
    subtext1: egui::Color32::from_rgb(186, 194, 222),
    subtext0: egui::Color32::from_rgb(166, 173, 200),
    overlay2: egui::Color32::from_rgb(147, 153, 178),
    overlay1: egui::Color32::from_rgb(127, 132, 156),
    overlay0: egui::Color32::from_rgb(108, 112, 134),
    surface2: egui::Color32::from_rgb(88, 91, 112),
    surface1: egui::Color32::from_rgb(69, 71, 90),
    surface0: egui::Color32::from_rgb(49, 50, 68),
    base: egui::Color32::from_rgb(30, 30, 46),
    mantle: egui::Color32::from_rgb(24, 24, 37),
    crust: egui::Color32::from_rgb(17, 17, 27),
};

pub const ACCENT: egui::Color32 = MOCHA.blue;
pub const SUCCESS: egui::Color32 = MOCHA.green;
pub const WARNING: egui::Color32 = MOCHA.yellow;
pub const ERROR: egui::Color32 = MOCHA.red;
pub const NEUTRAL: egui::Color32 = MOCHA.overlay1;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    Light,
    Dark,
    #[default]
    CatppuccinMocha,
}

impl ThemeChoice {
    pub const ALL: [Self; 3] = [Self::Light, Self::Dark, Self::CatppuccinMocha];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::CatppuccinMocha => "Catppuccin Mocha",
        }
    }

    pub fn apply(self, ctx: &egui::Context) {
        match self {
            Self::Light => {
                ctx.set_theme(egui::ThemePreference::Light);
                ctx.set_visuals(egui::Visuals::light());
            }
            Self::Dark => {
                ctx.set_theme(egui::ThemePreference::Dark);
                ctx.set_visuals(egui::Visuals::dark());
            }
            Self::CatppuccinMocha => {
                ctx.set_theme(egui::ThemePreference::Dark);
                set_theme(ctx, MOCHA);
            }
        }
    }

    pub const fn accent(self) -> egui::Color32 {
        match self {
            Self::Light => egui::Color32::from_rgb(30, 102, 245),
            Self::Dark => egui::Color32::from_rgb(90, 170, 255),
            Self::CatppuccinMocha => ACCENT,
        }
    }

    pub const fn success(self) -> egui::Color32 {
        match self {
            Self::Light => egui::Color32::from_rgb(64, 160, 43),
            Self::Dark => egui::Color32::from_rgb(80, 200, 120),
            Self::CatppuccinMocha => SUCCESS,
        }
    }

    pub const fn warning(self) -> egui::Color32 {
        match self {
            Self::Light => egui::Color32::from_rgb(223, 142, 29),
            Self::Dark => egui::Color32::from_rgb(230, 170, 60),
            Self::CatppuccinMocha => WARNING,
        }
    }

    pub const fn error(self) -> egui::Color32 {
        match self {
            Self::Light => egui::Color32::from_rgb(210, 15, 57),
            Self::Dark => egui::Color32::from_rgb(230, 80, 95),
            Self::CatppuccinMocha => ERROR,
        }
    }

    pub const fn neutral(self) -> egui::Color32 {
        match self {
            Self::Light => egui::Color32::from_rgb(124, 127, 147),
            Self::Dark => egui::Color32::from_rgb(140, 143, 161),
            Self::CatppuccinMocha => NEUTRAL,
        }
    }
}

fn set_theme(ctx: &egui::Context, theme: CatppuccinTheme) {
    let old = ctx.style_of(egui::Theme::Dark).visuals.clone();
    ctx.set_visuals(theme.visuals(old));
}

fn make_widget_visual(
    old: egui::style::WidgetVisuals,
    theme: &CatppuccinTheme,
    bg_fill: egui::Color32,
) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals {
        bg_fill,
        weak_bg_fill: bg_fill,
        bg_stroke: egui::Stroke {
            color: theme.overlay1,
            ..old.bg_stroke
        },
        fg_stroke: egui::Stroke {
            color: theme.text,
            ..old.fg_stroke
        },
        ..old
    }
}

impl CatppuccinTheme {
    fn visuals(&self, old: egui::Visuals) -> egui::Visuals {
        let shadow_color = egui::Color32::from_black_alpha(96);
        egui::Visuals {
            hyperlink_color: self.rosewater,
            faint_bg_color: self.surface0,
            extreme_bg_color: self.crust,
            text_edit_bg_color: Some(self.mantle),
            code_bg_color: self.mantle,
            warn_fg_color: self.peach,
            error_fg_color: self.red,
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
                bg_fill: self.blue.linear_multiply(0.2),
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
        assert_eq!(labels, ["Light", "Dark", "Catppuccin Mocha"]);
    }

    #[test]
    fn mocha_text_colors_pass_dense_ui_contrast() {
        for fg in [MOCHA.text, MOCHA.subtext1, MOCHA.subtext0] {
            assert!(contrast_ratio(fg, MOCHA.base) >= 4.5);
            assert!(contrast_ratio(fg, MOCHA.mantle) >= 4.5);
            assert!(contrast_ratio(fg, MOCHA.crust) >= 4.5);
        }
    }

    #[test]
    fn applied_theme_keeps_dark_mode_and_catppuccin_fills() {
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
