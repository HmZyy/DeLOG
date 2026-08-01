use delog_core::field_view::SampleMode;

use crate::config::settings::LegendPosition;
use crate::shell::app::commands::{AppCommand, CommandAvailability, CommandId, CommandPresentation};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GlobalPlotControl {
    CursorSampling(SampleMode),
    TogglePlayheadSnap,
    ToggleMeasuringMarker,
    ToggleAllLegends,
    CycleLegendPosition,
    EqualizePlotHeights,
}

#[cfg(test)]
impl GlobalPlotControl {
    pub const ALL: [Self; 6] = [
        Self::CursorSampling(SampleMode::Prev),
        Self::TogglePlayheadSnap,
        Self::ToggleMeasuringMarker,
        Self::ToggleAllLegends,
        Self::CycleLegendPosition,
        Self::EqualizePlotHeights,
    ];
}

pub const fn command_for_control(control: GlobalPlotControl) -> AppCommand {
    match control {
        GlobalPlotControl::CursorSampling(mode) => AppCommand::SetCursorSampling(mode),
        GlobalPlotControl::TogglePlayheadSnap => AppCommand::Static(CommandId::TogglePlayheadSnap),
        GlobalPlotControl::ToggleMeasuringMarker => {
            AppCommand::Static(CommandId::AddMeasuringMarker)
        }
        GlobalPlotControl::ToggleAllLegends => AppCommand::Static(CommandId::ToggleLegends),
        GlobalPlotControl::CycleLegendPosition => AppCommand::Static(CommandId::CycleLegendPosition),
        GlobalPlotControl::EqualizePlotHeights => AppCommand::Static(CommandId::EqualizePlots),
    }
}

pub struct GlobalPlotToolbarModel {
    pub cursor_sampling: SampleMode,
    pub playhead_snap: bool,
    pub measuring_marker: bool,
    pub all_legends_visible: bool,
    pub legend_position: LegendPosition,
}

pub fn show(
    ui: &mut egui::Ui,
    model: &GlobalPlotToolbarModel,
    presentations: &[CommandPresentation],
) -> Vec<AppCommand> {
    let mut commands = Vec::new();
    egui::Frame::group(ui.style()).show(ui, |ui| {
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
                crate::ui::icons::eye_off(),
                "Toggle legends on all plots",
                !model.all_legends_visible,
            )
            .clicked()
            {
                commands.push(command_for_control(GlobalPlotControl::ToggleAllLegends));
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

            if crate::ui::components::icon_button(
                ui,
                crate::ui::icons::grid_2x2_check(),
                "Equalize all plot heights",
                false,
            )
            .clicked()
            {
                commands.push(command_for_control(GlobalPlotControl::EqualizePlotHeights));
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
    fn toolbar_actions_are_all_global() {
        assert_eq!(GlobalPlotControl::ALL.len(), 6);
        assert!(!format!("{:?}", GlobalPlotControl::ALL).contains("Split"));
        assert!(!format!("{:?}", GlobalPlotControl::ALL).contains("FitAll"));
        assert!(GlobalPlotControl::ALL.contains(&GlobalPlotControl::ToggleMeasuringMarker));
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
    fn clicking_a_rendered_toolbar_sampling_choice_emits_the_canonical_command() {
        let ctx = egui::Context::default();
        let model = GlobalPlotToolbarModel {
            cursor_sampling: SampleMode::Prev,
            playhead_snap: false,
            measuring_marker: false,
            all_legends_visible: true,
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
