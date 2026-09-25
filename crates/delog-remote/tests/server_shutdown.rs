use delog_core::snapshot::DataStore;
use delog_remote::{RemoteConfig, RemoteServer};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
mod support;

fn config(root: &tempfile::TempDir) -> RemoteConfig {
    RemoteConfig {
        label: "loopback".into(),
        loaded_file: None,
        lease_idle_timeout: Duration::from_secs(60),
        request_timeout: Duration::from_secs(2),
        max_concurrent_downloads: 2,
        discovery_root: Some(root.path().to_owned()),
        control: delog_remote::ControlLimits::default(),
        uploads: delog_remote::UploadConfig::default(),
    }
}

#[test]
fn loopback_registration_snapshot_status_disconnect_shutdown_and_restart() {
    let root = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let server =
            RemoteServer::spawn(config(&root), support::services(Arc::new(DataStore::new())))
                .unwrap();
        let endpoint = server.endpoint();
        assert!(endpoint.ip().is_loopback());
        let url = format!("http://{endpoint}");
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let bootstrap = server.bootstrap_token().expose();
        let response = client
            .post(format!("{url}/v1/clients"))
            .bearer_auth(&bootstrap)
            .header("content-type", "application/json")
            .body(json!({"name":"reader"}).to_string())
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let registered: Value = serde_json::from_str(&response.text().unwrap()).unwrap();
        let token = registered["token"].as_str().unwrap();
        let response = client
            .post(format!("{url}/v1/snapshots"))
            .bearer_auth(token)
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let lease: Value = serde_json::from_str(&response.text().unwrap()).unwrap();
        let id = lease["lease_id"].as_str().unwrap();
        assert_eq!(
            client
                .get(format!("{url}/v1/snapshots/{id}/catalog"))
                .bearer_auth(token)
                .send()
                .unwrap()
                .status()
                .as_u16(),
            200
        );
        let status = server.status();
        assert_eq!(status.clients.len(), 1);
        assert_eq!(status.clients[0].owner_name, "reader");
        assert_eq!(status.leases.len(), 1);
        assert_eq!(status.leases[0].active_readers, 0);
        assert_eq!(status.leases[0].client, status.clients[0].client_id);
        let debug = format!("{server:?} {status:?}");
        assert!(!debug.contains(&bootstrap));
        assert!(!debug.contains(token));
        assert_eq!(
            client
                .delete(format!("{url}/v1/snapshots/{id}"))
                .bearer_auth(token)
                .send()
                .unwrap()
                .status()
                .as_u16(),
            200
        );
        assert!(server.status().leases.is_empty());
        assert!(server.revoke_client(&status.clients[0].client_id));
        assert_eq!(
            client
                .get(format!("{url}/v1/instance"))
                .bearer_auth(token)
                .send()
                .unwrap()
                .status()
                .as_u16(),
            403
        );
        server.shutdown().unwrap();
        assert!(
            std::net::TcpStream::connect_timeout(&endpoint, Duration::from_millis(100)).is_err()
        );
    }
}

#[test]
fn idle_clients_are_reaped_using_the_lease_idle_timeout() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(&root);
    cfg.lease_idle_timeout = Duration::from_millis(60);
    let server = RemoteServer::spawn(cfg, support::services(Arc::new(DataStore::new()))).unwrap();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    let response = client
        .post(format!("http://{}/v1/clients", server.endpoint()))
        .bearer_auth(server.bootstrap_token().expose())
        .body(json!({"name":"idle"}).to_string())
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !server.status().clients.is_empty() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    server.shutdown().unwrap();
}

#[test]
fn invalid_resource_limits_fail_before_startup() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(&root);
    cfg.max_concurrent_downloads = 0;
    assert!(RemoteServer::spawn(cfg, support::services(Arc::new(DataStore::new()))).is_err());
}

