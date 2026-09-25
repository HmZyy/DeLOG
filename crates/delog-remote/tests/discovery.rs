use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use delog_remote::discovery::{
    DiscoveryDescriptor, DiscoveryError, DiscoveryRegistration, prepare_default_root,
    remove_stale_descriptors,
};
use delog_remote::{InstanceDto, OpaqueId, SecretToken};
use serde_json::{Value, json};

fn descriptor_for(id: OpaqueId, label: &str, session: Option<&str>) -> DiscoveryDescriptor {
    let token = SecretToken::generate().unwrap();
    DiscoveryDescriptor::new(
        &InstanceDto::current(id, label, session.map(str::to_owned)),
        "127.0.0.1:4242".parse().unwrap(),
        &token,
    )
}

fn fixture_descriptor() -> DiscoveryDescriptor {
    descriptor_for(OpaqueId::generate().unwrap(), "bench", Some("flight.bin"))
}

fn entries(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    root
}

fn write_private(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
}

fn dead_pid() -> u32 {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--list")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn descriptor_json_with_pid(pid: u32) -> (String, Value) {
    let mut value = serde_json::to_value(fixture_descriptor()).unwrap();
    value["pid"] = json!(pid);
    (value["instance_id"].as_str().unwrap().to_owned(), value)
}

#[cfg(unix)]
#[test]
fn discovery_files_are_private_and_removed_on_drop() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let registration = DiscoveryRegistration::publish(root.path(), fixture_descriptor()).unwrap();
    assert_eq!(
        fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(registration.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let path = registration.path().to_owned();
    drop(registration);
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn unix_discovery_state_is_private_to_the_current_uid() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("delog").join("instances");
    let registration = DiscoveryRegistration::publish(&root, fixture_descriptor()).unwrap();
    let uid = rustix::process::getuid().as_raw();
    for (path, mode) in [
        (parent.path().join("delog"), 0o700),
        (root.clone(), 0o700),
        (registration.path().to_owned(), 0o600),
    ] {
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert_eq!(metadata.uid(), uid, "{}", path.display());
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            mode,
            "{}",
            path.display()
        );
    }
    let path = registration.path().to_owned();
    drop(registration);
    assert!(!path.exists());
}

#[cfg(windows)]
fn powershell(script: &str) -> String {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[cfg(windows)]
fn assert_private_dacl(path: &Path, user: &str) {
    let sddl = powershell(&format!(
        "(Get-Acl -LiteralPath '{}').Sddl",
        path.display().to_string().replace('\'', "''")
    ));
    let (_, dacl) = sddl
        .split_once("D:")
        .unwrap_or_else(|| panic!("{} has no DACL: {sddl}", path.display()));
    assert!(
        dacl.starts_with('P'),
        "{} DACL is not protected: {sddl}",
        path.display()
    );
    let mut user_allowed = false;
    for ace in dacl.split('(').skip(1) {
        let ace = ace.split(')').next().unwrap();
        let parts: Vec<&str> = ace.split(';').collect();
        let (kind, flags, sid) = (parts[0], parts[1], parts[5]);
        if kind == "D" {
            continue;
        }
        assert_eq!(kind, "A", "{} has a non-allow ACE: {sddl}", path.display());
        let trusted = sid == user || sid == "SY" || sid == "BA";
        let inherited_owner = sid == "CO" && flags.contains("IO");
        assert!(
            trusted || inherited_owner,
            "{} grants access to {sid}: {sddl}",
            path.display()
        );
        user_allowed |= sid == user;
    }
    assert!(
        user_allowed,
        "{} does not grant the current user: {sddl}",
        path.display()
    );
}

#[cfg(windows)]
#[test]
fn windows_discovery_dacl_grants_only_the_user_system_and_administrators() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("DeLOG").join("instances");
    let registration = DiscoveryRegistration::publish(&root, fixture_descriptor()).unwrap();
    let user = powershell("[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value");
    assert!(user.starts_with("S-1-5-"), "{user}");
    assert_private_dacl(&root, &user);
    assert_private_dacl(registration.path(), &user);
    let path = registration.path().to_owned();
    drop(registration);
    assert!(!path.exists());
}

#[test]
fn descriptor_json_matches_the_wire_contract() {
    let descriptor = fixture_descriptor();
    let token = descriptor.bootstrap_token().expose();
    let value = serde_json::to_value(&descriptor).unwrap();
    assert_eq!(
        value,
        json!({
            "format": 1,
            "instance_id": descriptor.instance_id().as_str(),
            "label": "bench",
            "pid": std::process::id(),
            "endpoint": "127.0.0.1:4242",
            "api_major": 1,
            "api_min_minor": 0,
            "api_max_minor": 0,
            "session_description": "flight.bin",
            "bootstrap_token": token,
        })
    );
    let bare =
        serde_json::to_value(descriptor_for(OpaqueId::generate().unwrap(), "x", None)).unwrap();
    assert!(bare.get("session_description").is_none());
    let parsed = DiscoveryDescriptor::parse(value.to_string().as_bytes()).unwrap();
    assert_eq!(parsed.bootstrap_token().expose(), token);
    assert_eq!(parsed.instance_id(), descriptor.instance_id());
    assert_eq!(parsed.pid(), std::process::id());
    let mut extra = value.clone();
    extra["unexpected"] = json!(true);
    assert!(DiscoveryDescriptor::parse(extra.to_string().as_bytes()).is_err());
    let mut future = value.clone();
    future["format"] = json!(2);
    assert!(DiscoveryDescriptor::parse(future.to_string().as_bytes()).is_err());
    let mut remote = value;
    remote["endpoint"] = json!("10.0.0.1:4242");
    assert!(DiscoveryDescriptor::parse(remote.to_string().as_bytes()).is_err());
}

