use super::*;
use delog_api::control::AccessMode;
use delog_core::identity::IdentityRegistry;

fn ingest_sender() -> IngestSender {
    delog_core::ingest::ingest_channel().0
}

fn snapshot_with_labels(labels: &[&str]) -> StoreSnapshot {
    let mut identity = IdentityRegistry::new();
    for label in labels {
        identity.add_source(*label);
    }
    StoreSnapshot::from_registry(&identity, [], 0).expect("valid empty snapshot")
}

fn fake_client_id() -> ClientId {
    serde_json::from_str("\"unknown-client\"").unwrap()
}

fn fake_lease_id() -> LeaseId {
    serde_json::from_str("\"unknown-lease\"").unwrap()
}

fn tempdir_config(discovery: &tempfile::TempDir) -> RemoteConfig {
    RemoteConfig {
        label: "test-session".into(),
        loaded_file: None,
        lease_idle_timeout: Duration::from_secs(60),
        request_timeout: Duration::from_secs(2),
        max_concurrent_downloads: 2,
        discovery_root: Some(discovery.path().to_owned()),
        control: ControlLimits::default(),
        uploads: delog_remote::UploadConfig::default(),
    }
}

struct NoControl;

impl AuthorizedControlHost for NoControl {
    fn call_as(
        &self,
        _principal: delog_api::control::ControlPrincipal,
        _request: delog_api::control::ControlRequest,
    ) -> delog_api::Result<delog_api::control::ControlResponse> {
        Ok(delog_api::control::ControlResponse::Unit)
    }
}

fn controller() -> ExternalApiController {
    ExternalApiController::new(egui::Context::default(), Arc::new(NoControl))
}

#[test]
fn controller_is_off_until_explicit_enable_and_disable_preserves_store_data() {
    let discovery = tempfile::tempdir().unwrap();
    let store = Arc::new(DataStore::new());
    let mut controller = controller();
    assert_eq!(controller.status(), ExternalApiStatus::Disabled);

    controller
        .enable(
            tempdir_config(&discovery),
            Arc::clone(&store),
            ingest_sender(),
        )
        .unwrap();
    assert!(matches!(controller.status(), ExternalApiStatus::Running(_)));

    controller.disable().unwrap();
    assert_eq!(controller.status(), ExternalApiStatus::Disabled);
    assert_eq!(store.current_epoch(), 0);
}

#[test]
fn enable_failure_is_recorded_and_leaves_the_controller_disabled() {
    let discovery = tempfile::tempdir().unwrap();
    let store = Arc::new(DataStore::new());
    let mut controller = controller();
    let mut config = tempdir_config(&discovery);
    config.max_concurrent_downloads = 0;

    let error = controller
        .enable(config, store, ingest_sender())
        .unwrap_err();
    assert!(matches!(error, StartError::InvalidConfig));
    assert_eq!(controller.status(), ExternalApiStatus::Disabled);
    assert_eq!(
        controller.last_error(),
        Some(StartError::InvalidConfig.to_string().as_str())
    );
}

#[test]
fn server_status_and_revoke_are_inert_while_disabled() {
    let controller = controller();
    assert!(controller.server_status().is_none());
    assert!(!controller.revoke_client(&fake_client_id()));
    assert!(!controller.revoke_lease(&fake_lease_id()));
}

#[test]
fn enable_then_revoke_unknown_ids_report_nothing_found() {
    let discovery = tempfile::tempdir().unwrap();
    let store = Arc::new(DataStore::new());
    let mut controller = controller();
    controller
        .enable(tempdir_config(&discovery), store, ingest_sender())
        .unwrap();

    let status = controller.server_status().expect("running server status");
    assert!(status.clients.is_empty());
    assert!(status.leases.is_empty());
    assert!(!controller.revoke_client(&fake_client_id()));
    assert!(!controller.revoke_lease(&fake_lease_id()));

    controller.disable().unwrap();
}

#[test]
fn instance_label_joins_active_source_labels_and_skips_removed_ones() {
    let mut identity = IdentityRegistry::new();
    let first = identity.add_source("flight-a");
    identity.add_source("flight-b");
    identity.remove_source(first);
    let snapshot = StoreSnapshot::from_registry(&identity, [], 0).unwrap();

    assert_eq!(instance_label(&snapshot), "flight-b");
}

