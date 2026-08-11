#![allow(dead_code)]

pub const APP: &str = include_str!("../src/shell/app/mod.rs");
pub const APP_COMMANDS: &str = include_str!("../src/shell/app/commands/specs.rs");
pub const BROWSER: &str = include_str!("../src/plotting/browser.rs");
pub const DATAFLOW_CANVAS: &str = include_str!("../src/dataflow/canvas.rs");
pub const DATAFLOW_WINDOW: &str = include_str!("../src/dataflow/window.rs");
pub const DATA_EXPORT: &str = include_str!("../src/export/data_export/mod.rs");
pub const DIAGNOSTICS: &str = include_str!("../src/ui/diagnostics.rs");
pub const DOCKS: &str = include_str!("../src/ui/docks.rs");
pub const GENERATE_MARKERS: &str = include_str!("../src/shell/generate_markers.rs");
pub const HOVER: &str = include_str!("../src/plotting/hover.rs");
pub const LEGEND: &str = include_str!("../src/plotting/legend.rs");
pub const LIVE: &str = include_str!("../src/ingest/live.rs");
pub const LOGGING: &str = include_str!("../src/ui/logging.rs");
pub const MARKERS: &str = include_str!("../src/plotting/markers.rs");
pub const MESSAGE_POPUP: &str = include_str!("../src/ui/message_popup.rs");
pub const PARSERS: &str = include_str!("../src/ingest/parsers.rs");
pub const PERFORMANCE: &str = include_str!("../src/ui/performance.rs");
pub const SCRIPTS: &str = include_str!("../src/scripting/scripts.rs");
pub const SETTINGS: &str = include_str!("../src/config/settings.rs");
pub const SYNC_WINDOW: &str = include_str!("../src/sync/sync_window/mod.rs");
pub const VEHICLE_DIALOG: &str = include_str!("../src/session/vehicle_dialog.rs");
pub const WORKSPACE: &str = include_str!("../src/shell/workspace/mod.rs");
pub const APP_MANIFEST: &str = include_str!("../Cargo.toml");
pub const CORE_INGEST: &str = include_str!("../../delog-core/src/ingest.rs");
pub const DATA_FLOW_DOCS: &str = include_str!("../../../docs/data_flow.md");