#[test]
fn debug_output_redacts_the_bootstrap_token() {
    let root = tempfile::tempdir().unwrap();
    let descriptor = fixture_descriptor();
    let token = descriptor.bootstrap_token().expose();
    let debug = format!("{descriptor:?}");
    assert!(!debug.contains(&token));
    assert!(debug.contains("REDACTED"));
    let registration = DiscoveryRegistration::publish(root.path(), descriptor).unwrap();
    let debug = format!("{registration:?}");
    assert!(!debug.contains(&token));
    assert!(!registration.path().to_string_lossy().contains(&token));
}

#[test]
fn publication_atomically_replaces_existing_descriptors() {
    let root = tempfile::tempdir().unwrap();
    let id = OpaqueId::generate().unwrap();
    let target = root.path().join(format!("{id}.json"));
    write_private(&target, b"partial");
    let mut registration =
        DiscoveryRegistration::publish(root.path(), descriptor_for(id.clone(), "first", None))
            .unwrap();
    assert_eq!(registration.path(), target);
    let written = DiscoveryDescriptor::parse(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(written.label(), "first");
    registration
        .update(descriptor_for(id.clone(), "second", Some("b.bin")))
        .unwrap();
    let written = DiscoveryDescriptor::parse(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(written.label(), "second");
    assert_eq!(written.session_description(), Some("b.bin"));
    assert_eq!(entries(root.path()), vec![format!("{id}.json")]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(matches!(
        registration.update(fixture_descriptor()),
        Err(DiscoveryError::InvalidDescriptor)
    ));
    registration.remove().unwrap();
    assert!(entries(root.path()).is_empty());
}

#[test]
fn startup_removes_only_private_descriptors_of_dead_processes() {
    let root = private_root();
    let dead = dead_pid();
    let (stale_id, stale) = descriptor_json_with_pid(dead);
    write_private(
        &root.path().join(format!("{stale_id}.json")),
        stale.to_string().as_bytes(),
    );
    let (live_id, live) = descriptor_json_with_pid(std::process::id());
    write_private(
        &root.path().join(format!("{live_id}.json")),
        live.to_string().as_bytes(),
    );
    let (mismatch_id, _) = descriptor_json_with_pid(dead);
    write_private(
        &root.path().join(format!("{mismatch_id}.json")),
        stale.to_string().as_bytes(),
    );
    let (garbage_id, _) = descriptor_json_with_pid(dead);
    write_private(
        &root.path().join(format!("{garbage_id}.json")),
        b"{not json",
    );
    let staging_id = OpaqueId::generate().unwrap();
    write_private(
        &root.path().join(format!(".{staging_id}.{dead}.tmp")),
        b"partial",
    );
    let live_staging = format!(".{staging_id}.{}.tmp", std::process::id());
    write_private(&root.path().join(&live_staging), b"partial");
    write_private(&root.path().join("notes.txt"), b"keep");
    write_private(&root.path().join(format!("{dead}.tmp")), b"keep");
    let mut expected = vec![
        format!("{live_id}.json"),
        format!("{mismatch_id}.json"),
        format!("{garbage_id}.json"),
        live_staging,
        "notes.txt".to_owned(),
        format!("{dead}.tmp"),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let (open_id, open) = descriptor_json_with_pid(dead);
        let open_path = root.path().join(format!("{open_id}.json"));
        write_private(&open_path, open.to_string().as_bytes());
        fs::set_permissions(&open_path, fs::Permissions::from_mode(0o644)).unwrap();
        expected.push(format!("{open_id}.json"));
        let (link_id, _) = descriptor_json_with_pid(dead);
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("target.json");
        write_private(&target, stale.to_string().as_bytes());
        std::os::unix::fs::symlink(&target, root.path().join(format!("{link_id}.json"))).unwrap();
        expected.push(format!("{link_id}.json"));
        assert_eq!(remove_stale_descriptors(root.path()).unwrap(), 2);
        assert!(target.exists());
    }
    #[cfg(not(unix))]
    assert_eq!(remove_stale_descriptors(root.path()).unwrap(), 2);
    expected.sort();
    assert_eq!(entries(root.path()), expected);
    let (again_id, again) = descriptor_json_with_pid(dead);
    write_private(
        &root.path().join(format!("{again_id}.json")),
        again.to_string().as_bytes(),
    );
    let registration = DiscoveryRegistration::publish(root.path(), fixture_descriptor()).unwrap();
    assert!(!root.path().join(format!("{again_id}.json")).exists());
    drop(registration);
    assert_eq!(entries(root.path()), expected);
}

#[cfg(unix)]
#[test]
fn unix_publication_refuses_group_or_other_writable_directories() {
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o770, 0o702, 0o720, 0o777] {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(mode)).unwrap();
        assert!(matches!(
            DiscoveryRegistration::publish(root.path(), fixture_descriptor()),
            Err(DiscoveryError::Insecure { .. })
        ));
        assert!(matches!(
            remove_stale_descriptors(root.path()),
            Err(DiscoveryError::Insecure { .. })
        ));
        assert!(entries(root.path()).is_empty());
        let nested = root.path().join("instances");
        assert!(matches!(
            DiscoveryRegistration::publish(&nested, fixture_descriptor()),
            Err(DiscoveryError::Insecure { .. })
        ));
        assert!(entries(root.path()).is_empty());
    }
    let readable = tempfile::tempdir().unwrap();
    fs::set_permissions(readable.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let registration =
        DiscoveryRegistration::publish(readable.path(), fixture_descriptor()).unwrap();
    assert_eq!(
        fs::metadata(readable.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    drop(registration);
}

#[cfg(unix)]
#[test]
fn unix_publication_refuses_directories_owned_by_another_uid() {
    use std::os::unix::fs::MetadataExt;
    let mine = tempfile::tempdir().unwrap();
    let uid = fs::metadata(mine.path()).unwrap().uid();
    let Some(foreign) = ["/", "/usr", "/etc", "/proc"]
        .into_iter()
        .map(Path::new)
        .find(|path| fs::metadata(path).is_ok_and(|meta| meta.uid() != uid))
    else {
        return;
    };
    assert!(matches!(
        DiscoveryRegistration::publish(foreign, fixture_descriptor()),
        Err(DiscoveryError::Insecure { .. })
    ));
    assert!(matches!(
        remove_stale_descriptors(foreign),
        Err(DiscoveryError::Insecure { .. })
    ));
}

#[cfg(unix)]
#[test]
fn unix_publication_refuses_symlinked_directories_and_creates_missing_components_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(matches!(
        DiscoveryRegistration::publish(&link, fixture_descriptor()),
        Err(DiscoveryError::Insecure { .. })
    ));
    assert!(entries(&real).is_empty());
    let nested = root.path().join("a").join("b");
    let registration = DiscoveryRegistration::publish(&nested, fixture_descriptor()).unwrap();
    for dir in [root.path().join("a"), nested.clone()] {
        assert_eq!(
            fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    drop(registration);
}

const CHILD_EXPECT: &str = "DELOG_DISCOVERY_TEST_EXPECT";

fn run_default_root_child(configure: impl FnOnce(&mut Command), expected: &Path) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "default_root_follows_the_platform_path_contract",
            "--test-threads=1",
        ])
        .env(CHILD_EXPECT, expected);
    configure(&mut command);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[test]
fn default_root_follows_the_platform_path_contract() {
    if let Some(expected) = std::env::var_os(CHILD_EXPECT) {
        let expected = PathBuf::from(expected);
        assert_eq!(prepare_default_root().unwrap(), expected);
        assert!(expected.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [expected.parent().unwrap(), expected.as_path()] {
                assert_eq!(
                    fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
        let registration = DiscoveryRegistration::publish(&expected, fixture_descriptor()).unwrap();
        assert!(registration.path().starts_with(&expected));
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let runtime = tempfile::tempdir().unwrap();
        let expected = runtime.path().join("delog").join("instances");
        run_default_root_child(
            |command| {
                command.env("XDG_RUNTIME_DIR", runtime.path());
            },
            &expected,
        );
        let temp = tempfile::tempdir().unwrap();
        let uid = fs::metadata(temp.path()).unwrap().uid();
        let expected = temp
            .path()
            .join(format!("delog-runtime-{uid}"))
            .join("instances");
        run_default_root_child(
            |command| {
                command
                    .env_remove("XDG_RUNTIME_DIR")
                    .env("TMPDIR", temp.path());
            },
            &expected,
        );
        let temp = tempfile::tempdir().unwrap();
        let expected = temp
            .path()
            .join(format!("delog-runtime-{uid}"))
            .join("instances");
        run_default_root_child(
            |command| {
                command
                    .env("XDG_RUNTIME_DIR", "relative/runtime")
                    .env("TMPDIR", temp.path());
            },
            &expected,
        );
    }
    #[cfg(windows)]
    {
        let local = tempfile::tempdir().unwrap();
        let expected = local.path().join("DeLOG").join("runtime").join("instances");
        run_default_root_child(
            |command| {
                command.env("LOCALAPPDATA", local.path());
            },
            &expected,
        );
    }
}
