//! Centered info/error message popups with an OK button (egui-native).

pub struct MessagePopup {
    title: String,
    text: String,
    error: bool,
}

impl MessagePopup {
    pub fn info(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            error: false,
        }
    }

    pub fn error(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            error: true,
        }
    }

    /// Draw the popup centered on screen. Returns `false` once dismissed
    /// (OK button, title-bar close or Escape).
    pub fn show(&self, ctx: &egui::Context, id: egui::Id) -> bool {
        let mut open = true;
        let mut dismissed = false;
        egui::Window::new(&self.title)
            .id(id)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (icon, tint) = if self.error {
                        (crate::ui::icons::circle_alert(), ui.visuals().error_fg_color)
                    } else {
                        (crate::ui::icons::info(), ui.visuals().text_color())
                    };
                    ui.add(
                        egui::Image::new(icon)
                            .tint(tint)
                            .fit_to_exact_size(egui::vec2(20.0, 20.0)),
                    );
                    ui.label(&self.text);
                });
                ui.vertical_centered(|ui| {
                    if ui.button("OK").clicked() {
                        dismissed = true;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            dismissed = true;
        }
        open && !dismissed
    }
}

/// Draw every queued popup, dropping the ones that were dismissed.
pub fn show_all(popups: &mut Vec<MessagePopup>, ctx: &egui::Context) {
    let mut index = 0usize;
    popups.retain(|popup| {
        let id = egui::Id::new(("message-popup", index));
        index += 1;
        popup.show(ctx, id)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_frame(ctx: &egui::Context, input: egui::RawInput, popup: &MessagePopup) -> bool {
        let mut open = true;
        let _ = ctx.run_ui(input, |ui| {
            open = popup.show(ui.ctx(), egui::Id::new("test-popup"));
        });
        open
    }

    fn escape_press() -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        input
    }

    #[test]
    fn popup_stays_open_without_interaction() {
        let ctx = egui::Context::default();
        let popup = MessagePopup::info("KML export", "exported 1 vehicle trajectory");
        assert!(run_frame(&ctx, egui::RawInput::default(), &popup));
    }

    #[test]
    fn escape_dismisses_info_and_error_popups() {
        let ctx = egui::Context::default();
        let info = MessagePopup::info("KML export", "done");
        assert!(!run_frame(&ctx, escape_press(), &info));
        let error = MessagePopup::error("KML export", "boom");
        assert!(!run_frame(&ctx, escape_press(), &error));
    }
}