#[test]
fn instance_label_falls_back_when_there_is_no_open_source() {
    let snapshot = snapshot_with_labels(&[]);
    assert_eq!(instance_label(&snapshot), "DeLOG session");
}

#[test]
fn loaded_file_is_a_basename_never_a_path() {
    let mut identity = IdentityRegistry::new();
    identity.add_source_from_path("/home/pilot/flights/2026-09-24.ulog");
    let snapshot = StoreSnapshot::from_registry(&identity, [], 0).unwrap();

    let loaded_file = loaded_file_basename(&snapshot).unwrap();
    assert!(!loaded_file.contains('/'));
    assert!(!loaded_file.contains('\\'));
    assert_eq!(loaded_file, "2026-09-24");
}

#[test]
fn build_config_uses_none_discovery_root_and_clamps_zero_limits() {
    let snapshot = snapshot_with_labels(&["flight-a"]);
    let limits = ExternalApiLimits {
        lease_idle_timeout_secs: 0,
        request_timeout_secs: 0,
        max_concurrent_downloads: 0,
        upload_max_mib: 0,
        upload_max_rows: 0,
        upload_max_fields: 0,
        max_concurrent_uploads: 0,
        max_queued_controls: 0,
        max_queued_controls_per_client: 0,
        control_timeout_secs: 0,
    };
    let config = build_config(&snapshot, limits, None);

    assert!(config.discovery_root.is_none());
    assert_eq!(config.lease_idle_timeout, Duration::from_secs(1));
    assert_eq!(config.request_timeout, Duration::from_secs(1));
    assert_eq!(config.max_concurrent_downloads, 1);
    assert_eq!(config.label, "flight-a");
}

#[test]
fn start_errors_are_reported_with_their_whole_source_chain() {
    let error = StartError::Discovery(delog_remote::DiscoveryError::Io(std::io::Error::other(
        "disk quota exceeded",
    )));
    assert_eq!(
        error_chain(&error),
        "could not publish instance discovery: discovery file operation failed: disk quota exceeded"
    );
    assert_eq!(
        error_chain(&StartError::InvalidConfig),
        StartError::InvalidConfig.to_string()
    );
}

