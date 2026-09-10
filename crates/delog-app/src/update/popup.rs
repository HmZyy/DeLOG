use super::{CURRENT_VERSION, Release};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    RemindLater,
    SkipVersion,
    DisableChecks,
}

pub fn show(ctx: &egui::Context, release: &Release) -> Option<UpdateAction> {
    let mut open = true;
    let mut action = None;
    egui::Window::new("Update available")
        .id(egui::Id::new("update-popup"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .show(ctx, |ui| {
            ui.strong(format!("DeLOG v{} is available", release.version));
            ui.label(format!("You are running v{CURRENT_VERSION}."));
            ui.add_space(ui.spacing().item_spacing.y);
            ui.hyperlink_to("View release", &release.url);
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .button("Skip this version")
                    .on_hover_text(format!(
                        "Do not mention v{} again. A later version will still be offered.",
                        release.version
                    ))
                    .clicked()
                {
                    action = Some(UpdateAction::SkipVersion);
                }
                if ui
                    .button("Stop checking for updates")
                    .on_hover_text(
                        "Never check for updates again. Re-enable it under Settings > General.",
                    )
                    .clicked()
                {
                    action = Some(UpdateAction::DisableChecks);
                }
            });
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return Some(UpdateAction::RemindLater);
    }
    if !open {
        return Some(UpdateAction::RemindLater);
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release() -> Release {
        Release {
            version: "9.9.9".to_owned(),
            url: "https://github.com/HmZyy/DeLOG/releases/tag/v9.9.9".to_owned(),
        }
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

    fn settled(ctx: &egui::Context, release: &Release) -> egui::FullOutput {
        let mut output = None;
        for _ in 0..3 {
            output = Some(ctx.run_ui(frame_input(), |ui| {
                show(ui.ctx(), release);
            }));
        }
        output.expect("the popup should render")
    }

    fn frame(
        ctx: &egui::Context,
        release: &Release,
        events: Vec<egui::Event>,
    ) -> Option<UpdateAction> {
        let mut action = None;
        let _ = ctx.run_ui(
            egui::RawInput {
                events,
                ..frame_input()
            },
            |ui| {
                action = show(ui.ctx(), release);
            },
        );
        action
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

    fn click(ctx: &egui::Context, release: &Release, label: &str) -> Option<UpdateAction> {
        let output = settled(ctx, release);
        let update = output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("accessibility tree should be emitted");
        let button = update
            .nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.label() == Some(label))
            .unwrap_or_else(|| {
                panic!(
                    "the popup should offer a {label:?} button, painted {:?}",
                    painted_text(&output)
                )
            });
        let bounds = button.bounds().expect("the button should have bounds");
        let pos = egui::pos2(
            ((bounds.x0 + bounds.x1) * 0.5) as f32,
            ((bounds.y0 + bounds.y1) * 0.5) as f32,
        );
        frame(
            ctx,
            release,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        frame(
            ctx,
            release,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
    }

    fn accesskit_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        ctx.enable_accesskit();
        ctx
    }

    #[test]
    fn the_popup_names_the_new_and_the_running_version() {
        let ctx = egui::Context::default();
        let texts = painted_text(&settled(&ctx, &release()));

        assert!(
            texts.iter().any(|text| text.contains("9.9.9")),
            "the popup should name the available version, painted {texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains(CURRENT_VERSION)),
            "the popup should name the running version {CURRENT_VERSION}, painted {texts:?}"
        );
    }

    #[test]
    fn the_popup_links_to_the_release() {
        let ctx = egui::Context::default();
        let output = settled(&ctx, &release());
        let texts = painted_text(&output);

        assert!(
            texts.iter().any(|text| text == "View release"),
            "the popup should link to the release, painted {texts:?}"
        );
    }

    #[test]
    fn waiting_without_interaction_decides_nothing() {
        let ctx = egui::Context::default();
        assert_eq!(frame(&ctx, &release(), Vec::new()), None);
    }

    #[test]
    fn skipping_this_version_reports_that_choice() {
        let ctx = accesskit_ctx();
        assert_eq!(
            click(&ctx, &release(), "Skip this version"),
            Some(UpdateAction::SkipVersion)
        );
    }

    #[test]
    fn stopping_the_checks_reports_that_choice() {
        let ctx = accesskit_ctx();
        assert_eq!(
            click(&ctx, &release(), "Stop checking for updates"),
            Some(UpdateAction::DisableChecks)
        );
    }

    #[test]
    fn escape_asks_to_be_reminded_later() {
        let ctx = egui::Context::default();
        let _ = settled(&ctx, &release());
        let escape = vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }];

        assert_eq!(
            frame(&ctx, &release(), escape),
            Some(UpdateAction::RemindLater)
        );
    }
}
