use crate::shell::app::commands::{
    AppCommand, ClassicMenuOwner, CommandAvailability, CommandId, CommandPresentation,
};
use crate::ui::components;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellEmphasis {
    #[default]
    Offline,
    Live,
}

impl ShellEmphasis {
    pub const fn toggle(self) -> Self {
        match self {
            Self::Offline => Self::Live,
            Self::Live => Self::Offline,
        }
    }
}

pub struct HeaderModel {
    pub emphasis: ShellEmphasis,
    pub live_statuses: Vec<LiveSummary>,
    pub load: LoadStatusView,
    pub fps: Option<f32>,
    pub theme: crate::ui::theme::ThemeChoice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSummary {
    pub index: usize,
    pub endpoint: String,
    pub state: String,
    pub rx_frames: u64,
    pub rows: u64,
    pub recording: Option<String>,
}

pub enum LoadStatusView {
    Idle,
    Active {
        label: String,
        progress: Option<f32>,
    },
}

pub struct HeaderOutput {
    pub commands: Vec<AppCommand>,
    pub refresh_dynamic_catalog: bool,
}

#[cfg(test)]
const CLASSIC_MENU_TITLES: &[&str] = &["File", "View", "Analyze", "Tools"];

const FILE_MENU: &[CommandId] = &[
    CommandId::Open,
    CommandId::ConnectLive,
    CommandId::CancelTasks,
];
const FILE_EXPORT_MENU: &[CommandId] = &[
    CommandId::ExportData,
    CommandId::ExportDiagnostics,
    CommandId::ExportProfiling,
    CommandId::ExportWorkspacePng,
];
const VIEW_MENU: &[CommandId] = &[
    CommandId::ToggleDataBrowser,
    CommandId::ToggleInspector,
    CommandId::ToggleScene3d,
];
const VIEW_PANELS_MENU: &[CommandId] = &[
    CommandId::OpenDiagnostics,
    CommandId::OpenPerformance,
    CommandId::OpenMarkers,
    CommandId::OpenScripting,
    CommandId::OpenLogging,
];
const TOOLS_LAYOUTS_MENU: &[CommandId] = &[
    CommandId::SaveLayout,
    CommandId::ManageLayouts,
    CommandId::ImportLayout,
    CommandId::ExportLayout,
    CommandId::ClearLayout,
];
const ANALYZE_MENU: &[CommandId] = &[
    CommandId::SyncSources,
    CommandId::OpenDataFlow,
];
const TOOLS_MENU: &[CommandId] = &[CommandId::OpenSettings];
const TOOLS_SCRIPTS_MENU: &[CommandId] = &[
    CommandId::OpenScriptEditor,
    CommandId::OpenScriptVariables,
];
const TOOLS_PARSERS_MENU: &[CommandId] = &[CommandId::OpenParserEditor];

fn header_bottom_margin(style: &egui::Style) -> f32 {
    crate::ui::design_tokens::DesignTokens::from_style(style).space_xs
}

#[cfg(test)]
pub(crate) fn classic_menu_command_ids() -> Vec<CommandId> {
    [
        FILE_MENU,
        FILE_EXPORT_MENU,
        VIEW_MENU,
        VIEW_PANELS_MENU,
        ANALYZE_MENU,
        TOOLS_MENU,
        TOOLS_SCRIPTS_MENU,
        TOOLS_PARSERS_MENU,
        TOOLS_LAYOUTS_MENU,
    ]
    .into_iter()
    .flatten()
    .copied()
    .chain([CommandId::Exit])
    .collect()
}

pub fn show(
    ui: &mut egui::Ui,
    model: &HeaderModel,
    presentations: &[CommandPresentation],
    show_toolbar: impl FnOnce(&mut egui::Ui) -> Vec<AppCommand>,
) -> HeaderOutput {
    let mut commands = Vec::new();
    let mut refresh_dynamic_catalog = false;
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.strong("DeLOG");
            ui.separator();
            let offline = ui
                .selectable_label(model.emphasis == ShellEmphasis::Offline, "Offline")
                .on_hover_text("Prioritize file-based analysis tools");
            let live = ui
                .selectable_label(model.emphasis == ShellEmphasis::Live, "Live")
                .on_hover_text("Prioritize live telemetry tools");
            if (offline.clicked() && model.emphasis != ShellEmphasis::Offline)
                || (live.clicked() && model.emphasis != ShellEmphasis::Live)
            {
                commands.push(AppCommand::ToggleShellEmphasis);
            }
            ui.separator();
            let primary = match model.emphasis {
                ShellEmphasis::Offline => CommandId::Open,
                ShellEmphasis::Live => CommandId::ConnectLive,
            };
            if let Some(presentation) = static_presentation(presentations, primary)
                && components::icon_text_button(
                    ui,
                    match model.emphasis {
                        ShellEmphasis::Offline => crate::ui::icons::folder_open(),
                        ShellEmphasis::Live => crate::ui::icons::satellite_dish(),
                    },
                    &presentation.label,
                    false,
                )
                .clicked()
            {
                commands.push(presentation.command.clone());
            }
            if crate::ui::components::icon_button(
                ui,
                crate::ui::icons::cube(),
                "Show or hide the 3D scene",
                false,
            )
            .clicked()
            {
                commands.push(AppCommand::Static(CommandId::ToggleScene3d));
            }
            for status in &model.live_statuses {
                let detail = format!("{} · {} rows", status.state, status.rows);
                let lowercase = status.state.to_ascii_lowercase();
                let state = if lowercase.contains("connected") {
                    components::StatusState::Success
                } else if lowercase.contains("connect") || lowercase.contains("wait") {
                    components::StatusState::Warning
                } else if lowercase.contains("error") || lowercase.contains("fail") {
                    components::StatusState::Error
                } else {
                    components::StatusState::Neutral
                };
                let chip = components::StatusChip {
                    label: status.endpoint.clone(),
                    detail: Some(detail),
                    state,
                };
                components::status_chip(ui, &chip, model.theme).on_hover_text(format!(
                        "{} received frames{}",
                        status.rx_frames,
                        status
                            .recording
                            .as_deref()
                            .map(|value| format!(" · {value}"))
                            .unwrap_or_default()
                    ));
                if components::icon_button(
                    ui,
                    crate::ui::icons::unplug(),
                    &format!("Disconnect {}", status.endpoint),
                    false,
                )
                .clicked()
                {
                    commands.push(AppCommand::DisconnectLink(status.index));
                }
            }
            if let LoadStatusView::Active { label, progress } = &model.load {
                ui.separator();
                ui.label(label);
                if let Some(progress) = progress {
                    let bar = ui.add(
                        egui::ProgressBar::new(*progress)
                            .desired_width(180.0)
                            .desired_height(14.0),
                    );
                    ui.painter().text(
                        bar.rect.center(),
                        egui::Align2::CENTER_CENTER,
                        format!("{}%", (progress * 100.0) as usize),
                        egui::TextStyle::Button.resolve(ui.style()),
                        ui.visuals().selection.stroke.color,
                    );
                } else {
                    ui.spinner();
                }
                if ui.small_button("Cancel").clicked() {
                    commands.push(AppCommand::Static(CommandId::CancelTasks));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(fps) = model.fps {
                    ui.weak(format!("{fps:.0} FPS"));
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.menu_button("File", |ui| {
                menu_items(
                    ui,
                    ClassicMenuOwner::File,
                    FILE_MENU,
                    presentations,
                    &mut commands,
                );
                ui.menu_button("Open With", |ui| {
                    dynamic_rows(
                        ui,
                        ClassicMenuOwner::File,
                        presentations,
                        &mut commands,
                        |command| matches!(command, AppCommand::OpenWithBuiltInParser(_)),
                    );
                });
                ui.menu_button("Export", |ui| {
                    menu_items(
                        ui,
                        ClassicMenuOwner::File,
                        FILE_EXPORT_MENU,
                        presentations,
                        &mut commands,
                    );
                });
                ui.separator();
                menu_item(ui, CommandId::Exit, presentations, &mut commands);
            });
            ui.menu_button("View", |ui| {
                checked_menu_items(
                    ui,
                    ClassicMenuOwner::View,
                    VIEW_MENU,
                    presentations,
                    &mut commands,
                );
                ui.menu_button("Panels", |ui| {
                    checked_menu_items(
                        ui,
                        ClassicMenuOwner::View,
                        VIEW_PANELS_MENU,
                        presentations,
                        &mut commands,
                    );
                });
            });
            ui.menu_button("Analyze", |ui| {
                menu_items(
                    ui,
                    ClassicMenuOwner::Analyze,
                    ANALYZE_MENU,
                    presentations,
                    &mut commands,
                );
            });
            let tools_menu = ui.menu_button("Tools", |ui| {
                ui.menu_button("Scripts", |ui| {
                    ui.menu_button("Run Scripts", |ui| {
                        dynamic_rows(
                            ui,
                            ClassicMenuOwner::Tools,
                            presentations,
                            &mut commands,
                            |command| matches!(command, AppCommand::RunScript(_)),
                        );
                    });
                    menu_items(
                        ui,
                        ClassicMenuOwner::Tools,
                        TOOLS_SCRIPTS_MENU,
                        presentations,
                        &mut commands,
                    );
                });
                ui.menu_button("Parsers", |ui| {
                    menu_items(
                        ui,
                        ClassicMenuOwner::Tools,
                        TOOLS_PARSERS_MENU,
                        presentations,
                        &mut commands,
                    );
                    ui.menu_button("Run Parser", |ui| {
                        dynamic_rows(
                            ui,
                            ClassicMenuOwner::Tools,
                            presentations,
                            &mut commands,
                            |command| matches!(command, AppCommand::OpenWithParser(_)),
                        );
                    });
                });
                ui.menu_button("Layouts", |ui| {
                    menu_item(ui, CommandId::SaveLayout, presentations, &mut commands);
                    ui.menu_button("Load Layout", |ui| {
                        dynamic_rows(
                            ui,
                            ClassicMenuOwner::Tools,
                            presentations,
                            &mut commands,
                            |command| matches!(command, AppCommand::LoadNamedLayout(_)),
                        );
                    });
                    menu_items(
                        ui,
                        ClassicMenuOwner::Tools,
                        &TOOLS_LAYOUTS_MENU[1..],
                        presentations,
                        &mut commands,
                    );
                });
                menu_items(
                    ui,
                    ClassicMenuOwner::Tools,
                    TOOLS_MENU,
                    presentations,
                    &mut commands,
                );
            });
            refresh_dynamic_catalog |= tools_menu.response.clicked();
            ui.separator();
            commands.extend(show_toolbar(ui));
        });
        ui.add_space(header_bottom_margin(ui.style()));
    });
    HeaderOutput {
        commands,
        refresh_dynamic_catalog,
    }
}