#[cfg(unix)]
#[test]
fn insecure_discovery_root_failure_records_the_cause() {
    use std::os::unix::fs::PermissionsExt;
    let discovery = tempfile::tempdir().unwrap();
    std::fs::set_permissions(discovery.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let mut controller = controller();
    let error = controller
        .enable(
            tempdir_config(&discovery),
            Arc::new(DataStore::new()),
            ingest_sender(),
        )
        .unwrap_err();
    assert!(matches!(error, StartError::Discovery(_)));
    let recorded = controller.last_error().unwrap();
    assert!(recorded.starts_with("could not publish instance discovery: "));
    assert!(recorded.contains("failed the current-user permission check"));
}

fn published_descriptor(discovery: &tempfile::TempDir) -> delog_remote::DiscoveryDescriptor {
    let paths: Vec<_> = std::fs::read_dir(discovery.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(paths.len(), 1);
    delog_remote::DiscoveryDescriptor::parse(&std::fs::read(&paths[0]).unwrap()).unwrap()
}

#[test]
fn source_label_changes_refresh_the_published_descriptor_only_on_change() {
    let discovery = tempfile::tempdir().unwrap();
    let store = Arc::new(DataStore::new());
    let mut controller = controller();
    let first = snapshot_with_labels(&["flight-a"]);
    controller
        .enable(
            build_config(
                &first,
                ExternalApiLimits::default(),
                Some(discovery.path().to_owned()),
            ),
            Arc::clone(&store),
            ingest_sender(),
        )
        .unwrap();

    assert!(!controller.sync_session(&first).unwrap());
    assert_eq!(published_descriptor(&discovery).label(), "flight-a");

    let second = snapshot_with_labels(&["flight-b", "flight-c"]);
    assert!(controller.sync_session(&second).unwrap());
    let descriptor = published_descriptor(&discovery);
    assert_eq!(descriptor.label(), "flight-b, flight-c");
    assert_eq!(descriptor.session_description(), Some("flight-b"));
    assert!(!controller.sync_session(&second).unwrap());

    controller.disable().unwrap();
    assert!(!controller.sync_session(&first).unwrap());
}

#[test]
fn closed_window_frames_still_refresh_the_descriptor() {
    let discovery = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let store = Arc::new(DataStore::new());
    let mut controller = ExternalApiController::new(ctx.clone(), Arc::new(NoControl));
    controller
        .enable(
            tempdir_config(&discovery),
            Arc::clone(&store),
            ingest_sender(),
        )
        .unwrap();
    let snapshot = snapshot_with_labels(&["flight-z"]);

    let logs = controller.frame(&ctx, &snapshot);

    assert!(logs.is_empty());
    assert_eq!(published_descriptor(&discovery).label(), "flight-z");
    controller.disable().unwrap();
}

fn show(
    controller: &mut ExternalApiController,
    ui: &mut egui::Ui,
    snapshot: &StoreSnapshot,
    store: &Arc<DataStore>,
) -> Vec<(crate::ui::logging::LogLevel, String)> {
    let mut logs = controller.frame(ui.ctx(), snapshot);
    logs.extend(controller.settings_ui(ui, snapshot, Arc::clone(store), ingest_sender()));
    logs
}

fn run_frames(ctx: &egui::Context, mut body: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
    let mut output = ctx.run_ui(egui::RawInput::default(), &mut body);
    for _ in 0..2 {
        output = ctx.run_ui(egui::RawInput::default(), &mut body);
    }
    output
}

fn find_text_rect(shape: &egui::epaint::Shape, expected: &str) -> Option<egui::Rect> {
    match shape {
        egui::epaint::Shape::Text(text) if text.galley.job.text == expected => {
            Some(text.visual_bounding_rect())
        }
        egui::epaint::Shape::Vec(shapes) => shapes
            .iter()
            .find_map(|shape| find_text_rect(shape, expected)),
        _ => None,
    }
}

fn painted(output: &egui::FullOutput, expected: &str) -> bool {
    output
        .shapes
        .iter()
        .any(|shape| find_text_rect(&shape.shape, expected).is_some())
}

#[test]
fn disabled_window_paints_the_enable_button_and_no_snippet() {
    let ctx = egui::Context::default();
    let store = Arc::new(DataStore::new());
    let mut controller = ExternalApiController::new(ctx.clone(), Arc::new(NoControl));
    let snapshot = StoreSnapshot::empty();

    let output = run_frames(&ctx, |ui| {
        let _ = show(&mut controller, ui, &snapshot, &store);
    });

    assert!(painted(&output, "Status"));
    assert!(painted(&output, "Enable"));
    assert!(!painted(&output, "Disable"));
}

#[test]
fn running_tab_paints_the_endpoint_and_instance_id_without_the_token() {
    let discovery = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let store = Arc::new(DataStore::new());
    let mut controller = ExternalApiController::new(ctx.clone(), Arc::new(NoControl));
    controller
        .enable(
            tempdir_config(&discovery),
            Arc::clone(&store),
            ingest_sender(),
        )
        .unwrap();
    let instance_id = controller.running.as_ref().unwrap().instance_id.clone();
    let snapshot = StoreSnapshot::empty();

    let output = run_frames(&ctx, |ui| {
        let _ = show(&mut controller, ui, &snapshot, &store);
    });

    let endpoint = controller.running.as_ref().unwrap().endpoint;
    assert!(painted(&output, &format!("Running at {endpoint}")));
    assert!(painted(&output, "Instance ID"));
    assert!(painted(&output, &instance_id));
    assert!(painted(&output, "Copy"));
    assert!(!output.shapes.iter().any(|shape| {
        fn contains(shape: &egui::epaint::Shape) -> bool {
            match shape {
                egui::epaint::Shape::Text(text) => text.galley.job.text.contains("DeLOG.connect"),
                egui::epaint::Shape::Vec(shapes) => shapes.iter().any(contains),
                _ => false,
            }
        }
        contains(&shape.shape)
    }));
    assert!(painted(&output, "Disable"));
    assert!(!painted(&output, "Enable"));

    let token = controller
        .handle
        .as_ref()
        .unwrap()
        .bootstrap_token()
        .expose();
    assert!(!painted(&output, &token));

    controller.disable().unwrap();
}

#[test]
fn empty_client_and_lease_lists_share_a_line_with_their_labels() {
    let discovery = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let store = Arc::new(DataStore::new());
    let mut controller = ExternalApiController::new(ctx.clone(), Arc::new(NoControl));
    controller
        .enable(
            tempdir_config(&discovery),
            Arc::clone(&store),
            ingest_sender(),
        )
        .unwrap();
    let snapshot = StoreSnapshot::empty();

    let output = run_frames(&ctx, |ui| {
        let _ = show(&mut controller, ui, &snapshot, &store);
    });

    for (label, value) in [
        ("Clients", "No clients connected"),
        ("Snapshot leases", "No active leases"),
    ] {
        let label = text_rect(&output, label);
        let value = text_rect(&output, value);
        assert!(
            (label.min.y - value.min.y).abs() < 1.0,
            "{label:?} vs {value:?}"
        );
    }
    controller.disable().unwrap();
}

fn register(controller: &ExternalApiController, name: &str) -> serde_json::Value {
    let handle = controller.handle.as_ref().unwrap();
    let response = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{}/v1/clients", handle.endpoint()))
        .bearer_auth(handle.bootstrap_token().expose())
        .body(serde_json::json!({ "name": name }).to_string())
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    serde_json::from_str(&response.text().unwrap()).unwrap()
}

fn click_at(ctx: &egui::Context, pos: egui::Pos2, body: &mut impl FnMut(&mut egui::Ui)) {
    for event in [
        vec![egui::Event::PointerMoved(pos)],
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    ] {
        let input = egui::RawInput {
            events: event,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, &mut *body);
    }
}

fn text_rect(output: &egui::FullOutput, expected: &str) -> egui::Rect {
    output
        .shapes
        .iter()
        .find_map(|shape| find_text_rect(&shape.shape, expected))
        .unwrap_or_else(|| panic!("{expected} was not painted"))
}

#[test]
fn build_config_applies_upload_and_control_limits() {
    let snapshot = snapshot_with_labels(&["flight-a"]);
    let limits = ExternalApiLimits {
        upload_max_mib: 3,
        upload_max_rows: 70,
        upload_max_fields: 5,
        max_concurrent_uploads: 6,
        max_queued_controls: 9,
        max_queued_controls_per_client: 2,
        control_timeout_secs: 11,
        ..ExternalApiLimits::default()
    };

    let config = build_config(&snapshot, limits, None);

    assert_eq!(config.uploads.limits.max_bytes, 3 * 1024 * 1024);
    assert_eq!(config.uploads.limits.max_rows, 70);
    assert_eq!(config.uploads.limits.max_fields, 5);
    assert_eq!(config.uploads.max_concurrent, 6);
    assert_eq!(config.control.max_queued, 9);
    assert_eq!(config.control.max_queued_per_client, 2);
    assert_eq!(config.control.timeout, Duration::from_secs(11));
    assert_eq!(
        ExternalApiLimits::default().upload_max_mib * 1024 * 1024,
        delog_remote::UploadLimits::default().max_bytes
    );
}

#[test]
fn edited_limits_apply_on_the_next_enable_only() {
    let discovery = tempfile::tempdir().unwrap();
    let snapshot = snapshot_with_labels(&["flight-a"]);
    let mut controller = controller();
    let discovery_root = Some(discovery.path().to_owned());
    controller
        .enable(
            build_config(&snapshot, controller.limits, discovery_root.clone()),
            Arc::new(DataStore::new()),
            ingest_sender(),
        )
        .unwrap();
    controller.limits.max_concurrent_uploads = 5;
    assert_eq!(
        controller.running_config().unwrap().uploads.max_concurrent,
        2
    );

    controller.disable().unwrap();
    controller
        .enable(
            build_config(&snapshot, controller.limits, discovery_root),
            Arc::new(DataStore::new()),
            ingest_sender(),
        )
        .unwrap();
    assert_eq!(
        controller.running_config().unwrap().uploads.max_concurrent,
        5
    );
    controller.disable().unwrap();
}

#[test]
fn full_access_requires_confirmation_and_reenable_starts_safe() {
    let discovery = tempfile::tempdir().unwrap();
    let mut controller = controller();
    assert_eq!(controller.request_access_mode(AccessMode::Full), None);
    assert!(!controller.confirming_full);
    controller
        .enable(
            tempdir_config(&discovery),
            Arc::new(DataStore::new()),
            ingest_sender(),
        )
        .unwrap();
    assert_eq!(controller.access_mode(), Some(AccessMode::Safe));

    assert_eq!(controller.request_access_mode(AccessMode::Full), None);
    assert!(controller.confirming_full);
    assert_eq!(controller.access_mode(), Some(AccessMode::Safe));
    controller.cancel_full_access();
    assert_eq!(controller.confirm_full_access(), None);
    assert_eq!(controller.access_mode(), Some(AccessMode::Safe));

    controller.request_access_mode(AccessMode::Full);
    assert_eq!(controller.confirm_full_access(), Some(AccessMode::Full));
    assert_eq!(controller.access_mode(), Some(AccessMode::Full));
    assert_eq!(controller.server_status().unwrap().access, AccessMode::Full);

    controller.disable().unwrap();
    assert_eq!(controller.access_mode(), None);
    controller
        .enable(
            tempdir_config(&discovery),
            Arc::new(DataStore::new()),
            ingest_sender(),
        )
        .unwrap();
    assert_eq!(controller.access_mode(), Some(AccessMode::Safe));
    assert_eq!(
        controller.request_access_mode(AccessMode::Safe),
        Some(AccessMode::Safe)
    );
    controller.disable().unwrap();
}

#[test]
fn owners_survive_disable_and_reenable_for_the_whole_session() {
    let discovery = tempfile::tempdir().unwrap();
    let store = Arc::new(DataStore::new());
    let mut controller = controller();
    controller
        .enable(
            tempdir_config(&discovery),
            Arc::clone(&store),
            ingest_sender(),
        )
        .unwrap();
    let first = register(&controller, "flight-diagnosis");
    controller.disable().unwrap();
    controller
        .enable(
            tempdir_config(&discovery),
            Arc::clone(&store),
            ingest_sender(),
        )
        .unwrap();
    let second = register(&controller, "flight-diagnosis");

    assert_eq!(first["owner_id"], second["owner_id"]);
    assert_ne!(first["client_id"], second["client_id"]);
    controller.disable().unwrap();
}

#[test]
fn clicking_full_asks_for_confirmation_before_granting_it() {
    let discovery = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let store = Arc::new(DataStore::new());
    let mut controller = ExternalApiController::new(ctx.clone(), Arc::new(NoControl));
    controller
        .enable(
            tempdir_config(&discovery),
            Arc::clone(&store),
            ingest_sender(),
        )
        .unwrap();
    let snapshot = StoreSnapshot::empty();
    let mut logs = Vec::new();
    let mut body = |ui: &mut egui::Ui| {
        logs.extend(show(&mut controller, ui, &snapshot, &store));
    };

    let output = run_frames(&ctx, &mut body);
    assert!(!painted(&output, settings_tab::FULL_ACCESS_WARNING));
    click_at(&ctx, text_rect(&output, "Full").center(), &mut body);
    let output = run_frames(&ctx, &mut body);
    assert!(painted(&output, settings_tab::FULL_ACCESS_WARNING));
    assert!(painted(&output, "Allow full control"));

    click_at(
        &ctx,
        text_rect(&output, "Allow full control").center(),
        &mut body,
    );
    let output = run_frames(&ctx, &mut body);
    assert!(!painted(&output, settings_tab::FULL_ACCESS_WARNING));

    assert_eq!(controller.access_mode(), Some(AccessMode::Full));
    assert!(
        logs.iter()
            .any(|(_, message)| message == "External API access set to Full")
    );
    controller.disable().unwrap();
}

#[test]
fn running_window_lists_clients_with_access_counts_and_limits_in_use() {
    let discovery = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let store = Arc::new(DataStore::new());
    let mut controller = ExternalApiController::new(ctx.clone(), Arc::new(NoControl));
    controller
        .enable(
            tempdir_config(&discovery),
            Arc::clone(&store),
            ingest_sender(),
        )
        .unwrap();
    let registered = register(&controller, "flight-diagnosis");
    let snapshot = StoreSnapshot::empty();

    let output = run_frames(&ctx, |ui| {
        let _ = show(&mut controller, ui, &snapshot, &store);
    });

    assert!(painted(&output, "flight-diagnosis"));
    assert!(painted(
        &output,
        "1 client, 0 leases, 0 downloads, 0 uploads, 0 queued controls"
    ));
    assert!(painted(&output, "Concurrent uploads"));
    assert!(painted(&output, "Queued controls per client"));
    assert!(painted(&output, "Control timeout"));
    assert!(painted(&output, settings_tab::LIMITS_NOTE));
    assert!(painted(&output, "in use: 2 s"));
    let client_row = text_rect(&output, "flight-diagnosis");
    let safe_labels: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| find_text_rect(&shape.shape, "Safe"))
        .collect();
    assert!(
        safe_labels
            .iter()
            .any(|rect| (rect.center().y - client_row.center().y).abs() < 4.0)
    );
    assert!(!painted(&output, registered["token"].as_str().unwrap()));
    controller.disable().unwrap();
}

#[test]
fn full_access_keeps_a_warning_visible_until_safe_is_restored() {
    let discovery = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let store = Arc::new(DataStore::new());
    let mut controller = ExternalApiController::new(ctx.clone(), Arc::new(NoControl));
    controller
        .enable(
            tempdir_config(&discovery),
            Arc::clone(&store),
            ingest_sender(),
        )
        .unwrap();
    let snapshot = StoreSnapshot::empty();
    let frame = |controller: &mut ExternalApiController| {
        run_frames(&ctx, |ui| {
            let _ = show(controller, ui, &snapshot, &store);
        })
    };

    assert!(!painted(
        &frame(&mut controller),
        settings_tab::FULL_ACCESS_ACTIVE
    ));
    controller.request_access_mode(AccessMode::Full);
    controller.confirm_full_access();
    assert!(painted(
        &frame(&mut controller),
        settings_tab::FULL_ACCESS_ACTIVE
    ));
    let output = frame(&mut controller);
    assert!(painted(&output, settings_tab::FULL_ACCESS_ACTIVE));
    assert!(!painted(&output, settings_tab::FULL_ACCESS_WARNING));

    controller.request_access_mode(AccessMode::Safe);
    assert!(!painted(
        &frame(&mut controller),
        settings_tab::FULL_ACCESS_ACTIVE
    ));
    controller.disable().unwrap();
}

mod real_app;

#[test]
fn the_guide_limits_table_matches_the_defaults() {
    let guide = include_str!("../../../../../docs/external_python_api.md");
    let table: std::collections::HashMap<&str, &str> = guide
        .split("## Limits")
        .nth(1)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            (cells.len() == 4 && !cells[1].starts_with('-') && cells[1] != "Limit")
                .then(|| (cells[1], cells[2]))
        })
        .collect();
    let limits = ExternalApiLimits::default();
    let expected = [
        (
            "Snapshot idle release",
            format!("{} s", limits.lease_idle_timeout_secs),
        ),
        (
            "Request timeout",
            format!("{} s", limits.request_timeout_secs),
        ),
        (
            "Concurrent data downloads",
            limits.max_concurrent_downloads.to_string(),
        ),
        ("Upload size", format!("{} MiB", limits.upload_max_mib)),
        ("Upload rows", limits.upload_max_rows.to_string()),
        ("Upload fields", limits.upload_max_fields.to_string()),
        (
            "Concurrent uploads",
            limits.max_concurrent_uploads.to_string(),
        ),
        ("Queued controls", limits.max_queued_controls.to_string()),
        (
            "Queued controls per client",
            limits.max_queued_controls_per_client.to_string(),
        ),
        (
            "Control timeout",
            format!("{} s", limits.control_timeout_secs),
        ),
    ];
    assert_eq!(table.len(), expected.len(), "{table:?}");
    for (label, value) in expected {
        assert_eq!(table.get(label).copied(), Some(value.as_str()), "{label}");
    }
    assert!(guide.contains(&format!(
        "after {} minutes without requests",
        match limits.lease_idle_timeout_secs / 60 {
            5 => "five".to_owned(),
            minutes => minutes.to_string(),
        }
    )));
}
