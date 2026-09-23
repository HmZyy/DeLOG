const MANIFEST: &str = include_str!("../Cargo.toml");

#[test]
fn native_api_has_no_python_ui_or_reverse_dependencies() {
    for forbidden in [
        "pyo3",
        "numpy",
        "delog-script",
        "delog-app",
        "egui",
        "eframe",
        "reqwest",
        "hyper",
        "axum",
        "tonic",
        "serde_json",
        "bincode",
    ] {
        let declared = MANIFEST.lines().any(|line| {
            line.trim_start()
                .strip_prefix(forbidden)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        });
        assert!(!declared, "{forbidden} must not be a delog-api dependency");
    }
}
