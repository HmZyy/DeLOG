use std::io::Cursor;

use arrow::ipc::reader::StreamReader;
use delog_api::control::{MarkerInfo, MarkerRequest};
use delog_core::ingest::{IngestSink, ParseSummary, ParsedBatch, SourceKind};
use delog_core::schema::{FieldSchema, TopicSchema};

use super::*;

const CHUNKS: usize = 3;
const ROWS: usize = delog_core::ingestor::FILE_CHUNK_ROWS;

impl HeadlessExternalApp {
    fn ingest_flight(&self) {
        let sender = self.session.ingest_sender();
        let mut sink = sender.file_sink();
        let source = sink.open_source("flight", SourceKind::File);
        let schema = Arc::new(
            TopicSchema::new(
                "IMU",
                [
                    FieldSchema::new("x", DataType::Float64, Some("m/s^2"), 1.0).unwrap(),
                    FieldSchema::new("y", DataType::Float64, Some("m/s^2"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        for chunk in 0..CHUNKS {
            let start = chunk * ROWS;
            let rows = start..start + ROWS;
            sink.submit(ParsedBatch::new(
                source,
                Arc::clone(&schema),
                Int64Array::from_iter_values(rows.clone().map(|row| row as i64 * 1_000)),
                vec![
                    Arc::new(Float64Array::from_iter_values(
                        rows.clone().map(|r| r as f64),
                    )) as ArrayRef,
                    Arc::new(Float64Array::from_iter_values(rows.map(|r| -(r as f64)))),
                ],
            ));
        }
        sink.close_source(
            source,
            ParseSummary {
                topic_count: 1,
                row_count: (CHUNKS * ROWS) as u64,
                ..ParseSummary::default()
            },
        );
        drop(sink);
        sender
            .publication_barrier()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let snapshot = self.session.snapshot();
        let imu = snapshot
            .topics
            .iter()
            .find(|topic| topic.entry.name == "IMU")
            .unwrap();
        assert!(snapshot.topic_store(imu.entry.id).unwrap().chunk_count() >= CHUNKS);
    }

    fn markers(&mut self) -> Vec<MarkerInfo> {
        self.trusted(ControlRequest::Markers(MarkerRequest::List))
            .into_markers()
            .unwrap()
    }
}

impl Api {
    fn open_snapshot(&self, client: &Client) -> String {
        let (status, value) = decode(
            self.http
                .post(self.url("/v1/snapshots"))
                .bearer_auth(&client.token)
                .send(),
        );
        assert_eq!(status, 200, "{value}");
        value["lease_id"].as_str().unwrap().into()
    }

    fn catalog(&self, client: &Client, lease: &str) -> Value {
        let (status, value) = decode(
            self.http
                .get(self.url(&format!("/v1/snapshots/{lease}/catalog")))
                .bearer_auth(&client.token)
                .send(),
        );
        assert_eq!(status, 200, "{value}");
        value
    }

    fn data(&self, client: &Client, lease: &str, topic: &str, query: &str) -> Vec<RecordBatch> {
        let response = self
            .http
            .get(self.url(&format!("/v1/snapshots/{lease}/topics/{topic}/data{query}")))
            .bearer_auth(&client.token)
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let bytes = response.bytes().unwrap();
        StreamReader::try_new(Cursor::new(bytes.to_vec()), None)
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn publish_arrow(&self, client: &Client, topic: &str, body: Vec<u8>) -> Value {
        let (status, value) = decode(
            self.http
                .put(self.url(&format!("/v1/publications/{topic}")))
                .bearer_auth(&client.token)
                .header("content-type", "application/vnd.apache.arrow.stream")
                .header("idempotency-key", next_key())
                .body(body)
                .send(),
        );
        assert_eq!(status, 201, "{value}");
        value
    }

    fn disconnect(&self, client: &Client) {
        let (status, value) = decode(
            self.http
                .delete(self.url(&format!("/v1/clients/{}", client.client_id)))
                .bearer_auth(&client.token)
                .send(),
        );
        assert_eq!(status, 200, "{value}");
    }
}

fn rows(batches: &[RecordBatch]) -> usize {
    batches.iter().map(RecordBatch::num_rows).sum()
}

fn column_f64(batches: &[RecordBatch], name: &str) -> Vec<f64> {
    batches
        .iter()
        .flat_map(|batch| {
            batch
                .column_by_name(name)
                .unwrap()
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .values()
                .to_vec()
        })
        .collect()
}

fn column_i64(batches: &[RecordBatch], name: &str) -> Vec<i64> {
    batches
        .iter()
        .flat_map(|batch| {
            batch
                .column_by_name(name)
                .unwrap()
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .values()
                .to_vec()
        })
        .collect()
}

fn derived_arrow(times: &[i64], x: &[f64], y: &[f64]) -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("__delog_time_ns", DataType::Int64, false),
        Field::new("norm", DataType::Float64, false),
        Field::new("lat", DataType::Float64, false),
        Field::new("lon", DataType::Float64, false),
        Field::new("alt", DataType::Float64, false),
    ]));
    let norm = x.iter().zip(y).map(|(x, y)| x.hypot(*y));
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(times.to_vec())),
        Arc::new(Float64Array::from_iter_values(norm)),
        Arc::new(Float64Array::from_iter_values(times.iter().map(|_| 47.0))),
        Arc::new(Float64Array::from_iter_values(times.iter().map(|_| 8.0))),
        Arc::new(Float64Array::from_iter_values(times.iter().map(|_| 400.0))),
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

fn handle_of<'a>(items: &'a Value, key: &str, value: &str) -> &'a Value {
    &items
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item[key] == value)
        .unwrap_or_else(|| panic!("no item with {key} = {value} in {items}"))["handle"]
}

