use super::Kind;
use super::place::ArmedTool;

const LIST_MAX_HEIGHT: f32 = 180.0;
const SWATCH_SIZE: f32 = 10.0;
const SWATCH_ROUNDING: f32 = 2.0;

pub struct AnnotationRow {
    pub pane: u64,
    pub plot_label: String,
    pub id: u64,
    pub kind: Kind,
    pub color: egui::Color32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    Edit { pane: u64, id: u64 },
    Remove { pane: u64, id: u64 },
    RemoveAll,
}

pub fn icon_for(kind: Kind) -> egui::ImageSource<'static> {
    match kind {
        Kind::Text => crate::ui::icons::text_cursor(),
        Kind::Segment => crate::ui::icons::slash(),
        Kind::Rect => crate::ui::icons::square(),
        Kind::Ellipse => crate::ui::icons::circle(),
        Kind::HLine => crate::ui::icons::minus(),
    }
}

fn tools(
    ui: &mut egui::Ui,
    armed: &mut Option<ArmedTool>,
    list_state: &mut egui::collapsing_header::CollapsingState,
    has_annotations: bool,
) -> Option<ToolbarAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        for kind in Kind::ALL {
            let selected = armed.map(|tool| tool.kind) == Some(kind);
            let icon = egui::Image::new(icon_for(kind))
                .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
                .tint(ui.visuals().text_color());
            let button = egui::Button::image(icon).selected(selected);
            if ui.add(button).on_hover_text(kind.label()).clicked() {
                *armed = if selected {
                    None
                } else {
                    Some(ArmedTool::new(kind))
                };
            }
        }
        ui.separator();
        let (icon, tooltip) = if list_state.is_open() {
            (crate::ui::icons::chevron_up(), "Hide annotation list")
        } else {
            (crate::ui::icons::chevron_down(), "Show annotation list")
        };
        if crate::ui::components::icon_button(ui, icon, tooltip, false).clicked() {
            list_state.toggle(ui);
        }
        if ui
            .add_enabled(has_annotations, egui::Button::new("Remove all"))
            .clicked()
        {
            action = Some(ToolbarAction::RemoveAll);
        }
    });
    action
}

fn swatch(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(SWATCH_SIZE), egui::Sense::hover());
    ui.painter().rect_filled(rect, SWATCH_ROUNDING, color);
}

fn list(ui: &mut egui::Ui, rows: &[AnnotationRow]) -> Option<ToolbarAction> {
    if rows.is_empty() {
        ui.vertical_centered(|ui| {
            ui.weak("No annotations");
        });
        return None;
    }
    let mut action = None;
    egui::ScrollArea::vertical()
        .max_height(LIST_MAX_HEIGHT)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Grid::new("annotation_list")
                .num_columns(4)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("Type");
                    ui.strong("Color");
                    ui.strong("Plot");
                    ui.strong("Actions");
                    ui.end_row();
                    for row in rows {
                        ui.label(row.kind.label());
                        swatch(ui, row.color);
                        ui.label(&row.plot_label);
                        ui.horizontal(|ui| {
                            if crate::ui::components::icon_button(
                                ui,
                                crate::ui::icons::pencil(),
                                "Edit annotation",
                                false,
                            )
                            .clicked()
                            {
                                action = Some(ToolbarAction::Edit {
                                    pane: row.pane,
                                    id: row.id,
                                });
                            }
                            if crate::ui::components::icon_button(
                                ui,
                                crate::ui::icons::trash(),
                                "Remove annotation",
                                false,
                            )
                            .clicked()
                            {
                                action = Some(ToolbarAction::Remove {
                                    pane: row.pane,
                                    id: row.id,
                                });
                            }
                        });
                        ui.end_row();
                    }
                });
        });
    action
}

pub fn show(
    ctx: &egui::Context,
    open: &mut bool,
    armed: &mut Option<ArmedTool>,
    rows: &[AnnotationRow],
) -> Option<ToolbarAction> {
    let mut action = None;
    egui::Window::new("Annotations")
        .id(egui::Id::new("annotation_toolbar"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let width_id = ui.make_persistent_id("annotation_toolbar_width");
            if let Some(width) = ui.data(|data| data.get_temp::<f32>(width_id)) {
                ui.set_min_width(width);
            }
            let mut list_state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                ui.make_persistent_id("annotation_list_visibility"),
                true,
            );
            action = tools(ui, armed, &mut list_state, !rows.is_empty());
            list_state.show_body_unindented(ui, |ui| {
                ui.separator();
                action = action.or(list(ui, rows));
            });
            let width = ui.min_rect().width();
            ui.data_mut(|data| data.insert_temp(width_id, width));
        });
    if !*open {
        *armed = None;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        }
    }

    fn row(pane: u64, id: u64, kind: Kind) -> AnnotationRow {
        AnnotationRow {
            pane,
            plot_label: format!("Plot {pane}"),
            id,
            kind,
            color: egui::Color32::RED,
        }
    }

    #[test]
    fn closing_the_toolbar_disarms_the_active_tool() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let mut open = true;
        let mut armed = Some(ArmedTool::new(Kind::Text));
        let _ = ctx.run_ui(raw_input(), |ui| {
            show(ui.ctx(), &mut open, &mut armed, &[]);
        });
        assert_eq!(armed, Some(ArmedTool::new(Kind::Text)));

        open = false;
        let _ = ctx.run_ui(raw_input(), |ui| {
            show(ui.ctx(), &mut open, &mut armed, &[]);
        });
        assert_eq!(armed, None);
    }

    #[test]
    fn an_empty_list_offers_no_action() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let mut open = true;
        let mut armed = None;
        let mut action = None;
        let _ = ctx.run_ui(raw_input(), |ui| {
            action = show(ui.ctx(), &mut open, &mut armed, &[]);
        });
        assert_eq!(action, None);
    }

    #[test]
    fn a_populated_list_renders_without_offering_an_unrequested_action() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let mut open = true;
        let mut armed = None;
        let rows = vec![row(1, 0, Kind::Rect), row(2, 0, Kind::HLine)];
        let mut action = None;
        let _ = ctx.run_ui(raw_input(), |ui| {
            action = show(ui.ctx(), &mut open, &mut armed, &rows);
        });
        assert_eq!(action, None, "no action without a click");
    }

    #[test]
    fn every_kind_maps_to_a_distinct_icon() {
        let sources: Vec<String> = Kind::ALL
            .iter()
            .map(|kind| format!("{:?}", icon_for(*kind)))
            .collect();
        for (index, source) in sources.iter().enumerate() {
            for other in &sources[index + 1..] {
                assert_ne!(source, other, "two kinds share an icon");
            }
        }
    }
}
