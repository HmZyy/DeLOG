use delog_core::field_view::SampleMode;

use crate::config::settings::LegendPosition;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GlobalPlotToolbarAction {
    FitAll,
    SetCursorSampling(SampleMode),
    TogglePlayheadSnap,
    ToggleAllLegends,
    CycleLegendPosition,
    EqualizePlotHeights,
}

#[cfg(test)]
impl GlobalPlotToolbarAction {
    pub const ALL: [Self; 6] = [
        Self::FitAll,
        Self::SetCursorSampling(SampleMode::Prev),
        Self::TogglePlayheadSnap,
        Self::ToggleAllLegends,
        Self::CycleLegendPosition,
        Self::EqualizePlotHeights,
    ];
}

pub struct GlobalPlotToolbarModel {
    pub cursor_sampling: SampleMode,
    pub playhead_snap: bool,
    pub all_legends_visible: bool,
    pub legend_position: LegendPosition,
}

pub fn show(ui: &mut egui::Ui, model: &GlobalPlotToolbarModel) -> Vec<GlobalPlotToolbarAction> {
    let mut actions = Vec::new();
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            if crate::ui::components::icon_button(
                ui,
                crate::ui::icons::maximize(),
                "Fit all plots",
                false,
            )
            .clicked()
            {
                actions.push(GlobalPlotToolbarAction::FitAll);
            }

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
                        actions.push(GlobalPlotToolbarAction::SetCursorSampling(mode));
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
                actions.push(GlobalPlotToolbarAction::TogglePlayheadSnap);
            }

            if crate::ui::components::icon_button(
                ui,
                crate::ui::icons::eye_off(),
                "Toggle legends on all plots",
                !model.all_legends_visible,
            )
            .clicked()
            {
                actions.push(GlobalPlotToolbarAction::ToggleAllLegends);
            }

            if crate::ui::components::icon_button(
                ui,
                legend_position_icon(model.legend_position),
                "Cycle legend position on all plots",
                false,
            )
            .clicked()
            {
                actions.push(GlobalPlotToolbarAction::CycleLegendPosition);
            }

            if crate::ui::components::icon_button(
                ui,
                crate::ui::icons::grid_2x2_check(),
                "Equalize all plot heights",
                false,
            )
            .clicked()
            {
                actions.push(GlobalPlotToolbarAction::EqualizePlotHeights);
            }

            ui.weak("X axes linked");
        });
    });
    actions
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

    #[test]
    fn toolbar_actions_are_all_global() {
        assert_eq!(GlobalPlotToolbarAction::ALL.len(), 6);
        assert!(!format!("{:?}", GlobalPlotToolbarAction::ALL).contains("Split"));
        assert!(!format!("{:?}", GlobalPlotToolbarAction::ALL).contains("Marker"));
    }
}
