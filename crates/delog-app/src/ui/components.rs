use crate::ui::design_tokens::DesignTokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusState {
    Neutral,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChip {
    pub label: String,
    pub detail: Option<String>,
    pub state: StatusState,
}

impl StatusChip {
    #[cfg(test)]
    pub fn connected(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: Some(detail.into()),
            state: StatusState::Success,
        }
    }

    pub fn text(&self) -> String {
        match self.detail.as_deref() {
            Some(detail) => format!("{} · {detail}", self.label),
            None => self.label.clone(),
        }
    }
}

pub fn icon_button(
    ui: &mut egui::Ui,
    icon: egui::ImageSource<'static>,
    tooltip: &str,
    selected: bool,
) -> egui::Response {
    let tokens = DesignTokens::from_style(ui.style());
    icon_button_sized(
        ui,
        icon,
        tooltip,
        selected,
        egui::Vec2::splat(tokens.control_height),
        egui::Vec2::splat(tokens.icon_size),
    )
}

pub fn icon_button_sized(
    ui: &mut egui::Ui,
    icon: egui::ImageSource<'static>,
    tooltip: &str,
    selected: bool,
    button_size: egui::Vec2,
    icon_size: egui::Vec2,
) -> egui::Response {
    let image = egui::Image::new(icon)
        .fit_to_exact_size(icon_size)
        .tint(ui.visuals().text_color())
        .alt_text(tooltip);
    let response = ui.add_sized(button_size, egui::Button::image(image).selected(selected));
    let enabled = response.enabled();
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            enabled,
            selected,
            tooltip,
        )
    });
    response.on_hover_text(tooltip)
}

pub fn icon_text_button(
    ui: &mut egui::Ui,
    icon: egui::ImageSource<'static>,
    label: &str,
    selected: bool,
) -> egui::Response {
    let tokens = DesignTokens::from_style(ui.style());
    let image = egui::Image::new(icon)
        .fit_to_exact_size(egui::Vec2::splat(tokens.icon_size))
        .tint(ui.visuals().text_color());
    ui.add_sized(
        [0.0, tokens.control_height],
        egui::Button::image_and_text(image, label).selected(selected),
    )
}

pub fn status_chip(
    ui: &mut egui::Ui,
    chip: &StatusChip,
    theme: crate::ui::theme::ThemeChoice,
) -> egui::Response {
    let color = match chip.state {
        StatusState::Neutral => theme.neutral(),
        StatusState::Success => theme.success(),
        StatusState::Warning => theme.warning(),
        StatusState::Error => theme.error(),
    };
    ui.add(
        egui::Button::new(egui::RichText::new(chip.text()).color(color))
            .sense(egui::Sense::hover()),
    )
}

pub fn panel_header(ui: &mut egui::Ui, title: &str) -> egui::Response {
    ui.add(egui::Label::new(egui::RichText::new(title).strong()))
}

pub fn menu_row(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: Option<&str>,
    enabled: bool,
    disabled_reason: Option<&str>,
) -> egui::Response {
    let text = shortcut.map_or_else(|| label.to_owned(), |key| format!("{label}\t{key}"));
    let response = ui.add_enabled(enabled, egui::Button::new(text));
    match disabled_reason {
        Some(reason) if !enabled => response.on_disabled_hover_text(reason),
        _ => response,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_chip_uses_text_and_not_only_color() {
        let model = StatusChip::connected("UDP 14550", "48 Hz");
        assert_eq!(model.label, "UDP 14550");
        assert_eq!(model.detail.as_deref(), Some("48 Hz"));
        assert_eq!(model.state, StatusState::Success);
    }

    #[test]
    fn icon_buttons_emit_accessible_labels_and_selected_state() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let texture = egui::load::SizedTexture::new(
                egui::TextureId::default(),
                egui::Vec2::splat(1.0),
            );
            icon_button(ui, texture.into(), "Pin plot", true);
            icon_button(ui, texture.into(), "Unpinned plot", false);
        });
        let update = output
            .platform_output
            .accesskit_update
            .expect("accessibility tree should be emitted");
        let find = |label: &str| {
            update
                .nodes
                .iter()
                .map(|(_, node)| node)
                .find(|node| node.label() == Some(label))
                .expect("labelled icon button should exist")
        };

        let selected = find("Pin plot");
        assert_eq!(selected.role(), egui::accesskit::Role::Button);
        assert_eq!(selected.toggled(), Some(egui::accesskit::Toggled::True));
        assert_eq!(
            find("Unpinned plot").toggled(),
            Some(egui::accesskit::Toggled::False)
        );
    }
}