fn menu_items(
    ui: &mut egui::Ui,
    owner: ClassicMenuOwner,
    ids: &[CommandId],
    presentations: &[CommandPresentation],
    selected: &mut Vec<AppCommand>,
) {
    debug_assert!(
        ids.iter()
            .all(|id| id.spec().classic_menu_owner == owner),
        "classic menu section contains a command owned by another menu"
    );
    for id in ids {
        menu_item(ui, *id, presentations, selected);
    }
}

fn checked_menu_items(
    ui: &mut egui::Ui,
    owner: ClassicMenuOwner,
    ids: &[CommandId],
    presentations: &[CommandPresentation],
    selected: &mut Vec<AppCommand>,
) {
    debug_assert!(ids.iter().all(|id| id.classic_menu_owner() == owner));
    for id in ids {
        if let Some(presentation) = static_presentation(presentations, *id) {
            presentation_row(ui, presentation, true, selected);
        }
    }
}

fn dynamic_rows(
    ui: &mut egui::Ui,
    owner: ClassicMenuOwner,
    presentations: &[CommandPresentation],
    selected: &mut Vec<AppCommand>,
    matches_command: impl Fn(&AppCommand) -> bool,
) {
    let matching = presentations
        .iter()
        .filter(|presentation| matches_command(&presentation.command));
    for presentation in matching {
        debug_assert_eq!(presentation.command.classic_menu_owner(), owner);
        presentation_row(ui, presentation, false, selected);
    }
}

