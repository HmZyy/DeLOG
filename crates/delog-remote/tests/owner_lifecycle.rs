use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use arrow_ipc::writer::StreamWriter;
use delog_api::control::{
    AccessMode, AuthorizedControlHost, ControlPrincipal, ControlRequest, ControlResponse,
    GenerationRequest, LayoutRequest, MarkerRequest, PlaybackInfo, PlaybackRequest, PlotInfo,
    PlotRequest, TraceRequest, WorkspaceInfo, WorkspaceRequest,
};
use delog_core::ingest::{IngestReceiver, IngestSender, ingest_channel};
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::snapshot::DataStore;
use delog_remote::{
    ControlLimits, OwnerRegistry, RemoteConfig, RemoteServer, RemoteServerHandle, RemoteServices,
    UploadConfig,
};
use serde_json::{Value, json};

#[derive(Default)]
struct LifecycleHost {
    plots: Mutex<Vec<PlotInfo>>,
    next_tile: AtomicU64,
    principals: Mutex<Vec<ControlPrincipal>>,
    fail_remove: Mutex<Option<delog_api::Error>>,
}

impl LifecycleHost {
    fn last_access(&self) -> AccessMode {
        self.principals.lock().unwrap().last().unwrap().access
    }
}

impl AuthorizedControlHost for LifecycleHost {
    fn call_as(
        &self,
        principal: ControlPrincipal,
        request: ControlRequest,
    ) -> delog_api::Result<ControlResponse> {
        self.principals.lock().unwrap().push(principal.clone());
        Ok(match request {
            ControlRequest::Workspace(WorkspaceRequest::AddPlot { .. }) => {
                let tile = self.next_tile.fetch_add(1, Ordering::Relaxed) + 10;
                let info = PlotInfo {
                    window: 0,
                    tile,
                    instance_id: tile,
                    index: 0,
                    label: "plot".into(),
                    owner: Some(principal.owner),
                };
                self.plots.lock().unwrap().push(info.clone());
                ControlResponse::Plots(vec![info])
            }
            ControlRequest::Generation(GenerationRequest::RemoveOwned { owner }) => {
                if let Some(error) = self.fail_remove.lock().unwrap().take() {
                    return Err(error);
                }
                let mut plots = self.plots.lock().unwrap();
                let before = plots.len();
                plots.retain(|plot| plot.owner.as_ref().is_none_or(|item| item.name != owner));
                ControlResponse::Removed(before - plots.len())
            }
            ControlRequest::Workspace(WorkspaceRequest::ListWindows) => {
                ControlResponse::Windows(vec![])
            }
            ControlRequest::Workspace(WorkspaceRequest::GetState) => {
                ControlResponse::Workspace(WorkspaceInfo {
                    scene_visible: true,
                })
            }
            ControlRequest::Playback(PlaybackRequest::Get) => {
                ControlResponse::Playback(PlaybackInfo {
                    speed: 1.0,
                    follow_live: false,
                })
            }
            ControlRequest::Plots(PlotRequest::List { .. }) => {
                ControlResponse::Plots(self.plots.lock().unwrap().clone())
            }
            ControlRequest::Traces(TraceRequest::List { .. }) => ControlResponse::Traces(vec![]),
            ControlRequest::Annotations(_) => ControlResponse::Annotations(vec![]),
            ControlRequest::Markers(MarkerRequest::List) => ControlResponse::Markers(vec![]),
            ControlRequest::Vehicles(_) => ControlResponse::Vehicles(vec![]),
            ControlRequest::Layouts(LayoutRequest::List) => ControlResponse::Names(vec![]),
            ControlRequest::Layouts(LayoutRequest::Current) => ControlResponse::Layout("{}".into()),
            _ => ControlResponse::Unit,
        })
    }
}

struct Client {
    name: String,
    client_id: String,
    owner_id: String,
    token: String,
}

struct Publication {
    owner: String,
    topic: String,
    generation: u64,
}

struct LifecycleFixture {
    root: tempfile::TempDir,
    store: Arc<DataStore>,
    sender: Option<IngestSender>,
    ingest: Option<thread::JoinHandle<()>>,
    host: Arc<LifecycleHost>,
    owners: Arc<OwnerRegistry>,
    server: Option<RemoteServerHandle>,
    http: reqwest::blocking::Client,
    held: Option<(Ingestor<NullObserver>, IngestReceiver)>,
    request_timeout: Duration,
}

