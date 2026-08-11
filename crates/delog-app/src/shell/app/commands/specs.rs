use super::{AccessRoute, ClassicMenuOwner, CommandGroup, CommandId, CommandSpec};

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
    pub const fn spec(self) -> CommandSpec {
        use CommandId::*;
        let mut spec = match self {
            Open => spec!(
                "Open log…",
                Source,
                Some("Ctrl+O"),
                "file import load",
                Header,
                ClassicMenu,
                Shortcut,
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
            DisconnectLive => {
                spec!(
                    "Disconnect all live links",
                    Source,
                    None,
                    "stop stream",
                    Header,
                    Palette
                )
            }
            CancelTasks => spec!(
                "Cancel active tasks",
                Source,
                None,
                "stop loading parser",
                Header,
                ClassicMenu,
                Palette
            ),
            ExportData => {
                spec!(
                    "Export data…",
                    Export,
                    None,
                    "csv parquet",
                    ClassicMenu,
                    Palette
                )
            }
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
                Some("Ctrl+E"),
                "signals topics sidebar",
                Header,
                ClassicMenu,
                Shortcut,
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
                Shortcut,
                Palette
            ),
            ManageLayouts => {
                spec!(
                    "Manage layouts…",
                    Workspace,
                    None,
                    "rename delete",
                    ClassicMenu,
                    Palette
                )
            }
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
                Some("="),
                "resize panels",
                Shortcut,
                Palette
            ),
            OpenDataFlow => {
                spec!(
                    "Data flow",
                    Analysis,
                    None,
                    "pipeline graph",
                    ClassicMenu,
                    Palette
                )
            }
            OpenScriptEditor => {
                spec!(
                    "Script editor…",
                    Extensions,
                    None,
                    "automation code",
                    ClassicMenu,
                    Palette
                )
            }
            OpenScriptVariables => spec!(
                "Script variables…",
                Extensions,
                None,
                "automation state",
                ClassicMenu,
                Palette
            ),
            OpenParserEditor => {
                spec!(
                    "Parser editor…",
                    Extensions,
                    None,
                    "custom decoder",
                    ClassicMenu,
                    Palette
                )
            }
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
            ToggleLegends => {
                spec!(
                    "Toggle legends",
                    Workspace,
                    None,
                    "plot key visibility",
                    Palette
                )
            }
            OpenFieldStats => {
                spec!(
                    "Field stats",
                    Workspace,
                    None,
                    "statistics traces",
                    GlobalToolbar,
                    Palette
                )
            }
            ToggleAnnotationToolbar => spec!(
                "Annotation toolbar",
                Workspace,
                None,
                "annotation annotate draw shapes toolbar",
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
            Exit => spec!(
                "Exit",
                Application,
                None,
                "quit close",
                ClassicMenu,
                Palette
            ),
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
}
