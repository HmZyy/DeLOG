const MANIFEST: &str = include_str!("../Cargo.toml");
const CORE_MANIFEST: &str = include_str!("../../delog-core/Cargo.toml");
const REMOTE_MANIFEST: &str = include_str!("../../delog-remote/Cargo.toml");
const SCRIPT_LIB: &str = include_str!("../../delog-script/src/lib.rs");

fn declares_dependency(manifest: &str, name: &str) -> bool {
    manifest.lines().any(|line| {
        line.trim_start()
            .strip_prefix(name)
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    })
}

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
        "tokio",
        "tonic",
        "serde_json",
        "bincode",
        "delog-remote",
    ] {
        assert!(
            !declares_dependency(MANIFEST, forbidden),
            "{forbidden} must not be a delog-api dependency"
        );
    }
}

#[test]
fn remote_server_depends_on_the_native_api_while_native_crates_stay_free_of_it() {
    for native_dependency in ["delog-api", "delog-core"] {
        assert!(
            declares_dependency(REMOTE_MANIFEST, native_dependency),
            "delog-remote must depend on {native_dependency}"
        );
    }
    for native_manifest in [MANIFEST, CORE_MANIFEST] {
        for forbidden in ["delog-remote", "axum", "tokio"] {
            assert!(
                !declares_dependency(native_manifest, forbidden),
                "{forbidden} must not be a native crate dependency"
            );
        }
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
