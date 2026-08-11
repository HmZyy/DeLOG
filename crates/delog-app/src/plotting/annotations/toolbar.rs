use super::Kind;
use super::place::ArmedTool;

pub fn icon_for(kind: Kind) -> egui::ImageSource<'static> {
    match kind {
        Kind::Text => crate::ui::icons::text_cursor(),
        Kind::Segment => crate::ui::icons::arrow_right(),
        Kind::Rect => crate::ui::icons::square(),
        Kind::Ellipse => crate::ui::icons::circle(),
        Kind::HLine => crate::ui::icons::minus(),
    }
}

pub fn show(ctx: &egui::Context, open: &mut bool, armed: &mut Option<ArmedTool>) {
    egui::Window::new("Annotations")
        .id(egui::Id::new("annotation_toolbar"))
        .open(open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for kind in Kind::ALL {
                    let selected = armed.map(|tool| tool.kind) == Some(kind);
                    let icon = egui::Image::new(icon_for(kind))
                        .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
                        .tint(ui.visuals().text_color());
                    let button = egui::Button::image(icon).selected(selected);
                    if ui.add(button).on_hover_text(kind.label()).clicked() {
                        *armed = if selected { None } else { Some(ArmedTool::new(kind)) };
                    }
                }
            });
        });
    if !*open {
        *armed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_toolbar_disarms_the_active_tool() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let mut open = true;
        let mut armed = Some(ArmedTool::new(Kind::Text));
        let raw_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(raw_input(), |ui| show(ui.ctx(), &mut open, &mut armed));
        assert_eq!(armed, Some(ArmedTool::new(Kind::Text)));

        open = false;
        let _ = ctx.run_ui(raw_input(), |ui| show(ui.ctx(), &mut open, &mut armed));
        assert_eq!(armed, None);
    }
}