fn owned(owner: &Option<String>) -> bool {
    owner.as_deref() == Some(OWNER)
}

#[test]
fn the_documented_external_workflow_runs_end_to_end_against_real_app_state() {
    let mut app = HeadlessExternalApp::new();
    app.ingest_flight();
    app.control.markers.add_at(500);

    let descriptors: Vec<DiscoveryDescriptor> = std::fs::read_dir(app.discovery.path())
        .unwrap()
        .map(|entry| DiscoveryDescriptor::parse(&std::fs::read(entry.unwrap().path()).unwrap()))
        .collect::<Result<_, _>>()
        .unwrap();
    let ExternalApiStatus::Running(running) = app.controller.status() else {
        panic!("external API is not running")
    };
    let selected = descriptors
        .iter()
        .find(|descriptor| descriptor.instance_id().as_str() == running.instance_id)
        .expect("the running instance is discoverable");
    assert_eq!(selected.endpoint(), running.endpoint);

    let (first_session, catalog, full, subset, subset_columns) = app.serve(|api| {
        let other = api.connect_as("other-script", false);
        api.resource(
            &other,
            json!({"op": "marker_add", "time_ns": 700_000, "label": "other owner"}),
        );

        let client = api.connect(false);
        let lease = api.open_snapshot(&client);
        let catalog = api.catalog(&client, &lease);
        let imu = &catalog["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["label"] == "flight")
            .unwrap()["topics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|topic| topic["name"] == "IMU")
            .unwrap()
            .clone();
        let topic = imu["handle"].as_str().unwrap();
        let full = api.data(&client, &lease, topic, "");
        let x = handle_of(&imu["fields"], "name", "x").as_str().unwrap();
        let subset = api.data(
            &client,
            &lease,
            topic,
            &format!("?fields={x}&start_ns=100000000&end_ns=199999999"),
        );
        let subset_columns: Vec<String> = subset[0]
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().clone())
            .collect();

        let times = column_i64(&full, "__delog_time_ns");
        let derived = api.publish_arrow(
            &client,
            "imu_norm",
            derived_arrow(&times, &column_f64(&full, "x"), &column_f64(&full, "y")),
        );
        let fields = &derived["topic"]["fields"];
        let window = api.resource(&client, json!({"op": "window_open", "title": "IMU review"}));
        let plot = api.resource(
            &client,
            json!({"op": "workspace_add_plot", "window": window, "direction": "vertical"}),
        );
        api.resource(
            &client,
            json!({"op": "trace_add", "plot": plot, "field": handle_of(fields, "name", "norm"), "mode": "line"}),
        );
        api.resource(
            &client,
            json!({"op": "marker_add", "time_ns": 150_000_000, "label": "review"}),
        );
        api.resource(
            &client,
            json!({
                "op": "annotation_add",
                "plot": plot,
                "geometry": {"kind": "text", "at": {"time_ns": 150_000_000, "y": 2.0}},
                "label": "norm peak",
            }),
        );
        api.resource(
            &client,
            json!({
                "op": "vehicle_add",
                "source": derived["handle"],
                "label": "IMU vehicle",
                "show": true,
                "show_path": false,
                "position": {
                    "kind": "gps",
                    "lat": handle_of(fields, "name", "lat"),
                    "lon": handle_of(fields, "name", "lon"),
                    "alt": handle_of(fields, "name", "alt"),
                    "lat_lon_dege7": false,
                    "alt_mm": false,
                    "alt_offset_m": 0.0,
                },
                "orientation": {"kind": "static"},
                "model": "quad",
                "color": "#3366ff",
                "path_color": "#ffffff",
                "scale": 1.0,
            }),
        );
        api.disconnect(&client);
        (client, catalog, full, subset, subset_columns)
    });

    let flight = catalog["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["label"] == "flight")
        .unwrap();
    let imu = &flight["topics"][0];
    assert_eq!(imu["name"], "IMU");
    assert_eq!(imu["row_count"], (CHUNKS * ROWS) as u64);
    let field_names: Vec<&str> = imu["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field["name"].as_str().unwrap())
        .collect();
    assert_eq!(field_names, ["x", "y"]);
    assert_eq!(rows(&full), CHUNKS * ROWS);
    assert!(full.len() >= CHUNKS, "{} batches", full.len());
    assert_eq!(subset_columns, ["__delog_time_ns", "__source_time_ns", "x"]);
    assert_eq!(rows(&subset), 100);
    assert_eq!(column_f64(&subset, "x")[0], 100.0);

    app.published_field("imu_norm", "norm");
    assert!(
        app.windows()
            .iter()
            .any(|w| w.title == "IMU review" && w.owner.as_ref().is_some_and(|o| o.name == OWNER))
    );
    assert!(
        app.plots()
            .iter()
            .any(|p| p.owner.as_ref().is_some_and(|o| o.name == OWNER))
    );
    assert!(
        app.traces()
            .iter()
            .any(|t| t.owner.as_ref().is_some_and(|o| o.name == OWNER))
    );
    assert!(app.annotations().iter().any(|a| owned(&a.owner)));
    assert!(
        app.vehicles()
            .iter()
            .any(|v| v.spec.owner.as_ref().is_some_and(|o| o.name == OWNER))
    );
    assert!(
        app.markers()
            .iter()
            .any(|m| owned(&m.owner) && m.label == "review")
    );

    let (forbidden, removal, _lease_client) = app.serve(move |api| {
        let (status, body) = decode(
            api.http
                .get(api.url("/v1/control/state"))
                .bearer_auth(&first_session.token)
                .send(),
        );
        assert!(matches!(status, 401 | 403), "{status} {body}");
        let client = api.connect(false);
        let state = api.state(&client);
        let owned_marker = handle_of(&state["markers"], "label", "review").clone();
        let (status, body) = api.command(
            &client,
            json!({"op": "marker_set", "marker": owned_marker, "label": "reviewed"}),
        );
        assert_eq!(status, 200, "{body}");
        let manual_marker = state["markers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|marker| marker["owner"].is_null())
            .unwrap()["handle"]
            .clone();
        let forbidden = api.command(
            &client,
            json!({"op": "marker_remove", "marker": manual_marker}),
        );
        let (status, removal) = api.command(&client, json!({"op": "remove_owned"}));
        assert_eq!(status, 200, "{removal}");
        api.open_snapshot(&client);
        (forbidden, removal, client)
    });
    assert_eq!(forbidden.0, 403, "{}", forbidden.1);
    assert_eq!(forbidden.1["code"], "forbidden");
    assert_eq!(removal["publications"], 1, "{removal}");
    assert!(removal["ui_resources"].as_u64().unwrap() > 0, "{removal}");

    let markers = app.markers();
    assert!(markers.iter().any(|m| m.owner.is_none()), "{markers:?}");
    assert!(
        markers
            .iter()
            .any(|m| m.owner.as_deref() == Some("external/other-script")),
        "{markers:?}"
    );
    assert!(!markers.iter().any(|m| owned(&m.owner)), "{markers:?}");
    assert!(!app.windows().iter().any(|w| w.owner.is_some()));
    assert!(app.traces().is_empty() && app.vehicles().is_empty());
    assert!(!app.plots().iter().any(|p| p.owner.is_some()));
    assert!(app.annotations().is_empty());
    assert!(find_published_field(&app.session.snapshot(), "imu_norm", "norm").is_none());

    let status = app.controller.server_status().unwrap();
    assert!(!status.clients.is_empty() && !status.leases.is_empty());
    let staging = app.controller.upload_staging_dir().unwrap();
    assert!(staging.is_dir());
    let api = app.api();
    app.controller.disable().unwrap();
    assert_eq!(std::fs::read_dir(app.discovery.path()).unwrap().count(), 0);
    assert!(!staging.exists());
    assert!(api.http.get(api.url("/v1/instance")).send().is_err());

    app.enable(Duration::from_secs(5));
    let status = app.controller.server_status().unwrap();
    assert!(status.clients.is_empty() && status.leases.is_empty());
    assert_eq!(app.controller.access_mode(), Some(AccessMode::Safe));
    app.controller.disable().unwrap();
}