fn menu_item(
    ui: &mut egui::Ui,
    id: CommandId,
    presentations: &[CommandPresentation],
    selected: &mut Vec<AppCommand>,
) {
    if let Some(presentation) = static_presentation(presentations, id) {
        presentation_row(ui, presentation, false, selected);
    }
}

fn presentation_row(
    ui: &mut egui::Ui,
    presentation: &CommandPresentation,
    checked: bool,
    selected: &mut Vec<AppCommand>,
) {
    let (enabled, reason) = match presentation.availability {
        CommandAvailability::Enabled => (true, None),
        CommandAvailability::Disabled(reason) => (false, Some(reason)),
    };
    let response = if checked {
        let mut is_selected = presentation.selected.unwrap_or(false);
        let text = presentation.shortcut.map_or_else(
            || presentation.label.clone(),
            |shortcut| format!("{}\t{shortcut}", presentation.label),
        );
        let response = ui.add_enabled(
            enabled,
            egui::Checkbox::new(&mut is_selected, text),
        );
        match reason {
            Some(reason) => response.on_disabled_hover_text(reason),
            None => response,
        }
    } else {
        components::menu_row(
            ui,
            &presentation.label,
            presentation.shortcut,
            enabled,
            reason,
        )
    };
    if response.clicked() {
        selected.push(presentation.command.clone());
        ui.close();
    }
}