static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

fn next_key() -> String {
    format!("lifecycle-{}", NEXT_KEY.fetch_add(1, Ordering::Relaxed))
}

fn arrow_body(value: f64) -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("__delog_time_ns", DataType::Int64, false),
        Field::new("error", DataType::Float64, false),
    ]));
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(vec![1_000, 2_000])),
        Arc::new(Float64Array::from(vec![value, value + 1.0])),
    ];
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).unwrap();
    let mut bytes = Vec::new();
    {
        let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
        writer.write(&batch).unwrap();
        writer.finish().unwrap();
    }
    bytes
}

struct StallingBody {
    head: Vec<u8>,
    release: mpsc::Receiver<()>,
}

impl Read for StallingBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.head.is_empty() {
            let count = self.head.len().min(buf.len());
            buf[..count].copy_from_slice(&self.head[..count]);
            self.head.drain(..count);
            return Ok(count);
        }
        let _ = self.release.recv();
        Err(std::io::Error::other("upload aborted by the test"))
    }
}

impl LifecycleFixture {
    fn new() -> Self {
        let mut fixture = Self::with_held_writer(Duration::from_secs(4));
        fixture.start_writer();
        fixture
    }

    fn with_held_writer(request_timeout: Duration) -> Self {
        let ingestor = Ingestor::new(NullObserver);
        let store = ingestor.store();
        let (sender, receiver) = ingest_channel();
        let mut fixture = Self {
            root: tempfile::tempdir().unwrap(),
            store,
            sender: Some(sender),
            ingest: None,
            held: Some((ingestor, receiver)),
            request_timeout,
            host: Arc::new(LifecycleHost::default()),
            owners: Arc::new(OwnerRegistry::new()),
            server: None,
            http: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        fixture.enable();
        fixture
    }

    fn start_writer(&mut self) {
        let (ingestor, receiver) = self.held.take().unwrap();
        self.ingest = Some(thread::spawn(move || ingestor.run(receiver)));
    }

    fn spawn_put(&self, client: &Client, topic: &str) -> thread::JoinHandle<(u16, Value)> {
        let request = self
            .http
            .put(self.url(&format!("/v1/publications/{topic}")))
            .bearer_auth(&client.token)
            .header("content-type", "application/vnd.apache.arrow.stream")
            .header("idempotency-key", next_key())
            .body(arrow_body(1.0));
        thread::spawn(move || {
            let response = request.send().unwrap();
            let status = response.status().as_u16();
            (
                status,
                serde_json::from_str(&response.text().unwrap()).unwrap(),
            )
        })
    }

    fn topic_visible(&self, topic: &str) -> bool {
        self.store.load().sources.iter().any(|source| {
            !source.entry.removed
                && source
                    .entry
                    .derived_provenance
                    .as_ref()
                    .is_some_and(|provenance| provenance.logical_topic == topic)
        })
    }

    fn enable(&mut self) {
        let config = RemoteConfig {
            label: "lifecycle".into(),
            loaded_file: None,
            lease_idle_timeout: Duration::from_secs(60),
            request_timeout: self.request_timeout,
            max_concurrent_downloads: 2,
            discovery_root: Some(self.root.path().to_owned()),
            control: ControlLimits::default(),
            uploads: UploadConfig::default(),
        };
        let services = RemoteServices {
            store: Arc::clone(&self.store),
            ingest: self.sender.clone().unwrap(),
            control: self.host.clone(),
            owners: Arc::clone(&self.owners),
        };
        self.server = Some(RemoteServer::spawn(config, services).unwrap());
    }

    fn disable(&mut self) {
        self.server.take().unwrap().shutdown().unwrap();
    }

    fn restart(&mut self) {
        self.disable();
        self.enable();
    }

    fn server(&self) -> &RemoteServerHandle {
        self.server.as_ref().unwrap()
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server().endpoint())
    }

