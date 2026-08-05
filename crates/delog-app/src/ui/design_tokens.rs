#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DesignTokens {
    pub control_height: f32,
    pub dense_row_height: f32,
    pub dense_row_gap: f32,
    pub icon_size: f32,
    pub radius: u8,
    pub space_xs: f32,
    pub space_sm: f32,
    pub space_md: f32,
    pub panel_padding: f32,
}

impl Default for DesignTokens {
    fn default() -> Self {
        Self {
            control_height: 30.0,
            dense_row_height: 20.0,
            dense_row_gap: 2.0,
            icon_size: 18.0,
            radius: 6,
            space_xs: 4.0,
            space_sm: 8.0,
            space_md: 12.0,
            panel_padding: 10.0,
        }
    }
}

impl DesignTokens {
    pub fn from_style(style: &egui::Style) -> Self {
        Self {
            control_height: style.spacing.interact_size.y.max(30.0),
            ..Self::default()
        }
    }
}

pub fn apply_design_metrics(ctx: &egui::Context) {
    let tokens = DesignTokens::default();
    ctx.all_styles_mut(|style| {
        style.spacing.button_padding = egui::vec2(tokens.space_sm, tokens.space_xs);
        style.spacing.item_spacing = egui::vec2(tokens.space_sm, tokens.space_sm);
        style.spacing.interact_size.y = tokens.control_height;
        let radius = egui::CornerRadius::same(tokens.radius);
        style.visuals.widgets.noninteractive.corner_radius = radius;
        style.visuals.widgets.inactive.corner_radius = radius;
        style.visuals.widgets.hovered.corner_radius = radius;
        style.visuals.widgets.active.corner_radius = radius;
        style.visuals.widgets.open.corner_radius = radius;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn design_tokens_keep_compact_controls_touchable() {
        let tokens = DesignTokens::default();
        assert_eq!(tokens.control_height, 30.0);
        assert_eq!(tokens.dense_row_height, 20.0);
        assert_eq!(tokens.dense_row_gap, 2.0);
        assert_eq!(tokens.icon_size, 18.0);
        assert!(tokens.dense_row_gap < tokens.space_sm);
        assert!(tokens.dense_row_height < tokens.control_height);
        assert!(tokens.control_height >= tokens.icon_size + 8.0);
    }
}
