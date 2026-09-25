use std::env;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arrow::array::{ArrayRef, BooleanArray, Int16Array, Int32Array, Int64Array, StringArray};
use arrow::datatypes::DataType;

use delog_core::chunk::Chunk;
use delog_core::identity::IdentityRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_core::store::TopicStore;

use delog_api::control::{
    AuthorizedControlHost, ControlPrincipal, ControlRequest, ControlResponse,
};
use delog_remote::{ControlLimits, RemoteConfig, RemoteServer, RemoteServices};

#[derive(Default)]
struct RecordingControlHost {
    calls: Mutex<Vec<ControlRequest>>,
}

impl AuthorizedControlHost for RecordingControlHost {
    fn call_as(
        &self,
        _principal: ControlPrincipal,
        request: ControlRequest,
    ) -> delog_api::Result<ControlResponse> {
        self.calls
            .lock()
            .expect("recording host poisoned")
            .push(request);
        Ok(ControlResponse::Unit)
    }
}

fn mixed_schema() -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            "mixed",
            [
                FieldSchema::new("count", DataType::Int16, None::<String>, 1.0).unwrap(),
                FieldSchema::new("armed", DataType::Boolean, None::<String>, 1.0).unwrap(),
                FieldSchema::new("label", DataType::Utf8, None::<String>, 1.0).unwrap(),
            ],
        )
        .unwrap(),
    )
}

fn gps_schema() -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            "GPS",
            [FieldSchema::new("Lat", DataType::Int32, Some("deg"), 1e-7).unwrap()],
        )
        .unwrap(),
    )
}

fn mixed_chunk(
    schema: &TopicSchema,
    times: &[i64],
    counts: &[i16],
    armed: &[bool],
    labels: &[&str],
) -> Arc<Chunk> {
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int16Array::from(counts.to_vec())),
        Arc::new(BooleanArray::from(armed.to_vec())),
        Arc::new(StringArray::from(labels.to_vec())),
    ];
    Arc::new(Chunk::try_new(Int64Array::from(times.to_vec()), cols, schema).unwrap())
}

fn gps_chunk(schema: &TopicSchema, time: i64, lat: i32) -> Arc<Chunk> {
    Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![time]),
            vec![Arc::new(Int32Array::from(vec![lat])) as ArrayRef],
            schema,
        )
        .unwrap(),
    )
}

fn build_store() -> Arc<DataStore> {
    let mut identity = IdentityRegistry::new();

    let flight = identity.add_source("flight");
    identity.set_source_offset_us(flight, -500_000).unwrap();
    let mixed = identity.add_topic(flight, "mixed").unwrap();
    for name in ["count", "armed", "label"] {
        identity.add_field(mixed, name).unwrap();
    }
    let flight_gps = identity.add_topic(flight, "GPS").unwrap();
    identity.add_field(flight_gps, "Lat").unwrap();

    let chase = identity.add_source("chase");
    let chase_gps = identity.add_topic(chase, "GPS").unwrap();
    identity.add_field(chase_gps, "Lat").unwrap();

    let mixed_schema = mixed_schema();
    let mixed_store = Arc::new(
        TopicStore::from_chunks(
            Arc::clone(&mixed_schema),
            [
                mixed_chunk(
                    &mixed_schema,
                    &[1_000_000, 1_010_000, 1_020_000],
                    &[10, 11, 12],
                    &[true, false, true],
                    &["a", "b", "c"],
                ),
                mixed_chunk(
                    &mixed_schema,
                    &[1_030_000, 1_040_000],
                    &[13, 14],
                    &[false, true],
                    &["d", "e"],
                ),
            ],
        )
        .unwrap(),
    );

    let flight_gps_schema = gps_schema();
    let flight_gps_store = Arc::new(
        TopicStore::from_chunks(
            Arc::clone(&flight_gps_schema),
            [gps_chunk(&flight_gps_schema, 2_000_000, 555)],
        )
        .unwrap(),
    );

    let chase_gps_schema = gps_schema();
    let chase_gps_store = Arc::new(
        TopicStore::from_chunks(
            Arc::clone(&chase_gps_schema),
            [gps_chunk(&chase_gps_schema, 3_000_000, 777)],
        )
        .unwrap(),
    );

    Arc::new(DataStore::from_snapshot(
        StoreSnapshot::from_registry(
            &identity,
            [
                (mixed, mixed_store),
                (flight_gps, flight_gps_store),
                (chase_gps, chase_gps_store),
            ],
            0,
        )
        .unwrap(),
    ))
}

fn main() {
    let discovery_root: PathBuf = env::var("DELOG_TEST_DISCOVERY_DIR")
        .map(PathBuf::from)
        .expect("DELOG_TEST_DISCOVERY_DIR must be set");

    let config = RemoteConfig {
        label: "fixture".into(),
        loaded_file: None,
        lease_idle_timeout: Duration::from_secs(60),
        request_timeout: Duration::from_secs(5),
        max_concurrent_downloads: 4,
        discovery_root: Some(discovery_root),
        control: ControlLimits::default(),
        uploads: delog_remote::UploadConfig::default(),
    };

    let server = RemoteServer::spawn(
        config,
        RemoteServices {
            store: build_store(),
            ingest: delog_core::ingest::ingest_channel().0,
            control: Arc::new(RecordingControlHost::default()),
            owners: Arc::new(delog_remote::OwnerRegistry::new()),
        },
    )
    .expect("fixture server failed to start");

    println!("READY {}", server.instance_id());
    io::stdout().flush().expect("failed to flush stdout");

    let mut buf = [0u8; 64];
    loop {
        match io::stdin().read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => continue,
        }
    }

    server
        .shutdown()
        .expect("fixture server failed to shut down");
}
