const APP_RUN: &str = include_str!("../../../packaging/linux/AppRun");
const INSTALLER: &str = include_str!("../../../packaging/windows/delog.iss");
const RELEASE: &str = include_str!("../../../.github/workflows/release.yml");

const DISCOVERY_STATE: [&str; 9] = [
    "instances",
    "runtime",
    "bootstrap",
    "token",
    "instance_id",
    "delog-runtime",
    "xdg_runtime_dir",
    "delog_discovery_dir",
    ".json",
];

fn assert_no_discovery_state(name: &str, source: &str) {
    let lowered = source.to_lowercase();
    for needle in DISCOVERY_STATE {
        assert!(
            !lowered.contains(needle),
            "{name} must not reference external API discovery state ({needle})"
        );
    }
}

#[test]
fn linux_launcher_leaves_discovery_state_to_the_running_app() {
    assert_no_discovery_state("packaging/linux/AppRun", APP_RUN);
    assert!(!APP_RUN.contains("mkdir"));
    assert!(!APP_RUN.contains("rm "));
    assert!(!APP_RUN.contains("chmod"));
}

#[test]
fn windows_installer_neither_creates_nor_deletes_runtime_directories() {
    assert_no_discovery_state("packaging/windows/delog.iss", INSTALLER);
    for section in [
        "[Dirs]",
        "[UninstallDelete]",
        "[InstallDelete]",
        "[UninstallRun]",
    ] {
        assert!(
            !INSTALLER.contains(section),
            "the installer must not manage {section}"
        );
    }
    assert!(!INSTALLER.to_lowercase().contains("{localappdata}"));
}

#[test]
fn releases_attach_the_python_client_without_publishing_to_an_index() {
    assert!(RELEASE.contains("uv build --project python/delog-client"));
    assert!(RELEASE.contains("test_installed_wheel.py"));
    assert!(RELEASE.contains("python-client"));
    for publisher in ["pypi", "twine", "uv publish", "gh-action-pypi-publish"] {
        assert!(
            !RELEASE.to_lowercase().contains(publisher),
            "releases must not publish the Python client to an index ({publisher})"
        );
    }
}
