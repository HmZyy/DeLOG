use delog_core::field_view::SampleMode;

pub use super::dynamic_commands::{
    DynamicCommandCatalog, dynamic_command_families, merge_dynamic_command_refresh,
};

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
    ToggleShellEmphasis,
    FitAll,
    SetCursorSampling(SampleMode),
    OpenWithParser(String),
    #[cfg_attr(not(feature = "scripting"), allow(dead_code))]
    RunScript(String),
    LoadNamedLayout(String),
    DisconnectLink(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClassicMenuOwner {
    File,
    View,
    Analyze,
    Tools,
}

impl ClassicMenuOwner {
    pub const fn title(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::View => "View",
            Self::Analyze => "Analyze",
            Self::Tools => "Tools",
        }
    }
}

impl AppCommand {
    pub const fn classic_menu_owner(&self) -> ClassicMenuOwner {
        match self {
            Self::Static(id) => id.classic_menu_owner(),
            Self::ToggleShellEmphasis | Self::DisconnectLink(_) => ClassicMenuOwner::File,
            Self::OpenWithParser(_) => ClassicMenuOwner::Tools,
            Self::FitAll => ClassicMenuOwner::View,
            Self::SetCursorSampling(_) => ClassicMenuOwner::Analyze,
            Self::RunScript(_) => ClassicMenuOwner::Tools,
            Self::LoadNamedLayout(_) => ClassicMenuOwner::View,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRoute {
    Header,
    ClassicMenu,
    GlobalToolbar,
    SceneToolbar,
    Transport,
    Shortcut,
    Palette,
}

impl AccessRoute {
    pub const fn search_term(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::ClassicMenu => "menu",
            Self::GlobalToolbar => "global toolbar",
            Self::SceneToolbar => "3d toolbar",
            Self::Transport => "transport",
            Self::Shortcut => "shortcut",
            Self::Palette => "palette",
        }
    }
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
    pub classic_menu_owner: ClassicMenuOwner,
    pub shortcut: Option<&'static str>,
    pub search_terms: &'static str,
    pub routes: &'static [AccessRoute],
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CommandContext {
    pub has_data: bool,
    pub offline_source_count: usize,
    pub live_link_count: usize,
    pub has_active_tasks: bool,
    pub scripting_enabled: bool,
}

impl CommandContext {
    pub const fn for_frame(
        has_data: bool,
        offline_source_count: usize,
        live_link_count: usize,
        native_tasks_active: bool,
        parser_task_active: bool,
        scripting_enabled: bool,
    ) -> Self {
        Self {
            has_data,
            offline_source_count,
            live_link_count,
            has_active_tasks: native_tasks_active || parser_task_active,
            scripting_enabled,
        }
    }
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
    /// `Some` for stateful commands, allowing every surface to render the
    /// same selected state. Stateless commands use `None`.
    pub selected: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
pub struct PresentationState {
    pub shell_emphasis_live: bool,
    pub cursor_sampling: SampleMode,
    pub data_browser_open: bool,
    pub inspector_open: bool,
    pub scene_3d_open: bool,
    pub diagnostics_open: bool,
    pub performance_open: bool,
    pub markers_open: bool,
    pub scripting_console_open: bool,
    pub logging_open: bool,
    pub playhead_snap: bool,
    pub measuring_marker: bool,
    pub legends_visible: bool,
}

impl Default for PresentationState {
    fn default() -> Self {
        Self {
            shell_emphasis_live: false,
            cursor_sampling: SampleMode::Prev,
            data_browser_open: false,
            inspector_open: false,
            scene_3d_open: false,
            diagnostics_open: false,
            performance_open: false,
            markers_open: false,
            scripting_console_open: false,
            logging_open: false,
            playhead_snap: false,
            measuring_marker: false,
            legends_visible: true,
        }
    }
}

impl PresentationState {
    fn selected_for(self, id: CommandId) -> Option<bool> {
        match id {
            CommandId::ToggleDataBrowser => Some(self.data_browser_open),
            CommandId::ToggleInspector => Some(self.inspector_open),
            CommandId::ToggleScene3d => Some(self.scene_3d_open),
            CommandId::OpenDiagnostics => Some(self.diagnostics_open),
            CommandId::OpenPerformance => Some(self.performance_open),
            CommandId::OpenMarkers => Some(self.markers_open),
            CommandId::OpenScripting => Some(self.scripting_console_open),
            CommandId::OpenLogging => Some(self.logging_open),
            CommandId::TogglePlayheadSnap => Some(self.playhead_snap),
            CommandId::AddMeasuringMarker => Some(self.measuring_marker),
            CommandId::ToggleLegends => Some(self.legends_visible),
            _ => None,
        }
    }
}

macro_rules! spec {
    ($label:literal, $group:ident, $shortcut:expr, $terms:literal, $($route:ident),+ $(,)?) => {
        CommandSpec {
            label: $label,
            group: CommandGroup::$group,
            classic_menu_owner: ClassicMenuOwner::File,
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
        let mut spec = match self {
            Open => spec!(
                "Open log…",
                Source,
                None,
                "file import load",
                Header,
                ClassicMenu,
                Palette
            ),
            ConnectLive => spec!(
                "Connect live…",
                Source,
                None,
                "stream telemetry mavlink",
                Header,
                ClassicMenu,
                Palette
            ),
            SyncSources => spec!(
                "Sync sources…",
                Source,
                None,
                "align logs time",
                ClassicMenu,
                Palette
            ),
            DisconnectLive => spec!(
                "Disconnect all live links",
                Source,
                None,
                "stop stream",
                Header,
                ClassicMenu,
                Palette
            ),
            CancelTasks => spec!(
                "Cancel active tasks",
                Source,
                None,
                "stop loading parser",
                Header,
                ClassicMenu,
                Palette
            ),
            ExportData => spec!(
                "Export data…",
                Export,
                None,
                "csv parquet",
                ClassicMenu,
                Palette
            ),
            ExportDiagnostics => spec!(
                "Export diagnostics…",
                Export,
                None,
                "errors warnings json",
                ClassicMenu,
                Palette
            ),
            ExportProfiling => spec!(
                "Export profiling…",
                Export,
                None,
                "performance metrics json",
                ClassicMenu,
                Palette
            ),
            ExportWorkspacePng => spec!(
                "Export workspace PNG…",
                Export,
                None,
                "image screenshot",
                Header,
                ClassicMenu,
                Palette
            ),
            ToggleDataBrowser => spec!(
                "Data Browser",
                Panels,
                None,
                "signals topics sidebar",
                Header,
                ClassicMenu,
                Palette
            ),
            ToggleInspector => spec!(
                "Inspector",
                Panels,
                None,
                "properties selection sidebar",
                Header,
                ClassicMenu,
                Palette
            ),
            ToggleScene3d => spec!(
                "3D Scene",
                Panels,
                None,
                "vehicle map view",
                Header,
                ClassicMenu,
                SceneToolbar,
                Palette
            ),
            OpenDiagnostics => spec!(
                "Diagnostics",
                Analysis,
                Some("F1"),
                "errors warnings",
                ClassicMenu,
                Shortcut,
                Palette
            ),
            OpenPerformance => spec!(
                "Performance",
                Analysis,
                Some("F2"),
                "profiling metrics",
                ClassicMenu,
                Shortcut,
                Palette
            ),
            OpenMarkers => spec!(
                "Markers",
                Analysis,
                Some("F3"),
                "annotations events",
                ClassicMenu,
                Shortcut,
                Palette
            ),
            OpenScripting => spec!(
                "Scripting Console",
                Extensions,
                Some("F9"),
                "automation code",
                ClassicMenu,
                Shortcut,
                Palette
            ),
            OpenLogging => spec!(
                "Application Logs",
                Analysis,
                Some("F12"),
                "logging messages",
                ClassicMenu,
                Shortcut,
                Palette
            ),
            SaveLayout => spec!(
                "Save layout…",
                Workspace,
                Some("Ctrl+S"),
                "workspace arrangement",
                ClassicMenu,
                Shortcut,
                Palette
            ),
            LoadLayout => spec!(
                "Load layout…",
                Workspace,
                Some("Ctrl+L"),
                "workspace arrangement",
                ClassicMenu,
                Shortcut,
                Palette
            ),
            ManageLayouts => spec!(
                "Manage layouts…",
                Workspace,
                None,
                "rename delete",
                ClassicMenu,
                Palette
            ),
            ClearLayout => spec!(
                "Clear current layout",
                Workspace,
                None,
                "reset workspace",
                ClassicMenu,
                Palette
            ),
            ImportLayout => spec!(
                "Import layout JSON…",
                Workspace,
                None,
                "workspace file",
                ClassicMenu,
                Palette
            ),
            ExportLayout => spec!(
                "Export layout JSON…",
                Workspace,
                None,
                "workspace file",
                ClassicMenu,
                Palette
            ),
            EqualizePlots => spec!(
                "Equalize plot heights",
                Workspace,
                None,
                "resize panels",
                GlobalToolbar,
                Palette
            ),
            OpenDataFlow => spec!(
                "Data flow",
                Analysis,
                None,
                "pipeline graph",
                ClassicMenu,
                Palette
            ),
            OpenScriptEditor => spec!(
                "Script editor…",
                Extensions,
                None,
                "automation code",
                ClassicMenu,
                Palette
            ),
            OpenScriptVariables => spec!(
                "Script variables…",
                Extensions,
                None,
                "automation state",
                ClassicMenu,
                Palette
            ),
            OpenParserEditor => spec!(
                "Parser editor…",
                Extensions,
                None,
                "custom decoder",
                ClassicMenu,
                Palette
            ),
            TogglePlayheadSnap => spec!(
                "Toggle playhead snap",
                Analysis,
                None,
                "sample cursor magnet",
                GlobalToolbar,
                Palette
            ),
            AddMeasuringMarker => spec!(
                "Toggle measuring marker",
                Analysis,
                None,
                "delta ruler",
                GlobalToolbar,
                Palette
            ),
            CycleLegendPosition => spec!(
                "Cycle legend position",
                Workspace,
                None,
                "plot key corner",
                GlobalToolbar,
                Palette
            ),
            ToggleLegends => spec!(
                "Toggle legends",
                Workspace,
                None,
                "plot key visibility",
                GlobalToolbar,
                Palette
            ),
            OpenSettings => spec!(
                "Settings…",
                Application,
                None,
                "preferences configuration",
                ClassicMenu,
                Palette
            ),
            Exit => spec!("Exit", Application, None, "quit close", ClassicMenu, Palette),
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
        };
        spec.classic_menu_owner = self.classic_menu_owner();
        spec
    }

    pub const fn classic_menu_owner(self) -> ClassicMenuOwner {
        use CommandId::*;
        match self {
            Open
            | ConnectLive
            | DisconnectLive
            | CancelTasks
            | ExportData
            | ExportDiagnostics
            | ExportProfiling
            | ExportWorkspacePng
            | Exit => ClassicMenuOwner::File,
            ToggleDataBrowser
            | ToggleInspector
            | ToggleScene3d
            | OpenDiagnostics
            | OpenPerformance
            | OpenMarkers
            | OpenScripting
            | OpenLogging
            | SaveLayout
            | LoadLayout
            | ManageLayouts
            | ClearLayout
            | ImportLayout
            | ExportLayout
            | EqualizePlots
            | CycleLegendPosition
            | ToggleLegends => ClassicMenuOwner::View,
            SyncSources
            | OpenDataFlow
            | TogglePlayheadSnap
            | AddMeasuringMarker
            | TogglePlayback
            | JumpStart
            | JumpEnd
            | StepLeft
            | StepRight
            | AddMarker => ClassicMenuOwner::Analyze,
            OpenScriptEditor
            | OpenScriptVariables
            | OpenParserEditor
            | OpenSettings => ClassicMenuOwner::Tools,
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
    state: &PresentationState,
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
                selected: state.selected_for(id),
            }
        })
        .collect();
    commands.extend([
        CommandPresentation {
            command: AppCommand::ToggleShellEmphasis,
            label: if state.shell_emphasis_live {
                "Emphasize offline workflows"
            } else {
                "Emphasize live workflows"
            }
            .to_owned(),
            shortcut: None,
            availability: CommandAvailability::Enabled,
            selected: Some(state.shell_emphasis_live),
        },
        CommandPresentation {
            command: AppCommand::FitAll,
            label: "Fit all plots".to_owned(),
            shortcut: None,
            availability: if context.has_data {
                CommandAvailability::Enabled
            } else {
                CommandAvailability::Disabled(
                    "Open a log or connect a live source first",
                )
            },
            selected: None,
        },
    ]);
    commands.extend(
        [SampleMode::Prev, SampleMode::Next, SampleMode::Linear]
            .into_iter()
            .map(|mode| CommandPresentation {
                command: AppCommand::SetCursorSampling(mode),
                label: format!("Cursor sampling: {}", sample_mode_label(mode)),
                shortcut: None,
                availability: CommandAvailability::Enabled,
                selected: Some(state.cursor_sampling == mode),
            }),
    );
    commands.extend(dynamic);
    commands
}

fn sample_mode_label(mode: SampleMode) -> &'static str {
    match mode {
        SampleMode::Prev => "Previous",
        SampleMode::Next => "Next",
        SampleMode::Linear => "Linear",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::app::dynamic_commands::{DynamicCommandNames, DynamicFamily};

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

    #[test]
    fn redesigned_shell_covers_static_and_dynamic_command_families() {
        assert!(CommandId::ALL.iter().all(|id| !id.spec().routes.is_empty()));
        assert!(dynamic_command_families().contains(&DynamicFamily::Parser));
        assert!(dynamic_command_families().contains(&DynamicFamily::Script));
        assert!(dynamic_command_families().contains(&DynamicFamily::Layout));
        assert!(dynamic_command_families().contains(&DynamicFamily::LiveLink));
    }

    #[test]
    fn every_static_command_has_one_canonical_classic_menu_owner() {
        let owned: std::collections::HashSet<_> = CommandId::ALL
            .iter()
            .map(|id| (*id, id.spec().classic_menu_owner))
            .collect();

        assert_eq!(owned.len(), CommandId::ALL.len());
        assert!(owned.iter().all(|(_, owner)| {
            matches!(
                owner,
                ClassicMenuOwner::File
                    | ClassicMenuOwner::View
                    | ClassicMenuOwner::Analyze
                    | ClassicMenuOwner::Tools
            )
        }));
    }

    #[test]
    fn dynamic_command_families_have_canonical_classic_menu_owners() {
        assert_eq!(
            AppCommand::OpenWithParser("csv".into()).classic_menu_owner(),
            ClassicMenuOwner::Tools
        );
        assert_eq!(
            AppCommand::RunScript("derive".into()).classic_menu_owner(),
            ClassicMenuOwner::Tools
        );
        assert_eq!(
            AppCommand::LoadNamedLayout("analysis".into()).classic_menu_owner(),
            ClassicMenuOwner::View
        );
        assert_eq!(
            AppCommand::DisconnectLink(0).classic_menu_owner(),
            ClassicMenuOwner::File
        );
    }

    #[test]
    fn generic_disconnect_is_distinct_from_named_link_disconnect() {
        assert_eq!(
            CommandId::DisconnectLive.spec().label,
            "Disconnect all live links"
        );
        assert_ne!(
            AppCommand::Static(CommandId::DisconnectLive),
            AppCommand::DisconnectLink(0)
        );
    }

    #[test]
    fn canonical_catalog_includes_shell_fit_and_every_cursor_sampling_choice() {
        let context = CommandContext::default();
        let presentations = present_commands(
            &context,
            &PresentationState {
                shell_emphasis_live: false,
                cursor_sampling: delog_core::field_view::SampleMode::Next,
                ..PresentationState::default()
            },
            [],
        );
        let commands: Vec<_> = presentations
            .iter()
            .map(|presentation| presentation.command.clone())
            .collect();

        assert!(commands.contains(&AppCommand::ToggleShellEmphasis));
        assert!(commands.contains(&AppCommand::FitAll));
        for mode in [
            delog_core::field_view::SampleMode::Prev,
            delog_core::field_view::SampleMode::Next,
            delog_core::field_view::SampleMode::Linear,
        ] {
            assert!(commands.contains(&AppCommand::SetCursorSampling(mode)));
        }

        let fit = presentations
            .iter()
            .find(|presentation| presentation.command == AppCommand::FitAll)
            .unwrap();
        assert_eq!(
            fit.availability,
            CommandAvailability::Disabled("Open a log or connect a live source first")
        );
        let next = presentations
            .iter()
            .find(|presentation| {
                presentation.command
                    == AppCommand::SetCursorSampling(
                        delog_core::field_view::SampleMode::Next,
                    )
            })
            .unwrap();
        assert_eq!(next.selected, Some(true));
    }

    #[test]
    fn dynamic_catalog_scans_once_until_explicitly_invalidated() {
        use std::cell::Cell;

        let scans = Cell::new(0);
        let mut catalog = DynamicCommandCatalog::default();
        let ensure = |catalog: &mut DynamicCommandCatalog| {
            catalog.ensure_with(|| {
                scans.set(scans.get() + 1);
                Ok::<_, ()>(DynamicCommandNames {
                    layouts: vec!["analysis".to_owned()],
                    scripts: vec!["derive".to_owned()],
                    parsers: vec!["custom".to_owned()],
                })
            });
        };

        let state = PresentationState {
            shell_emphasis_live: false,
            cursor_sampling: delog_core::field_view::SampleMode::Prev,
            ..PresentationState::default()
        };
        let build_presentations = |catalog: &mut DynamicCommandCatalog| {
            ensure(catalog);
            let dynamic = catalog
                .names()
                .layouts
                .iter()
                .map(|name| CommandPresentation {
                    command: AppCommand::LoadNamedLayout(name.clone()),
                    label: format!("Load layout: {name}"),
                    shortcut: None,
                    availability: CommandAvailability::Enabled,
                    selected: None,
                })
                .collect::<Vec<_>>();
            present_commands(&CommandContext::default(), &state, dynamic)
        };

        let first = build_presentations(&mut catalog);
        let second = build_presentations(&mut catalog);
        assert_eq!(scans.get(), 1, "normal frames must not rescan directories");
        assert_eq!(first, second);
        assert!(first.iter().any(|presentation| {
            presentation.command == AppCommand::LoadNamedLayout("analysis".to_owned())
        }));

        catalog.invalidate();
        let _ = build_presentations(&mut catalog);
        assert_eq!(scans.get(), 2, "explicit invalidation refreshes names once");
    }

    #[test]
    fn parser_only_work_enables_the_shared_cancel_presentation() {
        let context = CommandContext::for_frame(false, 0, 0, false, true, true);
        assert!(context.has_active_tasks);
        assert_eq!(
            CommandId::CancelTasks.availability(&context),
            CommandAvailability::Enabled
        );
    }

    #[test]
    fn dynamic_catalog_keeps_last_known_names_when_refresh_fails() {
        let mut catalog = DynamicCommandCatalog::default();
        catalog.ensure_with(|| {
            Ok::<_, ()>(DynamicCommandNames {
                layouts: vec!["analysis".to_owned()],
                scripts: vec!["derive".to_owned()],
                parsers: vec!["custom".to_owned()],
            })
        });
        let last_good = catalog.names().clone();

        catalog.invalidate();
        catalog.ensure_with(|| Err::<DynamicCommandNames, _>("transient read failure"));

        assert_eq!(catalog.names(), &last_good);
    }

    #[test]
    fn dynamic_refresh_isolates_a_failed_family_from_successful_families() {
        let previous = DynamicCommandNames {
            layouts: vec!["old-layout".to_owned()],
            scripts: vec!["old-script".to_owned()],
            parsers: vec!["old-parser".to_owned()],
        };

        let refreshed = merge_dynamic_command_refresh(
            &previous,
            Some(vec!["new-layout".to_owned()]),
            Some(vec!["new-script".to_owned()]),
            None,
        );

        assert_eq!(refreshed.layouts, ["new-layout"]);
        assert_eq!(refreshed.scripts, ["new-script"]);
        assert_eq!(refreshed.parsers, ["old-parser"]);
    }

    #[test]
    fn menu_and_toolbar_toggle_presentations_share_selected_and_disabled_state() {
        let state = PresentationState {
            data_browser_open: true,
            diagnostics_open: true,
            measuring_marker: true,
            ..PresentationState::default()
        };
        let presentations = present_commands(&CommandContext::default(), &state, []);
        let presentation = |id| {
            presentations
                .iter()
                .find(|item| item.command == AppCommand::Static(id))
                .unwrap()
        };

        assert_eq!(presentation(CommandId::ToggleDataBrowser).selected, Some(true));
        assert_eq!(presentation(CommandId::OpenDiagnostics).selected, Some(true));
        assert_eq!(presentation(CommandId::AddMeasuringMarker).selected, Some(true));
        assert_eq!(
            presentation(CommandId::AddMeasuringMarker).availability,
            CommandAvailability::Disabled("Open a log or connect a live source first")
        );
    }
}
