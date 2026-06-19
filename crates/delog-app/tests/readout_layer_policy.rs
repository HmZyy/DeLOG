const HOVER_SOURCE: &str = include_str!("../src/hover.rs");

#[test]
fn value_readout_uses_the_background_layer() {
    assert!(HOVER_SOURCE.contains("const READOUT_ORDER: egui::Order = egui::Order::Background;"));
    assert!(HOVER_SOURCE.contains(".order(READOUT_ORDER)"));
}