    fn try_connect(&self, name: &str, takeover: bool) -> (u16, Value) {
        let response = self
            .http
            .post(self.url("/v1/clients"))
            .bearer_auth(self.server().bootstrap_token().expose())
            .body(json!({"name": name, "takeover": takeover}).to_string())
            .send()
            .unwrap();
        let status = response.status().as_u16();
        (
            status,
            serde_json::from_str(&response.text().unwrap()).unwrap(),
        )
    }

    fn connect(&self, name: &str, takeover: bool) -> Client {
        let (status, value) = self.try_connect(name, takeover);
        assert_eq!(status, 200, "{value}");
        Client {
            name: name.into(),
            client_id: value["client_id"].as_str().unwrap().into(),
            owner_id: value["owner_id"].as_str().unwrap().into(),
            token: value["token"].as_str().unwrap().into(),
        }
    }

    fn disconnect(&self, client: Client) {
        let response = self
            .http
            .delete(self.url(&format!("/v1/clients/{}", client.client_id)))
            .bearer_auth(&client.token)
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
    }

    fn put(&self, client: &Client, topic: &str, replace: bool) -> (u16, Value) {
        let response = self
            .http
            .put(self.url(&format!("/v1/publications/{topic}?replace={replace}")))
            .bearer_auth(&client.token)
            .header("content-type", "application/vnd.apache.arrow.stream")
            .header("idempotency-key", next_key())
            .body(arrow_body(1.0))
            .send()
            .unwrap();
        let status = response.status().as_u16();
        (
            status,
            serde_json::from_str(&response.text().unwrap()).unwrap(),
        )
    }

    fn publish(&self, client: &Client, topic: &str) -> Publication {
        let (status, value) = self.put(client, topic, false);
        assert_eq!(status, 201, "{value}");
        Publication {
            owner: format!("external/{}", client.name),
            topic: topic.into(),
            generation: value["generation"].as_u64().unwrap(),
        }
    }

    fn control(&self, client: &Client, key: Option<&str>, body: Value) -> (u16, Value) {
        let mut request = self
            .http
            .post(self.url("/v1/control"))
            .bearer_auth(&client.token)
            .header("content-type", "application/json");
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let response = request.body(body.to_string()).send().unwrap();
        let status = response.status().as_u16();
        (
            status,
            serde_json::from_str(&response.text().unwrap()).unwrap(),
        )
    }

    fn open_plot(&self, client: &Client) -> u64 {
        let before: Vec<u64> = self.plot_tiles();
        let (status, value) = self.control(
            client,
            Some(&next_key()),
            json!({"op": "workspace_add_plot", "direction": "horizontal"}),
        );
        assert_eq!(status, 200, "{value}");
        self.plot_tiles()
            .into_iter()
            .find(|tile| !before.contains(tile))
            .unwrap()
    }

    fn plot_tiles(&self) -> Vec<u64> {
        self.host
            .plots
            .lock()
            .unwrap()
            .iter()
            .map(|plot| plot.tile)
            .collect()
    }

    fn remove_owned_with_key(&self, client: &Client, key: &str) -> (u16, Value) {
        self.control(client, Some(key), json!({"op": "remove_owned"}))
    }

    fn remove_owned(&self, client: &Client) -> Result<Value, Value> {
        let (status, value) = self.remove_owned_with_key(client, &next_key());
        if status == 200 { Ok(value) } else { Err(value) }
    }

    fn publication_visible(&self, publication: &Publication) -> bool {
        self.store.load().sources.iter().any(|source| {
            !source.entry.removed
                && source
                    .entry
                    .derived_provenance
                    .as_ref()
                    .is_some_and(|provenance| {
                        provenance.owner == publication.owner
                            && provenance.logical_topic == publication.topic
                    })
        })
    }

    fn plot_visible(&self, tile: &u64) -> bool {
        self.plot_tiles().contains(tile)
    }

    fn instance_status(&self, client: &Client) -> u16 {
        self.http
            .get(self.url("/v1/instance"))
            .bearer_auth(&client.token)
            .send()
            .unwrap()
            .status()
            .as_u16()
    }

