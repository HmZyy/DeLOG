use crate::shell::app::commands::{
    AppCommand, CommandAvailability, CommandId, CommandPresentation,
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
    pub active_source_label: Option<String>,
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

const CLASSIC_MENU_TITLES: &[&str] = &["File", "View", "Analyze", "Tools"];

const FILE_MENU: &[CommandId] = &[
    CommandId::Open,
    CommandId::ConnectLive,
    CommandId::DisconnectLive,
    CommandId::CancelTasks,
    CommandId::Exit,
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
    CommandId::OpenLogging,
];
const VIEW_LAYOUTS_MENU: &[CommandId] = &[
    CommandId::SaveLayout,
    CommandId::LoadLayout,
    CommandId::ManageLayouts,
    CommandId::ImportLayout,
    CommandId::ExportLayout,
    CommandId::ClearLayout,
    CommandId::EqualizePlots,
];
const ANALYZE_MENU: &[CommandId] = &[
    CommandId::SyncSources,
    CommandId::AddMarker,
    CommandId::OpenMarkers,
    CommandId::OpenDataFlow,
];
const TOOLS_MENU: &[CommandId] = &[CommandId::OpenSettings];
const TOOLS_SCRIPTS_MENU: &[CommandId] = &[
    CommandId::OpenScripting,
    CommandId::OpenScriptEditor,
    CommandId::OpenScriptVariables,
];
const TOOLS_PARSERS_MENU: &[CommandId] = &[CommandId::OpenParserEditor];

#[cfg(test)]
pub(crate) fn classic_menu_command_ids() -> Vec<CommandId> {
    [
        FILE_MENU,
        FILE_EXPORT_MENU,
        VIEW_MENU,
        VIEW_PANELS_MENU,
        VIEW_LAYOUTS_MENU,
        ANALYZE_MENU,
        TOOLS_MENU,
        TOOLS_SCRIPTS_MENU,
        TOOLS_PARSERS_MENU,
    ]
    .into_iter()
    .flatten()
    .copied()
    .collect()
}

pub fn show(
    ui: &mut egui::Ui,
    model: &HeaderModel,
    presentations: &[CommandPresentation],
) -> Vec<AppCommand> {
    let mut commands = Vec::new();
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
            if let Some(source) = &model.active_source_label {
                ui.weak(source);
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
                components::status_chip(ui, &chip, model.theme)
                    .on_hover_text(format!(
                        "{} received frames{}",
                        status.rx_frames,
                        status
                            .recording
                            .as_deref()
                            .map(|value| format!(" · {value}"))
                            .unwrap_or_default()
                    ))
                    .context_menu(|ui| {
                        if ui.button("Disconnect").clicked() {
                            commands.push(AppCommand::DisconnectLink(status.index));
                            ui.close();
                        }
                    });
            }
            if let LoadStatusView::Active { label, progress } = &model.load {
                ui.separator();
                ui.label(label);
                if let Some(progress) = progress {
                    ui.add(egui::ProgressBar::new(*progress).desired_width(90.0));
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
                menu_items(ui, FILE_MENU, presentations, &mut commands);
                ui.menu_button("Open With", |ui| {
                    dynamic_rows(ui, presentations, &mut commands, |command| {
                        matches!(command, AppCommand::OpenWithParser(_))
                    });
                });
                ui.menu_button("Export", |ui| {
                    menu_items(ui, FILE_EXPORT_MENU, presentations, &mut commands);
                });
                dynamic_rows(ui, presentations, &mut commands, |command| {
                    matches!(command, AppCommand::DisconnectLink(_))
                });
            });
            ui.menu_button("View", |ui| {
                menu_items(ui, VIEW_MENU, presentations, &mut commands);
                ui.menu_button("Panels", |ui| {
                    menu_items(ui, VIEW_PANELS_MENU, presentations, &mut commands);
                });
                ui.menu_button("Layouts", |ui| {
                    menu_items(ui, VIEW_LAYOUTS_MENU, presentations, &mut commands);
                    dynamic_rows(ui, presentations, &mut commands, |command| {
                        matches!(command, AppCommand::LoadNamedLayout(_))
                    });
                });
            });
            ui.menu_button("Analyze", |ui| {
                menu_items(ui, ANALYZE_MENU, presentations, &mut commands);
            });
            ui.menu_button("Tools", |ui| {
                menu_items(ui, TOOLS_MENU, presentations, &mut commands);
                ui.menu_button("Scripts", |ui| {
                    menu_items(ui, TOOLS_SCRIPTS_MENU, presentations, &mut commands);
                    dynamic_rows(ui, presentations, &mut commands, |command| {
                        matches!(command, AppCommand::RunScript(_))
                    });
                });
                ui.menu_button("Parsers", |ui| {
                    menu_items(ui, TOOLS_PARSERS_MENU, presentations, &mut commands);
                });
            });
            ui.separator();
            ui.weak("Ctrl+K  Commands");
        });
    });
    commands
}