fn static_presentation(
    presentations: &[CommandPresentation],
    id: CommandId,
) -> Option<&CommandPresentation> {
    presentations
        .iter()
        .find(|presentation| presentation.command == AppCommand::Static(id))
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

    fn checked_row_frame(
        ctx: &egui::Context,
        presentation: &CommandPresentation,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<AppCommand>) {
        let mut selected = Vec::new();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                events,
                ..Default::default()
            },
            |ui| presentation_row(ui, presentation, true, &mut selected),
        );
        (output, selected)
    }

    #[test]
    fn parsing_progress_bar_is_slim_wide_and_labelled() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let tokens = crate::ui::design_tokens::DesignTokens::default();
        let model = HeaderModel {
            emphasis: ShellEmphasis::Offline,
            live_statuses: Vec::new(),
            load: LoadStatusView::Active {
                label: "Parsing flight.bin".to_owned(),
                progress: Some(0.42),
            },
            fps: None,
            theme: crate::ui::theme::ThemeChoice::CatppuccinMocha,
        };
        let presentations = crate::shell::app::commands::present_commands(
            &crate::shell::app::commands::CommandContext::default(),
            &crate::shell::app::commands::PresentationState::default(),
            [],
        );
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_400.0, 300.0),
            )),
            ..Default::default()
        };
        let render = || {
            ctx.run_ui(input(), |ui| {
                show(ui, &model, &presentations, |_| Vec::new());
            })
        };
        let _ = render();
        let output = render();

        let mut texts = Vec::new();
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    out.push((text.galley.job.text.clone(), text.visual_bounding_rect()));
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }

        let percent = texts
            .iter()
            .find(|(text, _)| text.contains("42"))
            .unwrap_or_else(|| panic!("the bar should show its percentage, got {texts:?}"));
        let label = texts
            .iter()
            .find(|(text, _)| text == "Parsing flight.bin")
            .expect("the load label should still be shown");

        assert!(
            percent.1.left() > label.1.right(),
            "the percentage belongs inside the bar, after the label"
        );
        assert!(
            percent.1.height() < tokens.control_height,
            "the percentage text should fit a slimmer bar"
        );

        let mut bars = Vec::new();
        fn rects(shape: &egui::epaint::Shape, out: &mut Vec<egui::Rect>) {
            match shape {
                egui::epaint::Shape::Rect(rect) => out.push(rect.rect),
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| rects(s, out)),
                _ => {}
            }
        }
        for clipped in &output.shapes {
            rects(&clipped.shape, &mut bars);
        }
        let bar = bars
            .iter()
            .find(|rect| {
                rect.contains(percent.1.center()) && rect.width() > 100.0
            })
            .unwrap_or_else(|| panic!("the progress bar should enclose its percentage"));

        assert!(
            bar.height() < tokens.dense_row_height,
            "the bar should be slimmer than a dense row, got {}",
            bar.height()
        );
        assert!(
            bar.width() >= 170.0,
            "the bar should be wide, got {}",
            bar.width()
        );
        assert!(
            (percent.1.center().x - bar.center().x).abs() < 2.0,
            "the percentage should be centered in the bar, text at {} vs bar center {}",
            percent.1.center().x,
            bar.center().x
        );
        assert!(
            percent.1.height() <= bar.height(),
            "the percentage must fit inside the bar, text {} vs bar {}",
            percent.1.height(),
            bar.height()
        );
    }

    fn header_with_toolbar_probe(
        ctx: &egui::Context,
    ) -> (egui::FullOutput, HeaderOutput) {
        let model = HeaderModel {
            emphasis: ShellEmphasis::Offline,
            live_statuses: Vec::new(),
            load: LoadStatusView::Idle,
            fps: None,
            theme: crate::ui::theme::ThemeChoice::CatppuccinMocha,
        };
        let presentations = crate::shell::app::commands::present_commands(
            &crate::shell::app::commands::CommandContext::default(),
            &crate::shell::app::commands::PresentationState::default(),
            [],
        );
        let mut header_output = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_200.0, 300.0),
                )),
                ..Default::default()
            },
            |ui| {
                header_output = Some(show(ui, &model, &presentations, |ui| {
                    let _ = ui.button("Toolbar probe");
                    vec![AppCommand::Static(CommandId::TogglePlayheadSnap)]
                }));
            },
        );
        (output, header_output.expect("header should render"))
    }

    fn header_with_live_link(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, HeaderOutput) {
        let model = HeaderModel {
            emphasis: ShellEmphasis::Live,
            live_statuses: vec![LiveSummary {
                index: 3,
                endpoint: "UDP 127.0.0.1:14550".to_owned(),
                state: "Connected".to_owned(),
                rx_frames: 42,
                rows: 120,
                recording: None,
            }],
            load: LoadStatusView::Idle,
            fps: None,
            theme: crate::ui::theme::ThemeChoice::CatppuccinMocha,
        };
        let presentations = crate::shell::app::commands::present_commands(
            &crate::shell::app::commands::CommandContext {
                live_link_count: 1,
                ..Default::default()
            },
            &crate::shell::app::commands::PresentationState::default(),
            [CommandPresentation {
                command: AppCommand::DisconnectLink(3),
                label: "Disconnect UDP 127.0.0.1:14550".to_owned(),
                shortcut: None,
                availability: CommandAvailability::Enabled,
                selected: None,
            }],
        );
        let mut header_output = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_200.0, 300.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                header_output = Some(show(ui, &model, &presentations, |_| Vec::new()));
            },
        );
        (output, header_output.expect("header should render"))
    }

    #[test]
    fn header_places_toolbar_after_tools_on_the_menu_row() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        ctx.enable_accesskit();

        let (output, _) = header_with_toolbar_probe(&ctx);
        let update = output
            .platform_output
            .accesskit_update
            .expect("accessibility tree should be emitted");
        let bounds = |label: &str| {
            update
                .nodes
                .iter()
                .map(|(_, node)| node)
                .find(|node| node.label() == Some(label))
                .and_then(|node| node.bounds())
                .unwrap_or_else(|| panic!("{label} should have bounds"))
        };
        let tools = bounds("Tools");
        let toolbar = bounds("Toolbar probe");

        assert!(toolbar.x0 > tools.x1);
        assert_eq!(
            (toolbar.y0 + toolbar.y1) * 0.5,
            (tools.y0 + tools.y1) * 0.5,
        );
    }

    #[test]
    fn header_includes_toolbar_commands_in_its_output() {
        let ctx = egui::Context::default();
        let (_, output) = header_with_toolbar_probe(&ctx);

        assert!(
            output
                .commands
                .contains(&AppCommand::Static(CommandId::TogglePlayheadSnap))
        );
    }

    #[test]
    fn clicking_live_link_disconnect_icon_emits_that_links_command() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        ctx.enable_accesskit();

        let (output, _) = header_with_live_link(&ctx, Vec::new());
        let update = output
            .platform_output
            .accesskit_update
            .expect("accessibility tree should be emitted");
        let button = update
            .nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.label() == Some("Disconnect UDP 127.0.0.1:14550"))
            .expect("each live link should have a labelled disconnect button");
        let bounds = button
            .bounds()
            .expect("disconnect button should have bounds");
        let pos = egui::pos2(
            ((bounds.x0 + bounds.x1) * 0.5) as f32,
            ((bounds.y0 + bounds.y1) * 0.5) as f32,
        );
        let _ = header_with_live_link(
            &ctx,
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
        let (_, output) = header_with_live_link(
            &ctx,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );

        assert_eq!(output.commands, [AppCommand::DisconnectLink(3)]);
    }

    #[test]
    fn file_menu_omits_live_link_disconnect_rows() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        ctx.enable_accesskit();

        let (output, _) = header_with_live_link(&ctx, Vec::new());
        let update = output
            .platform_output
            .accesskit_update
            .expect("accessibility tree should be emitted");
        let file = update
            .nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.label() == Some("File"))
            .expect("File menu should be accessible");
        let bounds = file.bounds().expect("File menu should have bounds");
        let pos = egui::pos2(
            ((bounds.x0 + bounds.x1) * 0.5) as f32,
            ((bounds.y0 + bounds.y1) * 0.5) as f32,
        );
        let _ = header_with_live_link(
            &ctx,
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
        let (output, _) = header_with_live_link(
            &ctx,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let update = output
            .platform_output
            .accesskit_update
            .expect("opening File should update the accessibility tree");

        assert_eq!(
            update
                .nodes
                .iter()
                .map(|(_, node)| node)
                .filter(|node| node.label() == Some("Disconnect UDP 127.0.0.1:14550"))
                .count(),
            1,
            "opening File must not add a second disconnect control"
        );
    }

    #[test]
    fn header_bottom_margin_uses_compact_spacing() {
        let style = egui::Style::default();
        let tokens = crate::ui::design_tokens::DesignTokens::from_style(&style);

        assert_eq!(header_bottom_margin(&style), tokens.space_xs);
    }

    #[test]
    fn changing_shell_emphasis_never_requests_source_mutation() {
        assert_eq!(ShellEmphasis::Offline.toggle(), ShellEmphasis::Live);
        assert_eq!(ShellEmphasis::Live.toggle(), ShellEmphasis::Offline);
    }

    #[test]
    fn checked_view_rows_use_the_approved_names() {
        assert_eq!(CommandId::ToggleDataBrowser.spec().label, "Data Browser");
        assert_eq!(CommandId::ToggleInspector.spec().label, "Inspector");
        assert_eq!(CommandId::ToggleScene3d.spec().label, "3D Scene");
        assert_eq!(CommandId::OpenScripting.spec().label, "Scripting Console");
        assert_eq!(CommandId::OpenLogging.spec().label, "Application Logs");
    }

    #[test]
    fn clicking_a_rendered_checked_menu_row_emits_its_canonical_toggle() {
        let presentation = crate::shell::app::commands::present_commands(
            &crate::shell::app::commands::CommandContext::default(),
            &crate::shell::app::commands::PresentationState {
                data_browser_open: true,
                ..Default::default()
            },
            [],
        )
        .into_iter()
        .find(|item| item.command == AppCommand::Static(CommandId::ToggleDataBrowser))
        .unwrap();
        let ctx = egui::Context::default();
        let _ = checked_row_frame(&ctx, &presentation, vec![]);
        let (output, _) = checked_row_frame(&ctx, &presentation, vec![]);
        let rect = output
            .shapes
            .iter()
            .find_map(|shape| find_text_rect(&shape.shape, &presentation.label))
            .expect("checked menu label should be painted");
        let pos = rect.center();
        let _ = checked_row_frame(
            &ctx,
            &presentation,
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
        let (_, selected) = checked_row_frame(
            &ctx,
            &presentation,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );

        assert_eq!(
            selected,
            [AppCommand::Static(CommandId::ToggleDataBrowser)]
        );
    }

    #[test]
    fn classic_menus_own_each_static_command_once() {
        let ids = classic_menu_command_ids();
        let expected: std::collections::HashSet<_> = [
            CommandId::Open,
            CommandId::ConnectLive,
            CommandId::CancelTasks,
            CommandId::ExportData,
            CommandId::ExportDiagnostics,
            CommandId::ExportProfiling,
            CommandId::ExportWorkspacePng,
            CommandId::Exit,
            CommandId::ToggleDataBrowser,
            CommandId::ToggleInspector,
            CommandId::ToggleScene3d,
            CommandId::OpenDiagnostics,
            CommandId::OpenPerformance,
            CommandId::OpenMarkers,
            CommandId::OpenScripting,
            CommandId::OpenLogging,
            CommandId::SaveLayout,
            CommandId::ManageLayouts,
            CommandId::ImportLayout,
            CommandId::ExportLayout,
            CommandId::ClearLayout,
            CommandId::SyncSources,
            CommandId::OpenDataFlow,
            CommandId::OpenSettings,
            CommandId::OpenScriptEditor,
            CommandId::OpenScriptVariables,
            CommandId::OpenParserEditor,
        ]
        .into_iter()
        .collect();
        assert_eq!(
            ids.iter()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
        for id in &ids {
            assert_eq!(
                ids.iter().filter(|candidate| *candidate == id).count(),
                1,
                "{id:?}"
            );
            assert!(
                id.spec().routes.contains(&crate::shell::app::commands::AccessRoute::ClassicMenu),
                "rendered command lacks a classic-menu route: {id:?}"
            );
        }
        for (owner, sections) in [
            (
                ClassicMenuOwner::File,
                &[FILE_MENU, FILE_EXPORT_MENU][..],
            ),
            (
                ClassicMenuOwner::View,
                &[VIEW_MENU, VIEW_PANELS_MENU][..],
            ),
            (
                ClassicMenuOwner::Analyze,
                &[ANALYZE_MENU][..],
            ),
            (
                ClassicMenuOwner::Tools,
                &[
                    TOOLS_MENU,
                    TOOLS_SCRIPTS_MENU,
                    TOOLS_PARSERS_MENU,
                    TOOLS_LAYOUTS_MENU,
                ][..],
            ),
        ] {
            for id in sections.iter().flat_map(|section| section.iter()) {
                assert_eq!(id.classic_menu_owner(), owner, "misrouted command: {id:?}");
            }
        }
        assert_eq!(CommandId::Exit.classic_menu_owner(), ClassicMenuOwner::File);
        for omitted in [
            CommandId::AddMarker,
            CommandId::AddMeasuringMarker,
            CommandId::EqualizePlots,
            CommandId::TogglePlayheadSnap,
            CommandId::DisconnectLive,
        ] {
            assert!(!ids.contains(&omitted), "toolbar/shortcut command leaked into menu");
        }
        assert!(
            !CommandId::DisconnectLive
                .spec()
                .routes
                .contains(&crate::shell::app::commands::AccessRoute::ClassicMenu)
        );
    }

    #[test]
    fn classic_menu_titles_are_compact() {
        assert_eq!(CLASSIC_MENU_TITLES, &["File", "View", "Analyze", "Tools"]);
    }
}
