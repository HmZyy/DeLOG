#[path = "policy_sources.rs"]
mod policy_sources;

use policy_sources::HOVER;

#[test]
fn value_readout_uses_the_background_layer() {
    assert!(HOVER.contains("const READOUT_ORDER: egui::Order = egui::Order::Background;"));
    assert!(HOVER.contains(".order(READOUT_ORDER)"));
}
