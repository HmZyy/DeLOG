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
                ui.strong(format!("{NAME} v{VERSION}"));
                ui.label("-");
                ui.hyperlink_to("GitHub", REPOSITORY_URL);
            });
            ui.add_space(ui.spacing().item_spacing.y);
            ui.label(DESCRIPTION);
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
        painted_rows(output)
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    fn painted_rows(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::epaint::Shape::Text(text) => out.push((
                    text.galley.job.text.clone(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                )),
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

    fn rect_of(output: &egui::FullOutput, wanted: &str) -> egui::Rect {
        painted_rows(output)
            .into_iter()
            .find(|(text, _)| text == wanted)
            .unwrap_or_else(|| {
                panic!(
                    "the dialog should paint {wanted:?}, painted {:?}",
                    painted_text(output)
                )
            })
            .1
    }

    #[test]
    fn about_dialog_heads_with_the_name_version_and_github_link_on_one_line() {
        let ctx = egui::Context::default();
        let (output, _) = run_frame(&ctx, frame_input());

        let heading = rect_of(&output, &format!("{NAME} v{VERSION}"));
        let separator = rect_of(&output, "-");
        let link = rect_of(&output, "GitHub");

        for (label, rect) in [("separator", separator), ("link", link)] {
            assert!(
                (rect.center().y - heading.center().y).abs() < 2.0,
                "the {label} should share the heading's line, heading at {} vs {label} at {}",
                heading.center().y,
                rect.center().y
            );
        }
        assert!(
            heading.right() <= separator.left() && separator.right() <= link.left(),
            "the line should read name, separator, link; got {heading:?} {separator:?} {link:?}"
        );
    }

    #[test]
    fn about_dialog_paints_the_description_below_the_heading() {
        let ctx = egui::Context::default();
        let (output, _) = run_frame(&ctx, frame_input());

        let heading = rect_of(&output, &format!("{NAME} v{VERSION}"));
        let description = rect_of(&output, DESCRIPTION);

        assert!(
            description.top() > heading.bottom(),
            "the description should sit below the heading, heading {heading:?} description {description:?}"
        );
        assert!(
            description.left() <= heading.left() + 2.0,
            "the description should start at the heading's margin, heading {} description {}",
            heading.left(),
            description.left()
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
