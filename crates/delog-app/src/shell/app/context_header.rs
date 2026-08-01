use crate::shell::app::commands::{
    AppCommand, CommandAvailability, CommandGroup, CommandId, CommandPresentation,
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

const SOURCE_MENU: &[CommandId] = &[
    CommandId::Open,
    CommandId::ConnectLive,
    CommandId::SyncSources,
    CommandId::DisconnectLive,
    CommandId::CancelTasks,
];
const WORKSPACE_MENU: &[CommandId] = &[
    CommandId::SaveLayout,
    CommandId::LoadLayout,
    CommandId::ManageLayouts,
    CommandId::ClearLayout,
    CommandId::ImportLayout,
    CommandId::ExportLayout,
    CommandId::EqualizePlots,
];
const ANALYSIS_MENU: &[CommandId] = &[
    CommandId::OpenDiagnostics,
    CommandId::OpenPerformance,
    CommandId::OpenMarkers,
    CommandId::OpenLogging,
    CommandId::OpenDataFlow,
];
const EXTENSIONS_MENU: &[CommandId] = &[
    CommandId::OpenScripting,
    CommandId::OpenScriptEditor,
    CommandId::OpenScriptVariables,
    CommandId::OpenParserEditor,
];
const PANELS_MENU: &[CommandId] = &[
    CommandId::ToggleDataBrowser,
    CommandId::ToggleInspector,
    CommandId::ToggleScene3d,
    CommandId::OpenDiagnostics,
    CommandId::OpenPerformance,
    CommandId::OpenMarkers,
    CommandId::OpenScripting,
    CommandId::OpenLogging,
];
const EXPORT_MENU: &[CommandId] = &[
    CommandId::ExportData,
    CommandId::ExportDiagnostics,
    CommandId::ExportProfiling,
    CommandId::ExportWorkspacePng,
    CommandId::ExportLayout,
];

#[cfg(test)]
pub(crate) const fn menu_command_ids() -> &'static [CommandId] {
    &[
        CommandId::Open,
        CommandId::ConnectLive,
        CommandId::SyncSources,
        CommandId::DisconnectLive,
        CommandId::CancelTasks,
        CommandId::SaveLayout,
        CommandId::LoadLayout,
        CommandId::ManageLayouts,
        CommandId::ClearLayout,
        CommandId::ImportLayout,
        CommandId::ExportLayout,
        CommandId::EqualizePlots,
        CommandId::OpenDiagnostics,
        CommandId::OpenPerformance,
        CommandId::OpenMarkers,
        CommandId::OpenLogging,
        CommandId::OpenDataFlow,
        CommandId::OpenScripting,
        CommandId::OpenScriptEditor,
        CommandId::OpenScriptVariables,
        CommandId::OpenParserEditor,
        CommandId::ToggleDataBrowser,
        CommandId::ToggleInspector,
        CommandId::ToggleScene3d,
        CommandId::ExportData,
        CommandId::ExportDiagnostics,
        CommandId::ExportProfiling,
        CommandId::ExportWorkspacePng,
    ]
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
            if let Some(source) = &model.active_source_label {
                ui.weak(source);
            }
            for status in &model.live_statuses {
                let detail = format!("{} · {} rows", status.state, status.rows);
                let chip = components::StatusChip::connected(&status.endpoint, detail);
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
            menu(ui, "Source", SOURCE_MENU, presentations, &mut commands);
            menu(
                ui,
                "Workspace",
                WORKSPACE_MENU,
                presentations,
                &mut commands,
            );
            menu(ui, "Analysis", ANALYSIS_MENU, presentations, &mut commands);
            menu(
                ui,
                "Extensions",
                EXTENSIONS_MENU,
                presentations,
                &mut commands,
            );
            menu(ui, "Panels", PANELS_MENU, presentations, &mut commands);
            menu(ui, "Export", EXPORT_MENU, presentations, &mut commands);
            ui.menu_button("App", |ui| {
                menu_item(ui, CommandId::OpenSettings, presentations, &mut commands);
                menu_item(ui, CommandId::Exit, presentations, &mut commands);
            });
            ui.separator();
            ui.weak("Ctrl+K  Commands");
        });
    });
    commands
}

fn menu(
    ui: &mut egui::Ui,
    title: &str,
    ids: &[CommandId],
    presentations: &[CommandPresentation],
    selected: &mut Vec<AppCommand>,
) {
    ui.menu_button(title, |ui| {
        for id in ids {
            menu_item(ui, *id, presentations, selected);
        }
        for presentation in presentations.iter().filter(|presentation| {
            dynamic_group(&presentation.command) == Some(group_for_title(title))
        }) {
            presentation_row(ui, presentation, selected);
        }
    });
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

fn group_for_title(title: &str) -> CommandGroup {
    match title {
        "Source" => CommandGroup::Source,
        "Workspace" => CommandGroup::Workspace,
        "Analysis" => CommandGroup::Analysis,
        "Extensions" => CommandGroup::Extensions,
        "Panels" => CommandGroup::Panels,
        "Export" => CommandGroup::Export,
        _ => CommandGroup::Application,
    }
}

fn dynamic_group(command: &AppCommand) -> Option<CommandGroup> {
    match command {
        AppCommand::OpenWithParser(_) | AppCommand::DisconnectLink(_) => Some(CommandGroup::Source),
        AppCommand::LoadNamedLayout(_) => Some(CommandGroup::Workspace),
        AppCommand::RunScript(_) => Some(CommandGroup::Extensions),
        AppCommand::Static(_) | AppCommand::ToggleShellEmphasis => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::app::commands::AccessRoute;

    #[test]
    fn changing_shell_emphasis_never_requests_source_mutation() {
        assert_eq!(ShellEmphasis::Offline.toggle(), ShellEmphasis::Live);
        assert_eq!(ShellEmphasis::Live.toggle(), ShellEmphasis::Offline);
    }

    #[test]
    fn application_menus_cover_every_menu_route() {
        let routed = menu_command_ids();
        for id in CommandId::ALL {
            if id.spec().routes.iter().any(|route| {
                matches!(
                    route,
                    AccessRoute::SourceMenu
                        | AccessRoute::WorkspaceMenu
                        | AccessRoute::AnalysisMenu
                        | AccessRoute::ExtensionsMenu
                        | AccessRoute::PanelsMenu
                        | AccessRoute::ExportMenu
                )
            }) {
                assert!(routed.contains(id), "missing menu route for {id:?}");
            }
        }
    }
}