fn menu_items(
    ui: &mut egui::Ui,
    ids: &[CommandId],
    presentations: &[CommandPresentation],
    selected: &mut Vec<AppCommand>,
) {
    for id in ids {
        menu_item(ui, *id, presentations, selected);
    }
}

fn dynamic_rows(
    ui: &mut egui::Ui,
    presentations: &[CommandPresentation],
    selected: &mut Vec<AppCommand>,
    matches_command: impl Fn(&AppCommand) -> bool,
) {
    for presentation in presentations
        .iter()
        .filter(|presentation| matches_command(&presentation.command))
    {
        presentation_row(ui, presentation, selected);
    }
}

fn menu_item(
    ui: &mut egui::Ui,
    id: CommandId,
    presentations: &[CommandPresentation],
    selected: &mut Vec<AppCommand>,
) {
    if let Some(presentation) = static_presentation(presentations, id) {
        presentation_row(ui, presentation, selected);
    }
}

fn presentation_row(
    ui: &mut egui::Ui,
    presentation: &CommandPresentation,
    selected: &mut Vec<AppCommand>,
) {
    let (enabled, reason) = match presentation.availability {
        CommandAvailability::Enabled => (true, None),
        CommandAvailability::Disabled(reason) => (false, Some(reason)),
    };
    if components::menu_row(
        ui,
        &presentation.label,
        presentation.shortcut,
        enabled,
        reason,
    )
    .clicked()
    {
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

    #[test]
    fn changing_shell_emphasis_never_requests_source_mutation() {
        assert_eq!(ShellEmphasis::Offline.toggle(), ShellEmphasis::Live);
        assert_eq!(ShellEmphasis::Live.toggle(), ShellEmphasis::Offline);
    }

    #[test]
    fn classic_menus_own_each_static_command_once() {
        let ids = classic_menu_command_ids();
        assert_eq!(
            ids,
            vec![
                CommandId::Open,
                CommandId::ConnectLive,
                CommandId::DisconnectLive,
                CommandId::CancelTasks,
                CommandId::Exit,
                CommandId::ExportData,
                CommandId::ExportDiagnostics,
                CommandId::ExportProfiling,
                CommandId::ExportWorkspacePng,
                CommandId::ToggleDataBrowser,
                CommandId::ToggleInspector,
                CommandId::ToggleScene3d,
                CommandId::OpenDiagnostics,
                CommandId::OpenPerformance,
                CommandId::OpenLogging,
                CommandId::SaveLayout,
                CommandId::LoadLayout,
                CommandId::ManageLayouts,
                CommandId::ImportLayout,
                CommandId::ExportLayout,
                CommandId::ClearLayout,
                CommandId::EqualizePlots,
                CommandId::SyncSources,
                CommandId::AddMarker,
                CommandId::OpenMarkers,
                CommandId::OpenDataFlow,
                CommandId::OpenSettings,
                CommandId::OpenScripting,
                CommandId::OpenScriptEditor,
                CommandId::OpenScriptVariables,
                CommandId::OpenParserEditor,
            ]
        );
        for id in &ids {
            assert_eq!(
                ids.iter().filter(|candidate| *candidate == id).count(),
                1,
                "{id:?}"
            );
        }
    }

    #[test]
    fn classic_menu_titles_are_compact() {
        assert_eq!(CLASSIC_MENU_TITLES, &["File", "View", "Analyze", "Tools"]);
    }
}