    fn wait_until(&self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !condition(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn start_stalled_upload(
        &self,
        client: &Client,
        topic: &str,
    ) -> (mpsc::Sender<()>, thread::JoinHandle<Option<u16>>) {
        let (release, stalled) = mpsc::channel();
        let body = StallingBody {
            head: arrow_body(1.0)[..32].to_vec(),
            release: stalled,
        };
        let request = self
            .http
            .put(self.url(&format!("/v1/publications/{topic}")))
            .bearer_auth(&client.token)
            .header("content-type", "application/vnd.apache.arrow.stream")
            .header("idempotency-key", next_key())
            .body(reqwest::blocking::Body::new(body));
        let upload = thread::spawn(move || {
            request
                .send()
                .ok()
                .map(|response| response.status().as_u16())
        });
        self.wait_until("the upload to start", |fixture| {
            fixture.server().status().active_uploads == 1
        });
        (release, upload)
    }

    fn live_derived_sources(&self) -> usize {
        self.store
            .load()
            .sources
            .iter()
            .filter(|source| !source.entry.removed && source.entry.derived_provenance.is_some())
            .count()
    }
}

impl Drop for LifecycleFixture {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            let _ = server.shutdown();
        }
        self.sender.take();
        if self.held.is_some() {
            self.start_writer();
        }
        if let Some(ingest) = self.ingest.take() {
            let _ = ingest.join();
        }
    }
}

#[test]
fn reconnecting_same_name_reclaims_committed_resources() {
    let fixture = LifecycleFixture::new();
    let first = fixture.connect("flight-diagnosis", false);
    let publication = fixture.publish(&first, "error");
    let plot = fixture.open_plot(&first);
    let owner = first.owner_id.clone();
    fixture.disconnect(first);

    assert!(fixture.publication_visible(&publication));
    assert!(fixture.plot_visible(&plot));

    let second = fixture.connect("flight-diagnosis", false);
    assert_eq!(second.owner_id, owner);
    let report = fixture.remove_owned(&second).unwrap();
    assert_eq!(
        report,
        json!({"kind": "removed", "ui_resources": 1, "publications": 1})
    );
    assert!(!fixture.publication_visible(&publication));
    assert!(!fixture.plot_visible(&plot));
}

#[test]
fn remove_owned_leaves_another_owners_resources_untouched() {
    let fixture = LifecycleFixture::new();
    let first = fixture.connect("flight-diagnosis", false);
    let other = fixture.connect("other-analysis", false);
    let mine = fixture.publish(&first, "error");
    let theirs = fixture.publish(&other, "error");
    let my_plot = fixture.open_plot(&first);
    let their_plot = fixture.open_plot(&other);

    fixture.remove_owned(&first).unwrap();

    assert!(!fixture.publication_visible(&mine));
    assert!(!fixture.plot_visible(&my_plot));
    assert!(fixture.publication_visible(&theirs));
    assert!(fixture.plot_visible(&their_plot));
    assert_eq!(fixture.instance_status(&other), 200);
}

#[test]
fn same_name_cannot_connect_while_the_owner_is_active() {
    let fixture = LifecycleFixture::new();
    let first = fixture.connect("flight-diagnosis", false);

    let (status, error) = fixture.try_connect("flight-diagnosis", false);

    assert_eq!(status, 409);
    assert_eq!(error["code"], "conflict");
    assert_eq!(fixture.instance_status(&first), 200);
    assert_eq!(fixture.server().status().clients.len(), 1);
}

#[test]
fn takeover_revokes_the_previous_token_and_its_leases() {
    let fixture = LifecycleFixture::new();
    let first = fixture.connect("flight-diagnosis", false);
    let lease = fixture
        .http
        .post(fixture.url("/v1/snapshots"))
        .bearer_auth(&first.token)
        .send()
        .unwrap();
    assert_eq!(lease.status().as_u16(), 200);
    assert_eq!(fixture.server().status().leases.len(), 1);

    let second = fixture.connect("flight-diagnosis", true);

    assert_eq!(second.owner_id, first.owner_id);
    assert_ne!(second.client_id, first.client_id);
    assert_eq!(fixture.instance_status(&first), 403);
    assert_eq!(fixture.instance_status(&second), 200);
    let status = fixture.server().status();
    assert!(status.leases.is_empty());
    assert_eq!(status.clients.len(), 1);
    assert_eq!(status.clients[0].client_id.as_str(), second.client_id);
}

