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
    let image = egui::Image::new(icon)
        .fit_to_exact_size(egui::Vec2::splat(tokens.icon_size))
        .tint(ui.visuals().text_color());
    ui.add_sized(
        egui::Vec2::splat(tokens.control_height),
        egui::Button::image(image).selected(selected),
    )
    .on_hover_text(tooltip)
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
}