#[test]
fn shutdown_deadline_includes_cancellation_during_large_snapshot_preparation() {
    let store = support::wide_store(30_000);
    let snapshot = store.load();
    let baseline = Arc::strong_count(&snapshot);
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(&root);
    cfg.request_timeout = Duration::from_millis(100);
    let server = RemoteServer::spawn(cfg, support::services(store.clone())).unwrap();
    let endpoint = server.endpoint();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let registered: Value = serde_json::from_str(
        &client
            .post(format!("http://{endpoint}/v1/clients"))
            .bearer_auth(server.bootstrap_token().expose())
            .body(json!({"name":"reader"}).to_string())
            .send()
            .unwrap()
            .text()
            .unwrap(),
    )
    .unwrap();
    let token = registered["token"].as_str().unwrap().to_owned();
    let request = std::thread::spawn(move || {
        client
            .post(format!("http://{endpoint}/v1/snapshots"))
            .bearer_auth(token)
            .send()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while Arc::strong_count(&snapshot) == baseline {
        assert!(
            Instant::now() < deadline,
            "snapshot preparation did not start"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let started = Instant::now();
    let shutdown = server.shutdown();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "shutdown exceeded the 100 ms budget (plus scheduling tolerance): {:?}, {shutdown:?}",
        started.elapsed()
    );
    assert!(matches!(
        shutdown,
        Ok(()) | Err(delog_remote::ShutdownError::Timeout)
    ));
    if let Ok(response) = request.join().unwrap() {
        assert!(
            !response.status().is_success(),
            "cancelled preparation published a lease"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while Arc::strong_count(&snapshot) != baseline {
        assert!(
            Instant::now() < deadline,
            "cancelled preparation retained its snapshot"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn active_http_streams_release_snapshot_guards_on_revocation_expiry_and_shutdown() {
    use std::io::Read;
    for action in ["client", "lease", "expiry", "shutdown"] {
        let store = support::store(128, 16384);
        let snapshot = store.load();
        let baseline = Arc::strong_count(&snapshot);
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(&root);
        if action == "expiry" {
            cfg.lease_idle_timeout = Duration::from_millis(400);
        }
        let server = RemoteServer::spawn(cfg, support::services(store.clone())).unwrap();
        let endpoint = server.endpoint();
        let url = format!("http://{endpoint}");
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let registered: Value = serde_json::from_str(
            &client
                .post(format!("{url}/v1/clients"))
                .bearer_auth(server.bootstrap_token().expose())
                .body(json!({"name":"streamer"}).to_string())
                .send()
                .unwrap()
                .text()
                .unwrap(),
        )
        .unwrap();
        let token = registered["token"].as_str().unwrap();
        let lease: Value = serde_json::from_str(
            &client
                .post(format!("{url}/v1/snapshots"))
                .bearer_auth(token)
                .send()
                .unwrap()
                .text()
                .unwrap(),
        )
        .unwrap();
        let id = lease["lease_id"].as_str().unwrap();
        let catalog: Value = serde_json::from_str(
            &client
                .get(format!("{url}/v1/snapshots/{id}/catalog"))
                .bearer_auth(token)
                .send()
                .unwrap()
                .text()
                .unwrap(),
        )
        .unwrap();
        let topic = catalog["sources"][0]["topics"][0]["handle"]
            .as_str()
            .unwrap();
        let mut response = client
            .get(format!("{url}/v1/snapshots/{id}/topics/{topic}/data"))
            .bearer_auth(token)
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let mut prefix = [0; 1024];
        assert!(response.read(&mut prefix).unwrap() > 0);
        assert_eq!(server.status().leases[0].active_readers, 1);
        let mut server = Some(server);
        match action {
            "client" => {
                assert!(
                    server
                        .as_ref()
                        .unwrap()
                        .revoke_client(&server.as_ref().unwrap().status().clients[0].client_id)
                );
            }
            "lease" => {
                assert!(
                    server
                        .as_ref()
                        .unwrap()
                        .revoke_lease(&server.as_ref().unwrap().status().leases[0].id)
                );
            }
            "shutdown" => {
                server.take().unwrap().shutdown().unwrap();
            }
            "expiry" => {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !server.as_ref().unwrap().status().leases.is_empty() {
                    assert!(Instant::now() < deadline);
                    assert_eq!(
                        client
                            .get(format!("{url}/v1/instance"))
                            .bearer_auth(token)
                            .send()
                            .unwrap()
                            .status()
                            .as_u16(),
                        200
                    );
                    std::thread::sleep(Duration::from_millis(30));
                }
                assert_eq!(server.as_ref().unwrap().status().clients.len(), 1);
                assert_eq!(
                    client
                        .get(format!("{url}/v1/snapshots/{id}/catalog"))
                        .bearer_auth(token)
                        .send()
                        .unwrap()
                        .status()
                        .as_u16(),
                    410
                );
            }
            _ => unreachable!(),
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Arc::strong_count(&snapshot) != baseline {
            assert!(
                Instant::now() < deadline,
                "snapshot guard retained after {action}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut remaining = Vec::new();
        assert!(
            response.read_to_end(&mut remaining).is_err(),
            "cancelled stream completed after {action}"
        );
        if let Some(server) = server {
            server.shutdown().unwrap();
        }
        assert!(
            std::net::TcpStream::connect_timeout(&endpoint, Duration::from_millis(100)).is_err()
        );
    }
}

fn published_descriptors(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect()
}

#[test]
fn server_publishes_discovery_after_binding_and_removes_it_on_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(&root);
    cfg.loaded_file = Some(
        std::path::Path::new("flights")
            .join("2026")
            .join("flight.bin")
            .to_string_lossy()
            .into_owned(),
    );
    let server = RemoteServer::spawn(cfg, support::services(Arc::new(DataStore::new()))).unwrap();
    let paths = published_descriptors(root.path());
    assert_eq!(paths.len(), 1);
    let descriptor =
        delog_remote::DiscoveryDescriptor::parse(&std::fs::read(&paths[0]).unwrap()).unwrap();
    assert_eq!(descriptor.endpoint(), server.endpoint());
    assert_eq!(descriptor.pid(), std::process::id());
    assert_eq!(descriptor.label(), "loopback");
    assert_eq!(descriptor.session_description(), Some("flight.bin"));
    let token = server.bootstrap_token().expose();
    assert_eq!(descriptor.bootstrap_token().expose(), token);
    assert!(!paths[0].to_string_lossy().contains(&token));
    let instance: Value = serde_json::from_str(
        &reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{}/v1/instance", server.endpoint()))
            .bearer_auth(&token)
            .send()
            .unwrap()
            .text()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(instance["instance_id"], descriptor.instance_id().as_str());
    assert_eq!(instance["session_description"], "flight.bin");
    assert_eq!(
        paths[0].file_name().unwrap().to_string_lossy(),
        format!("{}.json", descriptor.instance_id())
    );
    let endpoint = server.endpoint();
    server.shutdown().unwrap();
    assert!(std::net::TcpStream::connect_timeout(&endpoint, Duration::from_millis(100)).is_err());
    assert!(published_descriptors(root.path()).is_empty());
}

#[test]
fn dropping_the_server_removes_its_discovery_descriptor() {
    let root = tempfile::tempdir().unwrap();
    let server =
        RemoteServer::spawn(config(&root), support::services(Arc::new(DataStore::new()))).unwrap();
    assert_eq!(published_descriptors(root.path()).len(), 1);
    drop(server);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !published_descriptors(root.path()).is_empty() {
        assert!(Instant::now() < deadline, "descriptor outlived the server");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn insecure_discovery_root_fails_startup_without_publishing() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let error = RemoteServer::spawn(config(&root), support::services(Arc::new(DataStore::new())))
        .unwrap_err();
    assert!(matches!(error, delog_remote::StartError::Discovery(_)));
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
}

struct OpenLease {
    client: reqwest::blocking::Client,
    url: String,
    token: String,
    lease: String,
    topic: String,
}

impl OpenLease {
    fn new(server: &delog_remote::RemoteServerHandle, name: &str) -> Self {
        let url = format!("http://{}", server.endpoint());
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let registered: Value = serde_json::from_str(
            &client
                .post(format!("{url}/v1/clients"))
                .bearer_auth(server.bootstrap_token().expose())
                .body(json!({ "name": name }).to_string())
                .send()
                .unwrap()
                .text()
                .unwrap(),
        )
        .unwrap();
        let token = registered["token"].as_str().unwrap().to_owned();
        let lease: Value = serde_json::from_str(
            &client
                .post(format!("{url}/v1/snapshots"))
                .bearer_auth(&token)
                .send()
                .unwrap()
                .text()
                .unwrap(),
        )
        .unwrap();
        let lease = lease["lease_id"].as_str().unwrap().to_owned();
        let mut open = Self {
            client,
            url,
            token,
            lease,
            topic: String::new(),
        };
        let catalog: Value = serde_json::from_str(&open.get("catalog").text().unwrap()).unwrap();
        open.topic = catalog["sources"][0]["topics"][0]["handle"]
            .as_str()
            .unwrap()
            .to_owned();
        open
    }

    fn get(&self, tail: &str) -> reqwest::blocking::Response {
        self.client
            .get(format!("{}/v1/snapshots/{}/{tail}", self.url, self.lease))
            .bearer_auth(&self.token)
            .send()
            .unwrap()
    }

    fn data(&self) -> reqwest::blocking::Response {
        self.get(&format!("topics/{}/data", self.topic))
    }
}

#[test]
fn steady_reader_outlives_several_idle_timeouts_and_keeps_its_client() {
    use std::io::Read;
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(&root);
    cfg.lease_idle_timeout = Duration::from_secs(1);
    let server = RemoteServer::spawn(cfg, support::services(support::store(128, 16384))).unwrap();
    let open = OpenLease::new(&server, "steady");
    let mut response = open.data();
    assert_eq!(response.status().as_u16(), 200);
    let started = Instant::now();
    let mut buffer = vec![0; 64 * 1024];
    while started.elapsed() < Duration::from_millis(3_500) {
        let mut filled = 0;
        while filled < buffer.len() {
            let read = response.read(&mut buffer[filled..]).unwrap();
            assert!(read > 0, "stream ended while the reader was steady");
            filled += read;
        }
        assert_eq!(server.status().clients.len(), 1);
        assert_eq!(server.status().leases.len(), 1);
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut remaining = Vec::new();
    response.read_to_end(&mut remaining).unwrap();
    assert_eq!(server.status().clients.len(), 1);
    assert_eq!(open.get("catalog").status().as_u16(), 200);
    server.shutdown().unwrap();
}

#[test]
fn abandoned_reader_releases_its_download_slot_while_the_lease_stays_active() {
    use std::io::Read;
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(&root);
    cfg.lease_idle_timeout = Duration::from_millis(400);
    cfg.request_timeout = Duration::from_secs(3);
    cfg.max_concurrent_downloads = 1;
    let server = RemoteServer::spawn(cfg, support::services(support::store(128, 16384))).unwrap();
    let open = OpenLease::new(&server, "stalled");
    let mut stalled = open.data();
    assert_eq!(stalled.status().as_u16(), 200);
    let mut prefix = [0; 1024];
    assert!(stalled.read(&mut prefix).unwrap() > 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.status().leases[0].active_readers != 0 {
        assert!(
            Instant::now() < deadline,
            "stalled stream kept its reader guard"
        );
        assert_eq!(open.get("catalog").status().as_u16(), 200);
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut second = open.data();
    assert_eq!(second.status().as_u16(), 200);
    let mut body = Vec::new();
    second.read_to_end(&mut body).unwrap();
    let mut rest = Vec::new();
    assert!(stalled.read_to_end(&mut rest).is_err());
    assert_eq!(server.status().clients.len(), 1);
    server.shutdown().unwrap();
}

#[test]
fn stalled_stream_does_not_keep_its_lease_or_client_alive() {
    use std::io::Read;
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(&root);
    cfg.lease_idle_timeout = Duration::from_millis(300);
    cfg.request_timeout = Duration::from_millis(300);
    let store = support::store(128, 16384);
    let snapshot = store.load();
    let baseline = Arc::strong_count(&snapshot);
    let server = RemoteServer::spawn(cfg, support::services(store)).unwrap();
    let open = OpenLease::new(&server, "idle-stream");
    let mut stalled = open.data();
    let mut prefix = [0; 1024];
    assert!(stalled.read(&mut prefix).unwrap() > 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = server.status();
        if status.leases.is_empty()
            && status.clients.is_empty()
            && Arc::strong_count(&snapshot) == baseline
        {
            break;
        }
        assert!(Instant::now() < deadline, "stalled stream stayed alive");
        std::thread::sleep(Duration::from_millis(20));
    }
    server.shutdown().unwrap();
}

#[test]
fn session_updates_refresh_the_descriptor_and_instance_metadata() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(&root);
    cfg.loaded_file = Some("first.bin".into());
    let server = RemoteServer::spawn(cfg, support::services(Arc::new(DataStore::new()))).unwrap();
    let read_descriptor = || {
        let paths = published_descriptors(root.path());
        assert_eq!(paths.len(), 1);
        delog_remote::DiscoveryDescriptor::parse(&std::fs::read(&paths[0]).unwrap()).unwrap()
    };
    let instance = || -> Value {
        serde_json::from_str(
            &reqwest::blocking::Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(format!("http://{}/v1/instance", server.endpoint()))
                .bearer_auth(server.bootstrap_token().expose())
                .send()
                .unwrap()
                .text()
                .unwrap(),
        )
        .unwrap()
    };
    assert!(
        !server
            .update_session("loopback", Some("first.bin"))
            .unwrap()
    );
    let renamed = std::path::Path::new("flights")
        .join("second.bin")
        .to_string_lossy()
        .into_owned();
    assert!(server.update_session("renamed", Some(&renamed)).unwrap());
    let descriptor = read_descriptor();
    assert_eq!(descriptor.label(), "renamed");
    assert_eq!(descriptor.session_description(), Some("second.bin"));
    assert_eq!(descriptor.instance_id(), server.instance_id());
    assert_eq!(descriptor.endpoint(), server.endpoint());
    let current = instance();
    assert_eq!(current["label"], "renamed");
    assert_eq!(current["session_description"], "second.bin");
    assert!(!server.update_session("renamed", Some(&renamed)).unwrap());
    assert!(server.update_session("empty", None).unwrap());
    assert_eq!(read_descriptor().session_description(), None);
    assert!(instance().get("session_description").is_none());
    server.shutdown().unwrap();
    assert!(published_descriptors(root.path()).is_empty());
}