#[test]
fn disable_and_reenable_preserve_committed_state_for_the_same_owner() {
    let mut fixture = LifecycleFixture::new();
    let first = fixture.connect("flight-diagnosis", false);
    let publication = fixture.publish(&first, "error");
    let plot = fixture.open_plot(&first);
    let owner = first.owner_id.clone();

    fixture.restart();

    assert!(fixture.publication_visible(&publication));
    assert!(fixture.plot_visible(&plot));
    assert_eq!(fixture.instance_status(&first), 403);
    let second = fixture.connect("flight-diagnosis", false);
    assert_eq!(second.owner_id, owner);

    let state = fixture
        .http
        .get(fixture.url("/v1/control/state"))
        .bearer_auth(&second.token)
        .send()
        .unwrap();
    let state: Value = serde_json::from_str(&state.text().unwrap()).unwrap();
    assert_eq!(state["plots"][0]["owner"], "external/flight-diagnosis");

    let (status, error) = fixture.put(&second, "error", false);
    assert_eq!(status, 409, "{error}");
    assert_eq!(error["code"], "conflict");
    let (status, replaced) = fixture.put(&second, "error", true);
    assert_eq!(status, 200, "{replaced}");
    assert_eq!(replaced["generation"], publication.generation + 1);
    assert_eq!(fixture.live_derived_sources(), 1);

    let report = fixture.remove_owned(&second).unwrap();
    assert_eq!(report["ui_resources"], 1);
    assert_eq!(report["publications"], 1);
    assert!(!fixture.publication_visible(&publication));
    assert!(!fixture.plot_visible(&plot));
}

#[test]
fn another_owner_cannot_reclaim_publications_after_reenable() {
    let mut fixture = LifecycleFixture::new();
    let first = fixture.connect("flight-diagnosis", false);
    let publication = fixture.publish(&first, "error");

    fixture.restart();

    let other = fixture.connect("other-analysis", false);
    let report = fixture.remove_owned(&other).unwrap();
    assert_eq!(report["publications"], 0);
    assert!(fixture.publication_visible(&publication));
}

#[test]
fn full_mode_applies_to_connected_clients_and_resets_after_restart() {
    let mut fixture = LifecycleFixture::new();
    assert_eq!(fixture.server().access_mode(), AccessMode::Safe);
    let client = fixture.connect("flight-diagnosis", false);
    fixture.open_plot(&client);
    assert_eq!(fixture.host.last_access(), AccessMode::Safe);

    fixture.server().set_access_mode(AccessMode::Full);
    assert_eq!(fixture.server().status().access, AccessMode::Full);
    fixture.open_plot(&client);
    assert_eq!(fixture.host.last_access(), AccessMode::Full);

    fixture.restart();

    assert_eq!(fixture.server().access_mode(), AccessMode::Safe);
    assert_eq!(fixture.server().status().access, AccessMode::Safe);
    let client = fixture.connect("flight-diagnosis", false);
    fixture.open_plot(&client);
    assert_eq!(fixture.host.last_access(), AccessMode::Safe);
}

#[test]
fn revoking_a_client_discards_its_uncommitted_upload() {
    let fixture = LifecycleFixture::new();
    let client = fixture.connect("flight-diagnosis", false);
    let (release, upload) = fixture.start_stalled_upload(&client, "error");

    let id = fixture.server().status().clients[0].client_id.clone();
    assert!(fixture.server().revoke_client(&id));

    fixture.wait_until("the upload to be released", |fixture| {
        fixture.server().status().active_uploads == 0
    });
    let _ = release.send(());
    let status = upload.join().unwrap();
    assert_ne!(status, Some(201));
    assert_eq!(fixture.live_derived_sources(), 0);

    let again = fixture.connect("flight-diagnosis", false);
    fixture.publish(&again, "error");
    assert_eq!(fixture.live_derived_sources(), 1);
}

#[test]
fn disabling_the_server_discards_uncommitted_uploads_and_keeps_committed_ones() {
    let mut fixture = LifecycleFixture::new();
    let client = fixture.connect("flight-diagnosis", false);
    let committed = fixture.publish(&client, "committed");
    let (release, upload) = fixture.start_stalled_upload(&client, "pending");

    fixture.disable();
    let _ = release.send(());
    let status = upload.join().unwrap();

    assert_ne!(status, Some(201));
    assert!(fixture.publication_visible(&committed));
    assert_eq!(fixture.live_derived_sources(), 1);
}

