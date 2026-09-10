pub const NAME: &str = "DeLOG";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");
pub const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

pub fn show(ctx: &egui::Context) -> bool {
    let mut open = true;
    let mut dismissed = false;
    egui::Window::new("About DeLOG")
        .id(egui::Id::new("about-dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Image::new(crate::ui::icons::info())
                        .tint(ui.visuals().text_color())
                        .fit_to_exact_size(egui::vec2(20.0, 20.0)),
                );
                ui.strong(NAME);
                ui.label(format!("v{VERSION}"));
            });
            ui.add_space(ui.spacing().item_spacing.y);
            ui.label(DESCRIPTION);
            ui.add_space(ui.spacing().item_spacing.y);
            ui.hyperlink_to("GitHub", REPOSITORY_URL);
            ui.separator();
            ui.vertical_centered(|ui| {
                if ui.button("Close").clicked() {
                    dismissed = true;
                }
            });
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        dismissed = true;
    }
    open && !dismissed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_frame(ctx: &egui::Context, input: egui::RawInput) -> (egui::FullOutput, bool) {
        let mut open = true;
        let mut output = None;
        for _ in 0..3 {
            output = Some(ctx.run_ui(input.clone(), |ui| {
                open = show(ui.ctx());
            }));
        }
        (output.expect("the dialog should render"), open)
    }

    fn frame_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 600.0),
            )),
            ..Default::default()
        }
    }

    fn painted_text(output: &egui::FullOutput) -> Vec<String> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(text) => out.push(text.galley.job.text.clone()),
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn about_dialog_paints_the_name_and_current_version() {
        let ctx = egui::Context::default();
        let (output, _) = run_frame(&ctx, frame_input());
        let texts = painted_text(&output);

        assert!(
            texts.iter().any(|text| text == NAME),
            "the dialog should name the application, painted {texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains(VERSION)),
            "the dialog should paint version {VERSION}, painted {texts:?}"
        );
    }

    #[test]
    fn about_dialog_paints_the_description() {
        let ctx = egui::Context::default();
        let (output, _) = run_frame(&ctx, frame_input());
        let texts = painted_text(&output);

        assert!(
            texts.iter().any(|text| text == DESCRIPTION),
            "the dialog should paint the package description, painted {texts:?}"
        );
    }

    #[test]
    fn about_dialog_links_to_the_project_on_github() {
        let ctx = egui::Context::default();
        let (output, _) = run_frame(&ctx, frame_input());
        let texts = painted_text(&output);

        assert!(
            REPOSITORY_URL.starts_with("https://github.com/"),
            "the repository should be a GitHub URL, got {REPOSITORY_URL}"
        );
        assert!(
            texts.iter().any(|text| text == "GitHub"),
            "the dialog should offer a GitHub link, painted {texts:?}"
        );
    }

    #[test]
    fn about_dialog_stays_open_without_interaction() {
        let ctx = egui::Context::default();
        let (_, open) = run_frame(&ctx, frame_input());
        assert!(open);
    }

    #[test]
    fn escape_dismisses_the_about_dialog() {
        let ctx = egui::Context::default();
        let mut input = frame_input();
        input.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let (_, open) = run_frame(&ctx, input);
        assert!(!open);
    }
}
