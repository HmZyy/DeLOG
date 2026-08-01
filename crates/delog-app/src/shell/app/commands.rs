#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandId {
    Open,
    ConnectLive,
    SyncSources,
    DisconnectLive,
    CancelTasks,
    ExportData,
    ExportDiagnostics,
    ExportProfiling,
    ExportWorkspacePng,
    ToggleDataBrowser,
    ToggleInspector,
    ToggleScene3d,
    OpenDiagnostics,
    OpenPerformance,
    OpenMarkers,
    OpenScripting,
    OpenLogging,
    SaveLayout,
    LoadLayout,
    ManageLayouts,
    ClearLayout,
    ImportLayout,
    ExportLayout,
    EqualizePlots,
    OpenDataFlow,
    OpenScriptEditor,
    OpenScriptVariables,
    OpenParserEditor,
    TogglePlayheadSnap,
    AddMeasuringMarker,
    CycleLegendPosition,
    ToggleLegends,
    OpenSettings,
    Exit,
    TogglePlayback,
    JumpStart,
    JumpEnd,
    StepLeft,
    StepRight,
    AddMarker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    Static(CommandId),
    OpenWithParser(String),
    RunScript(String),
    LoadNamedLayout(String),
    DisconnectLink(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRoute {
    Header,
    SourceMenu,
    WorkspaceMenu,
    AnalysisMenu,
    ExtensionsMenu,
    PanelsMenu,
    ExportMenu,
    PlotContext,
    SceneToolbar,
    Transport,
    Shortcut,
    Palette,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandGroup {
    Source,
    Workspace,
    Analysis,
    Extensions,
    Panels,
    Export,
    Application,
    Transport,
}

#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub label: &'static str,
    pub group: CommandGroup,
    pub shortcut: Option<&'static str>,
    pub search_terms: &'static str,
    pub routes: &'static [AccessRoute],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicFamily {
    Parser,
    Script,
    Layout,
    LiveLink,
}

pub const fn dynamic_command_families() -> &'static [DynamicFamily] {
    &[
        DynamicFamily::Parser,
        DynamicFamily::Script,
        DynamicFamily::Layout,
        DynamicFamily::LiveLink,
    ]
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CommandContext {
    pub has_data: bool,
    pub offline_source_count: usize,
    pub live_link_count: usize,
    pub has_active_tasks: bool,
    pub scripting_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAvailability {
    Enabled,
    Disabled(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPresentation {
    pub command: AppCommand,
    pub label: String,
    pub shortcut: Option<&'static str>,
    pub availability: CommandAvailability,
}

macro_rules! spec {
    ($label:literal, $group:ident, $shortcut:expr, $terms:literal, $($route:ident),+ $(,)?) => {
        CommandSpec {
            label: $label,
            group: CommandGroup::$group,
            shortcut: $shortcut,
            search_terms: $terms,
            routes: &[$(AccessRoute::$route),+],
        }
    };
}

impl CommandId {
    pub const ALL: &'static [Self] = &[
        Self::Open,
        Self::ConnectLive,
        Self::SyncSources,
        Self::DisconnectLive,
        Self::CancelTasks,
        Self::ExportData,
        Self::ExportDiagnostics,
        Self::ExportProfiling,
        Self::ExportWorkspacePng,
        Self::ToggleDataBrowser,
        Self::ToggleInspector,
        Self::ToggleScene3d,
        Self::OpenDiagnostics,
        Self::OpenPerformance,
        Self::OpenMarkers,
        Self::OpenScripting,
        Self::OpenLogging,
        Self::SaveLayout,
        Self::LoadLayout,
        Self::ManageLayouts,
        Self::ClearLayout,
        Self::ImportLayout,
        Self::ExportLayout,
        Self::EqualizePlots,
        Self::OpenDataFlow,
        Self::OpenScriptEditor,
        Self::OpenScriptVariables,
        Self::OpenParserEditor,
        Self::TogglePlayheadSnap,
        Self::AddMeasuringMarker,
        Self::CycleLegendPosition,
        Self::ToggleLegends,
        Self::OpenSettings,
        Self::Exit,
        Self::TogglePlayback,
        Self::JumpStart,
        Self::JumpEnd,
        Self::StepLeft,
        Self::StepRight,
        Self::AddMarker,
    ];

    pub const fn spec(self) -> CommandSpec {
        use CommandId::*;
        match self {
            Open => spec!(
                "Open log…",
                Source,
                None,
                "file import load",
                Header,
                SourceMenu,
                Palette
            ),
            ConnectLive => spec!(
                "Connect live…",
                Source,
                None,
                "stream telemetry mavlink",
                Header,
                SourceMenu,
                Palette
            ),
            SyncSources => spec!(
                "Sync sources…",
                Source,
                None,
                "align logs time",
                SourceMenu,
                Palette
            ),
            DisconnectLive => spec!(
                "Disconnect live",
                Source,
                None,
                "stop stream",
                Header,
                SourceMenu,
                Palette
            ),
            CancelTasks => spec!(
                "Cancel active tasks",
                Source,
                None,
                "stop loading parser",
                Header,
                SourceMenu,
                Palette
            ),
            ExportData => spec!(
                "Export data…",
                Export,
                None,
                "csv parquet",
                ExportMenu,
                Palette
            ),
            ExportDiagnostics => spec!(
                "Export diagnostics…",
                Export,
                None,
                "errors warnings json",
                ExportMenu,
                Palette
            ),
            ExportProfiling => spec!(
                "Export profiling…",
                Export,
                None,
                "performance metrics json",
                ExportMenu,
                Palette
            ),
            ExportWorkspacePng => spec!(
                "Export workspace PNG…",
                Export,
                None,
                "image screenshot",
                Header,
                ExportMenu,
                Palette
            ),
            ToggleDataBrowser => spec!(
                "Toggle data browser",
                Panels,
                None,
                "signals topics sidebar",
                Header,
                PanelsMenu,
                Palette
            ),
            ToggleInspector => spec!(
                "Toggle inspector",
                Panels,
                None,
                "properties selection sidebar",
                Header,
                PanelsMenu,
                Palette
            ),
            ToggleScene3d => spec!(
                "Toggle 3D scene",
                Panels,
                None,
                "vehicle map view",
                Header,
                PanelsMenu,
                SceneToolbar,
                Palette
            ),
            OpenDiagnostics => spec!(
                "Diagnostics",
                Analysis,
                Some("F1"),
                "errors warnings",
                AnalysisMenu,
                PanelsMenu,
                Shortcut,
                Palette
            ),
            OpenPerformance => spec!(
                "Performance",
                Analysis,
                Some("F2"),
                "profiling metrics",
                AnalysisMenu,
                PanelsMenu,
                Shortcut,
                Palette
            ),
            OpenMarkers => spec!(
                "Markers",
                Analysis,
                Some("F3"),
                "annotations events",
                AnalysisMenu,
                PanelsMenu,
                Shortcut,
                Palette
            ),
            OpenScripting => spec!(
                "Scripting console",
                Extensions,
                Some("F9"),
                "automation code",
                ExtensionsMenu,
                PanelsMenu,
                Shortcut,
                Palette
            ),
            OpenLogging => spec!(
                "Application logs",
                Analysis,
                Some("F12"),
                "logging messages",
                AnalysisMenu,
                PanelsMenu,
                Shortcut,
                Palette
            ),
            SaveLayout => spec!(
                "Save layout…",
                Workspace,
                Some("Ctrl+S"),
                "workspace arrangement",
                WorkspaceMenu,
                Shortcut,
                Palette
            ),
            LoadLayout => spec!(
                "Load layout…",
                Workspace,
                Some("Ctrl+L"),
                "workspace arrangement",
                WorkspaceMenu,
                Shortcut,
                Palette
            ),
            ManageLayouts => spec!(
                "Manage layouts…",
                Workspace,
                None,
                "rename delete",
                WorkspaceMenu,
                Palette
            ),
            ClearLayout => spec!(
                "Clear current layout",
                Workspace,
                None,
                "reset workspace",
                WorkspaceMenu,
                Palette
            ),
            ImportLayout => spec!(
                "Import layout JSON…",
                Workspace,
                None,
                "workspace file",
                WorkspaceMenu,
                Palette
            ),
            ExportLayout => spec!(
                "Export layout JSON…",
                Workspace,
                None,
                "workspace file",
                WorkspaceMenu,
                ExportMenu,
                Palette
            ),
            EqualizePlots => spec!(
                "Equalize plot heights",
                Workspace,
                None,
                "resize panels",
                Header,
                WorkspaceMenu,
                PlotContext,
                Palette
            ),
            OpenDataFlow => spec!(
                "Data flow",
                Analysis,
                None,
                "pipeline graph",
                AnalysisMenu,
                Palette
            ),
            OpenScriptEditor => spec!(
                "Script editor…",
                Extensions,
                None,
                "automation code",
                ExtensionsMenu,
                Palette
            ),
            OpenScriptVariables => spec!(
                "Script variables…",
                Extensions,
                None,
                "automation state",
                ExtensionsMenu,
                Palette
            ),
            OpenParserEditor => spec!(
                "Parser editor…",
                Extensions,
                None,
                "custom decoder",
                ExtensionsMenu,
                Palette
            ),
            TogglePlayheadSnap => spec!(
                "Toggle playhead snap",
                Analysis,
                None,
                "sample cursor magnet",
                Header,
                PlotContext,
                Palette
            ),
            AddMeasuringMarker => spec!(
                "Toggle measuring marker",
                Analysis,
                None,
                "delta ruler",
                Header,
                PlotContext,
                Palette
            ),
            CycleLegendPosition => spec!(
                "Cycle legend position",
                Workspace,
                None,
                "plot key corner",
                Header,
                PlotContext,
                Palette
            ),
            ToggleLegends => spec!(
                "Toggle legends",
                Workspace,
                None,
                "plot key visibility",
                Header,
                PlotContext,
                Palette
            ),
            OpenSettings => spec!(
                "Settings…",
                Application,
                None,
                "preferences configuration",
                Header,
                Palette
            ),
            Exit => spec!("Exit", Application, None, "quit close", Header, Palette),
            TogglePlayback => spec!(
                "Play or pause",
                Transport,
                Some("Space"),
                "timeline",
                Transport,
                Shortcut,
                Palette
            ),
            JumpStart => spec!(
                "Jump to start",
                Transport,
                Some("Home"),
                "timeline first",
                Transport,
                Shortcut,
                Palette
            ),
            JumpEnd => spec!(
                "Jump to end or live",
                Transport,
                Some("End"),
                "timeline latest",
                Transport,
                Shortcut,
                Palette
            ),
            StepLeft => spec!(
                "Previous sample",
                Transport,
                Some("Left"),
                "timeline step",
                Transport,
                Shortcut,
                Palette
            ),
            StepRight => spec!(
                "Next sample",
                Transport,
                Some("Right"),
                "timeline step",
                Transport,
                Shortcut,
                Palette
            ),
            AddMarker => spec!(
                "Add marker",
                Transport,
                Some("M"),
                "timeline annotation",
                Transport,
                Shortcut,
                Palette
            ),
        }
    }

    pub const fn availability(self, context: &CommandContext) -> CommandAvailability {
        match self {
            Self::SyncSources if context.offline_source_count < 2 => {
                CommandAvailability::Disabled("Open at least two offline sources to synchronize")
            }
            Self::DisconnectLive if context.live_link_count == 0 => {
                CommandAvailability::Disabled("No live connection is active")
            }
            Self::CancelTasks if !context.has_active_tasks => {
                CommandAvailability::Disabled("No background task is active")
            }
            Self::OpenScripting
            | Self::OpenScriptEditor
            | Self::OpenScriptVariables
            | Self::OpenParserEditor
                if !context.scripting_enabled =>
            {
                CommandAvailability::Disabled("Scripting support is not enabled in this build")
            }
            Self::AddMeasuringMarker if !context.has_data => {
                CommandAvailability::Disabled("Open a log or connect a live source first")
            }
            _ => CommandAvailability::Enabled,
        }
    }
}

pub fn present_commands(
    context: &CommandContext,
    dynamic: impl IntoIterator<Item = CommandPresentation>,
) -> Vec<CommandPresentation> {
    let mut commands: Vec<_> = CommandId::ALL
        .iter()
        .copied()
        .map(|id| {
            let spec = id.spec();
            CommandPresentation {
                command: AppCommand::Static(id),
                label: spec.label.to_owned(),
                shortcut: spec.shortcut,
                availability: id.availability(context),
            }
        })
        .collect();
    commands.extend(dynamic);
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_current_feature_has_a_redesigned_access_route() {
        let missing: Vec<_> = CommandId::ALL
            .iter()
            .filter(|id| id.spec().routes.is_empty())
            .collect();
        assert!(missing.is_empty(), "commands without a route: {missing:?}");
    }

    #[test]
    fn current_shortcuts_are_preserved() {
        assert_eq!(CommandId::TogglePlayback.spec().shortcut, Some("Space"));
        assert_eq!(CommandId::SaveLayout.spec().shortcut, Some("Ctrl+S"));
        assert_eq!(CommandId::LoadLayout.spec().shortcut, Some("Ctrl+L"));
        assert_eq!(CommandId::AddMarker.spec().shortcut, Some("M"));
        assert_eq!(CommandId::OpenDiagnostics.spec().shortcut, Some("F1"));
        assert_eq!(CommandId::OpenPerformance.spec().shortcut, Some("F2"));
        assert_eq!(CommandId::OpenMarkers.spec().shortcut, Some("F3"));
        assert_eq!(CommandId::OpenScripting.spec().shortcut, Some("F9"));
        assert_eq!(CommandId::OpenLogging.spec().shortcut, Some("F12"));
    }

    #[test]
    fn context_sensitive_commands_explain_why_they_are_disabled() {
        let empty = CommandContext::default();
        assert!(matches!(
            CommandId::SyncSources.availability(&empty),
            CommandAvailability::Disabled(_)
        ));
        assert!(matches!(
            CommandId::DisconnectLive.availability(&empty),
            CommandAvailability::Disabled(_)
        ));
        assert!(matches!(
            CommandId::CancelTasks.availability(&empty),
            CommandAvailability::Disabled(_)
        ));
        assert_eq!(
            CommandId::SyncSources.availability(&CommandContext {
                offline_source_count: 2,
                ..empty
            }),
            CommandAvailability::Enabled,
        );
        assert_eq!(
            CommandId::ExportData.availability(&empty),
            CommandAvailability::Enabled
        );
        assert_eq!(
            CommandId::OpenDiagnostics.availability(&empty),
            CommandAvailability::Enabled
        );
    }

    #[test]
    fn dynamic_command_families_are_part_of_the_parity_contract() {
        assert_eq!(
            dynamic_command_families(),
            &[
                DynamicFamily::Parser,
                DynamicFamily::Script,
                DynamicFamily::Layout,
                DynamicFamily::LiveLink
            ],
        );
    }
}