#[test]
fn partial_remove_owned_keeps_its_record_and_a_retry_finishes_cleanup() {
    let fixture = LifecycleFixture::new();
    let client = fixture.connect("flight-diagnosis", false);
    let publication = fixture.publish(&client, "error");
    let plot = fixture.open_plot(&client);
    *fixture.host.fail_remove.lock().unwrap() = Some(
        delog_api::Error::unavailable("the DeLOG window did not respond")
            .with_completion(delog_api::MutationCompletion::NotStarted),
    );

    let (status, error) = fixture.remove_owned_with_key(&client, "first-cleanup");

    assert_eq!(status, 503, "{error}");
    assert_eq!(error["code"], "unavailable");
    assert_eq!(error["completion"], "committed");
    assert_eq!(error["details"]["publications"], 1);
    assert_eq!(error["details"]["ui_resources"], Value::Null);
    assert_eq!(error["details"]["failed"], json!(["ui"]));
    assert!(!fixture.publication_visible(&publication));
    assert!(fixture.plot_visible(&plot));

    let record = fixture
        .http
        .get(fixture.url("/v1/requests/by-key"))
        .bearer_auth(&client.token)
        .header("idempotency-key", "first-cleanup")
        .send()
        .unwrap();
    assert_eq!(record.status().as_u16(), 200);
    let record: Value = serde_json::from_str(&record.text().unwrap()).unwrap();
    assert_eq!(record["state"], "completed");

    let (status, replay) = fixture.remove_owned_with_key(&client, "first-cleanup");
    assert_eq!(status, 503);
    assert_eq!(replay, error);

    let report = fixture.remove_owned(&client).unwrap();
    assert_eq!(
        report,
        json!({"kind": "removed", "ui_resources": 1, "publications": 0})
    );
    assert!(!fixture.plot_visible(&plot));
}

#[test]
fn remove_owned_cannot_be_batched_because_it_returns_a_report() {
    let fixture = LifecycleFixture::new();
    let client = fixture.connect("flight-diagnosis", false);
    let response = fixture
        .http
        .post(fixture.url("/v1/control/batch"))
        .bearer_auth(&client.token)
        .header("idempotency-key", next_key())
        .body(json!({"commands": [{"op": "remove_owned"}]}).to_string())
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
}

#[test]
fn status_reports_operational_counts_without_secrets() {
    let fixture = LifecycleFixture::new();
    let client = fixture.connect("flight-diagnosis", false);
    let status = fixture.server().status();
    assert_eq!(status.clients.len(), 1);
    assert_eq!(status.active_uploads, 0);
    assert_eq!(status.active_downloads, 0);
    assert_eq!(status.queued_controls, 0);
    assert_eq!(status.access, AccessMode::Safe);
    let debug = format!("{status:?}");
    assert!(!debug.contains(&client.token));
    assert_eq!(fixture.server().config().uploads, UploadConfig::default());
}

#[test]
fn revoking_after_staging_prevents_a_staged_upload_from_committing() {
    let mut fixture = LifecycleFixture::with_held_writer(Duration::from_millis(1200));
    let client = fixture.connect("flight-diagnosis", false);
    let first = fixture.spawn_put(&client, "first");
    fixture.wait_until("the first upload to reach its commit", |fixture| {
        fixture.server().status().active_uploads == 1
    });
    thread::sleep(Duration::from_millis(50));
    let second = fixture.spawn_put(&client, "second");
    fixture.wait_until("the second upload to finish staging", |fixture| {
        fixture.server().status().active_uploads == 2
    });
    thread::sleep(Duration::from_millis(100));

    let id = fixture.server().status().clients[0].client_id.clone();
    assert!(fixture.server().revoke_client(&id));
    let (_, first) = first.join().unwrap();
    let (status, second) = second.join().unwrap();

    assert_eq!(first["completion"], "unknown", "{first}");
    assert_eq!(status, 503, "{second}");
    assert_eq!(second["completion"], "not_started");
    fixture.start_writer();
    fixture.wait_until("the first commit to land", |fixture| {
        fixture.topic_visible("first")
    });
    thread::sleep(Duration::from_millis(200));
    assert!(!fixture.topic_visible("second"));
}
