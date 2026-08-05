use delog_core::field_view::SampleMode;

use crate::config::settings::LegendPosition;
use crate::shell::app::commands::{AppCommand, CommandAvailability, CommandId, CommandPresentation};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GlobalPlotControl {
    CursorSampling(SampleMode),
    TogglePlayheadSnap,
    ToggleMeasuringMarker,
    CycleLegendPosition,
    OpenFieldStats,
}

#[cfg(test)]
impl GlobalPlotControl {
    pub const ALL: [Self; 5] = [
        Self::CursorSampling(SampleMode::Prev),
        Self::TogglePlayheadSnap,
        Self::ToggleMeasuringMarker,
        Self::CycleLegendPosition,
        Self::OpenFieldStats,
    ];
}

pub const fn command_for_control(control: GlobalPlotControl) -> AppCommand {
    match control {
        GlobalPlotControl::CursorSampling(mode) => AppCommand::SetCursorSampling(mode),
        GlobalPlotControl::TogglePlayheadSnap => AppCommand::Static(CommandId::TogglePlayheadSnap),
        GlobalPlotControl::ToggleMeasuringMarker => {
            AppCommand::Static(CommandId::AddMeasuringMarker)
        }
        GlobalPlotControl::CycleLegendPosition => AppCommand::Static(CommandId::CycleLegendPosition),
        GlobalPlotControl::OpenFieldStats => AppCommand::Static(CommandId::OpenFieldStats),
    }
}

pub struct GlobalPlotToolbarModel {
    pub cursor_sampling: SampleMode,
    pub playhead_snap: bool,
    pub measuring_marker: bool,
    pub legend_position: LegendPosition,
}

fn toolbar_container_frame(_style: &egui::Style) -> egui::Frame {
    egui::Frame::NONE
}

pub fn show(
    ui: &mut egui::Ui,
    model: &GlobalPlotToolbarModel,
    presentations: &[CommandPresentation],
) -> Vec<AppCommand> {
    let mut commands = Vec::new();
    toolbar_container_frame(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            let cursor_icon = egui::Image::new(crate::ui::icons::mouse_pointer())
                .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
                .tint(ui.visuals().text_color());
            egui::containers::menu::MenuButton::from_button(egui::Button::image_and_text(
                cursor_icon,
                format!("Cursor: {}", sample_mode_label(model.cursor_sampling)),
            ))
            .ui(ui, |ui| {
                for mode in [SampleMode::Prev, SampleMode::Next, SampleMode::Linear] {
                    if ui
                        .selectable_label(model.cursor_sampling == mode, sample_mode_label(mode))
                        .clicked()
                    {
                        commands.push(command_for_control(GlobalPlotControl::CursorSampling(
                            mode,
                        )));
                        ui.close();
                    }
                }
            });

            if crate::ui::components::icon_button(
                ui,
                crate::ui::icons::magnet(),
                "Toggle playhead snap on all plots",
                model.playhead_snap,
            )
            .clicked()
            {
                commands.push(command_for_control(GlobalPlotControl::TogglePlayheadSnap));
            }

            let marker = command_for_control(GlobalPlotControl::ToggleMeasuringMarker);
            let marker_presentation = presentations
                .iter()
                .find(|presentation| presentation.command == marker);
            let marker_enabled = marker_presentation.is_none_or(|presentation| {
                presentation.availability == CommandAvailability::Enabled
            });
            let marker_response = ui
                .add_enabled_ui(marker_enabled, |ui| {
                    crate::ui::components::icon_button(
                        ui,
                        crate::ui::icons::ruler(),
                        marker_presentation
                            .map(|presentation| presentation.label.as_str())
                            .unwrap_or("Toggle measuring marker"),
                        marker_presentation
                            .and_then(|presentation| presentation.selected)
                            .unwrap_or(model.measuring_marker),
                    )
                })
                .inner;
            let marker_response = match marker_presentation
                .map(|presentation| &presentation.availability)
            {
                Some(CommandAvailability::Disabled(reason)) => {
                    marker_response.on_disabled_hover_text(*reason)
                }
                _ => marker_response,
            };
            if marker_response.clicked() {
                commands.push(marker);
            }

            if crate::ui::components::icon_button(
                ui,
                legend_position_icon(model.legend_position),
                "Cycle legend position on all plots",
                false,
            )
            .clicked()
            {
                commands.push(command_for_control(GlobalPlotControl::CycleLegendPosition));
            }

            let stats = command_for_control(GlobalPlotControl::OpenFieldStats);
            let stats_presentation = presentations
                .iter()
                .find(|presentation| presentation.command == stats);
            let stats_enabled = stats_presentation.is_none_or(|presentation| {
                presentation.availability == CommandAvailability::Enabled
            });
            let stats_response = ui
                .add_enabled_ui(stats_enabled, |ui| {
                    crate::ui::components::icon_button(
                        ui,
                        crate::ui::icons::sigma(),
                        "Field stats for every plotted trace",
                        false,
                    )
                })
                .inner;
            let stats_response = match stats_presentation
                .map(|presentation| &presentation.availability)
            {
                Some(CommandAvailability::Disabled(reason)) => {
                    stats_response.on_disabled_hover_text(*reason)
                }
                _ => stats_response,
            };
            if stats_response.clicked() {
                commands.push(stats);
            }

        });
    });
    commands
}

