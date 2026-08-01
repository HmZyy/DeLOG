// Removed once the Context Canvas header and Inspector consume every helper.
#[allow(dead_code)]
pub mod components;
#[allow(dead_code)]
pub mod design_tokens;
pub mod diagnostics;
pub mod docks;
pub mod fuzzy;
// The pane-local tool migration consumes the complete icon set in Task 6.
#[allow(dead_code)]
pub mod icons;
pub mod logging;
pub mod message_popup;
pub mod performance;
pub mod theme;
