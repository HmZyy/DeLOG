const MANIFEST: &str = include_str!("../Cargo.toml");
const SCRIPT_LIB: &str = include_str!("../../delog-script/src/lib.rs");

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

#[test]
fn script_crate_does_not_reexport_native_api_types() {
    let forbidden = [
        "Annotation",
        "Control",
        "Generation",
        "Layout",
        "LoadReport",
        "Marker",
        "Param",
        "PendingMarker",
        "Playback",
        "PlotInfo",
        "Profile",
        "ScriptOwner",
        "SplitDirection",
        "Trace",
        "Vehicle",
    ];
    for line in SCRIPT_LIB
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("pub use "))
    {
        for name in forbidden {
            assert!(
                !line.contains(name),
                "delog-script must not re-export native {name} types: {line}"
            );
        }
    }
}