fn sample_mode_label(mode: SampleMode) -> &'static str {
    match mode {
        SampleMode::Prev => "Previous",
        SampleMode::Next => "Next",
        SampleMode::Linear => "Linear",
    }
}

fn legend_position_icon(position: LegendPosition) -> egui::ImageSource<'static> {
    match position {
        LegendPosition::TopLeft => crate::ui::icons::dice_top_left(),
        LegendPosition::TopRight => crate::ui::icons::dice_top_right(),
        LegendPosition::BottomLeft => crate::ui::icons::dice_bottom_left(),
        LegendPosition::BottomRight => crate::ui::icons::dice_bottom_right(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_text_rect(shape: &egui::epaint::Shape, expected: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == expected => {
                Some(text.visual_bounding_rect())
            }
            egui::epaint::Shape::Vec(shapes) => shapes
                .iter()
                .find_map(|shape| find_text_rect(shape, expected)),
            _ => None,
        }
    }

    fn toolbar_frame(
        ctx: &egui::Context,
        model: &GlobalPlotToolbarModel,
        presentations: &[CommandPresentation],
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<AppCommand>) {
        let mut commands = Vec::new();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 300.0),
                )),
                events,
                ..Default::default()
            },
            |ui| commands = show(ui, model, presentations),
        );
        (output, commands)
    }

    fn click_events(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn toolbar_builds_and_the_sigma_icon_follows_the_tint_convention() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let model = GlobalPlotToolbarModel {
            cursor_sampling: SampleMode::Prev,
            playhead_snap: false,
            measuring_marker: false,
            legend_position: LegendPosition::TopRight,
        };
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_200.0, 200.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(input(), |ui| {
            show(ui, &model, &[]);
        });
        let _ = ctx.run_ui(input(), |ui| {
            show(ui, &model, &[]);
        });

        ctx.forget_all_images();
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/sigma.svg"
        ))
        .expect("the sigma icon should be bundled");
        let text = String::from_utf8(bytes).expect("svg is utf8");
        assert!(
            text.contains("stroke=\"#ffffff\""),
            "icons must use a white stroke so the runtime tint colors them"
        );
        assert!(text.contains("<path"), "the sigma icon should have geometry");
    }

    #[test]
    fn toolbar_actions_are_all_global() {
        assert_eq!(GlobalPlotControl::ALL.len(), 5);
        assert!(!format!("{:?}", GlobalPlotControl::ALL).contains("Split"));
        assert!(!format!("{:?}", GlobalPlotControl::ALL).contains("FitAll"));
        assert!(!format!("{:?}", GlobalPlotControl::ALL).contains("ToggleAllLegends"));
        assert!(!format!("{:?}", GlobalPlotControl::ALL).contains("EqualizePlotHeights"));
        assert!(GlobalPlotControl::ALL.contains(&GlobalPlotControl::ToggleMeasuringMarker));
        assert!(GlobalPlotControl::ALL.contains(&GlobalPlotControl::OpenFieldStats));
    }

    #[test]
    fn removed_toolbar_controls_keep_palette_commands() {
        for id in [CommandId::ToggleLegends, CommandId::EqualizePlots] {
            let routes = id.spec().routes;
            assert!(routes.contains(&crate::shell::app::commands::AccessRoute::Palette));
            assert!(!routes.contains(&crate::shell::app::commands::AccessRoute::GlobalToolbar));
        }
    }

    #[test]
    fn toolbar_controls_emit_the_canonical_app_commands() {
        assert_eq!(
            command_for_control(GlobalPlotControl::ToggleMeasuringMarker),
            crate::shell::app::commands::AppCommand::Static(
                crate::shell::app::commands::CommandId::AddMeasuringMarker
            )
        );
        assert_eq!(
            command_for_control(GlobalPlotControl::CursorSampling(SampleMode::Linear)),
            crate::shell::app::commands::AppCommand::SetCursorSampling(SampleMode::Linear)
        );
        assert_eq!(
            command_for_control(GlobalPlotControl::TogglePlayheadSnap),
            crate::shell::app::commands::AppCommand::Static(
                crate::shell::app::commands::CommandId::TogglePlayheadSnap
            )
        );
    }

    #[test]
    fn toolbar_frame_adds_no_outline_or_padding_inside_the_header_row() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let frame = toolbar_container_frame(&ctx.global_style());
        assert_eq!(frame.stroke, egui::Stroke::NONE);
        assert_eq!(frame.inner_margin, egui::Margin::ZERO);
        assert_eq!(frame.outer_margin, egui::Margin::ZERO);

        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            toolbar_container_frame(ui.style()).show(ui, |ui| {
                ui.label("Inset toolbar");
            });
            egui::Frame::NONE.show(ui, |ui| {
                ui.label("Baseline toolbar");
            });
        });
        let inset_left = output
            .shapes
            .iter()
            .find_map(|shape| find_text_rect(&shape.shape, "Inset toolbar"))
            .expect("inset toolbar label should be painted")
            .left();
        let baseline_left = output
            .shapes
            .iter()
            .find_map(|shape| find_text_rect(&shape.shape, "Baseline toolbar"))
            .expect("baseline toolbar label should be painted")
            .left();

        assert_eq!(inset_left, baseline_left);
    }

    #[test]
    fn clicking_a_rendered_toolbar_sampling_choice_emits_the_canonical_command() {
        let ctx = egui::Context::default();
        let model = GlobalPlotToolbarModel {
            cursor_sampling: SampleMode::Prev,
            playhead_snap: false,
            measuring_marker: false,
            legend_position: LegendPosition::TopLeft,
        };
        let presentations = crate::shell::app::commands::present_commands(
            &crate::shell::app::commands::CommandContext::default(),
            &crate::shell::app::commands::PresentationState::default(),
            [],
        );

        let _ = toolbar_frame(&ctx, &model, &presentations, vec![]);
        let (output, _) = toolbar_frame(&ctx, &model, &presentations, vec![]);
        let cursor_rect = output
            .shapes
            .iter()
            .find_map(|shape| find_text_rect(&shape.shape, "Cursor: Previous"))
            .expect("cursor toolbar menu should be painted");
        let cursor_pos = cursor_rect.center();
        let _ = toolbar_frame(
            &ctx,
            &model,
            &presentations,
            click_events(cursor_pos, true),
        );
        let _ = toolbar_frame(
            &ctx,
            &model,
            &presentations,
            click_events(cursor_pos, false),
        );
        let (output, _) = toolbar_frame(&ctx, &model, &presentations, vec![]);
        let linear_rect = output
            .shapes
            .iter()
            .find_map(|shape| find_text_rect(&shape.shape, "Linear"))
            .expect("sampling menu choice should be painted");
        let linear_pos = linear_rect.center();
        let _ = toolbar_frame(
            &ctx,
            &model,
            &presentations,
            click_events(linear_pos, true),
        );
        let (_, commands) = toolbar_frame(
            &ctx,
            &model,
            &presentations,
            click_events(linear_pos, false),
        );

        assert_eq!(commands, [AppCommand::SetCursorSampling(SampleMode::Linear)]);
    }
}
