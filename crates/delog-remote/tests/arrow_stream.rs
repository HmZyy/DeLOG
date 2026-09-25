use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int16Array, Int32Array, Int64Array,
};
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use arrow_ipc::reader::StreamReader;
use futures_util::StreamExt;

use delog_core::chunk::Chunk;
use delog_core::identity::{IdentityRegistry, TopicId};
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

use delog_remote::{
    ArrowStreamStats, ClientRegistry, CreatedLease, LeaseManager, LeaseReadGuard, OpaqueId,
    ReadQuery, RegisterClientRequest, SecretToken, TopicBatchIter, arrow_body,
    arrow_body_with_stats,
};

struct Fixture {
    leases: LeaseManager,
    lease: CreatedLease,
    guard: LeaseReadGuard,
    topic: TopicId,
    bool_field: OpaqueId,
    i16_field: OpaqueId,
    f64_field: OpaqueId,
    other_topic_field: OpaqueId,
}

impl Fixture {
    fn guard(&self) -> LeaseReadGuard {
        self.leases
            .read(&self.lease.id, &self.lease.client, Instant::now())
            .unwrap()
    }

    fn active_readers(&self) -> usize {
        self.leases
            .list_active()
            .into_iter()
            .find(|status| status.id == self.lease.id)
            .map(|status| status.active_readers)
            .unwrap_or(0)
    }
}

fn imu_schema() -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            "IMU",
            [
                FieldSchema::new("B", DataType::Boolean, None::<String>, 1.0).unwrap(),
                FieldSchema::new("I16", DataType::Int16, Some("count"), 1.0).unwrap(),
                FieldSchema::new("F64", DataType::Float64, Some("m"), 0.25)
                    .unwrap()
                    .with_description("height"),
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

fn imu_chunk(schema: &TopicSchema, times: &[i64]) -> Arc<Chunk> {
    let bools: Vec<Option<bool>> = (0..times.len())
        .map(|i| if i == 1 { None } else { Some(i % 2 == 0) })
        .collect();
    let ints: Vec<i16> = (0..times.len()).map(|i| i as i16).collect();
    let floats: Vec<f64> = times.iter().map(|&t| t as f64).collect();
    let cols: Vec<ArrayRef> = vec![
        Arc::new(BooleanArray::from(bools)),
        Arc::new(Int16Array::from(ints)),
        Arc::new(Float64Array::from(floats)),
    ];
    Arc::new(Chunk::try_new(Int64Array::from(times.to_vec()), cols, schema).unwrap())
}

fn build_fixture(offset_us: i64, chunks: &[Vec<i64>]) -> Fixture {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    identity.set_source_offset_us(source, offset_us).unwrap();
    let imu = identity.add_topic(source, "IMU").unwrap();
    for name in ["B", "I16", "F64"] {
        identity.add_field(imu, name).unwrap();
    }
    let gps = identity.add_topic(source, "GPS").unwrap();
    identity.add_field(gps, "Lat").unwrap();

    let schema = imu_schema();
    let imu_store = Arc::new(
        TopicStore::from_chunks(
            Arc::clone(&schema),
            chunks.iter().map(|times| imu_chunk(&schema, times)),
        )
        .unwrap(),
    );
    let gps_schema = gps_schema();
    let gps_chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![chunks[0][0]]),
            vec![Arc::new(Int32Array::from(vec![7])) as ArrayRef],
            &gps_schema,
        )
        .unwrap(),
    );
    let gps_store = Arc::new(TopicStore::from_chunks(gps_schema, [gps_chunk]).unwrap());
    let snapshot =
        StoreSnapshot::from_registry(&identity, [(imu, imu_store), (gps, gps_store)], 0).unwrap();

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = registry
        .register(
            &bootstrap,
            &bootstrap,
            RegisterClientRequest {
                name: "reader".to_owned(),
                takeover: false,
            },
        )
        .unwrap()
        .client_id;

    let leases = LeaseManager::new(Duration::from_secs(600));
    let lease = leases
        .create(client, Arc::new(snapshot), Instant::now())
        .unwrap();
    let guard = leases
        .read(&lease.id, &lease.client, Instant::now())
        .unwrap();

    let mut handles = HashMap::new();
    for source in &guard.catalog().sources {
        for topic in &source.topics {
            for field in &topic.fields {
                handles.insert(
                    format!("{}.{}", topic.name, field.name),
                    field.handle.clone(),
                );
            }
        }
    }

    Fixture {
        leases,
        lease,
        guard,
        topic: imu,
        bool_field: handles["IMU.B"].clone(),
        i16_field: handles["IMU.I16"].clone(),
        f64_field: handles["IMU.F64"].clone(),
        other_topic_field: handles["GPS.Lat"].clone(),
    }
}

fn topic_fixture_with_offset(offset_us: i64) -> Fixture {
    build_fixture(offset_us, &[vec![1_000, 1_100, 1_200], vec![1_300, 1_400]])
}

fn all_fields() -> ReadQuery {
    ReadQuery {
        fields: None,
        start_ns: None,
        end_ns: None,
    }
}

fn i64_column(batch: &RecordBatch, index: usize) -> Vec<i64> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .values()
        .to_vec()
}

fn column_by_name(batches: &[RecordBatch], name: &str) -> Vec<i64> {
    batches
        .iter()
        .flat_map(|batch| {
            let index = batch.schema().index_of(name).unwrap();
            i64_column(batch, index)
        })
        .collect()
}

#[test]
fn batches_preserve_types_and_include_both_timelines() {
    let fixture = topic_fixture_with_offset(-250);
    let query = ReadQuery {
        fields: Some(vec![fixture.bool_field.clone(), fixture.i16_field.clone()]),
        start_ns: Some(750_000),
        end_ns: Some(900_000),
    };
    let batches = TopicBatchIter::new(fixture.guard, fixture.topic, query)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(batches[0].schema().field(0).name(), "__delog_time_ns");
    assert_eq!(batches[0].schema().field(1).name(), "__source_time_ns");
    assert_eq!(batches[0].schema().field(2).data_type(), &DataType::Boolean);
    assert_eq!(batches[0].schema().field(3).data_type(), &DataType::Int16);

    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_columns(), 4);
    assert_eq!(batches[0].schema().field(0).data_type(), &DataType::Int64);
    assert_eq!(batches[0].schema().field(1).data_type(), &DataType::Int64);
    assert_eq!(i64_column(&batches[0], 0), vec![750_000, 850_000]);
    assert_eq!(i64_column(&batches[0], 1), vec![1_000_000, 1_100_000]);
    let bools = batches[0]
        .column(2)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .unwrap();
    assert!(bools.value(0));
    assert!(bools.is_null(1));
    let ints = batches[0]
        .column(3)
        .as_any()
        .downcast_ref::<Int16Array>()
        .unwrap();
    assert_eq!(ints.values().to_vec(), vec![0, 1]);
}

#[test]
fn absent_fields_select_every_data_field_across_every_chunk() {
    let fixture = topic_fixture_with_offset(0);
    let batches = TopicBatchIter::new(fixture.guard, fixture.topic, all_fields())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let names: Vec<String> = batches[0]
        .schema()
        .fields()
        .iter()
        .map(|field| field.name().clone())
        .collect();
    assert_eq!(
        names,
        ["__delog_time_ns", "__source_time_ns", "B", "I16", "F64"]
    );
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].num_rows(), 3);
    assert_eq!(batches[1].num_rows(), 2);
    assert_eq!(
        column_by_name(&batches, "__delog_time_ns"),
        vec![1_000_000, 1_100_000, 1_200_000, 1_300_000, 1_400_000]
    );
}

#[test]
fn explicit_projection_preserves_the_caller_order() {
    let fixture = topic_fixture_with_offset(0);
    let query = ReadQuery {
        fields: Some(vec![fixture.f64_field.clone(), fixture.bool_field.clone()]),
        start_ns: None,
        end_ns: None,
    };
    let iter = TopicBatchIter::new(fixture.guard, fixture.topic, query).unwrap();
    let schema = iter.schema().clone();
    assert_eq!(schema.field(2).name(), "F64");
    assert_eq!(schema.field(3).name(), "B");
    assert_eq!(schema.fields().len(), 4);
}

#[test]
fn schema_and_field_metadata_describe_the_snapshot_topic() {
    let fixture = topic_fixture_with_offset(0);
    let iter = TopicBatchIter::new(fixture.guard, fixture.topic, all_fields()).unwrap();
    let schema = iter.schema().clone();

    let metadata = schema.metadata();
    assert_eq!(metadata["delog.api_version"], "1.0");
    assert_eq!(metadata["delog.snapshot_epoch"], "0");
    assert_eq!(metadata["delog.source"], "flight");
    assert_eq!(metadata["delog.topic"], "IMU");

    let f64_meta = schema.field_with_name("F64").unwrap().metadata();
    assert_eq!(f64_meta["delog.unit"], "m");
    assert_eq!(f64_meta["delog.description"], "height");
    assert_eq!(f64_meta["delog.multiplier"], "0.25");

    let bool_meta = schema.field_with_name("B").unwrap().metadata();
    assert!(!bool_meta.contains_key("delog.unit"));
    assert!(!bool_meta.contains_key("delog.description"));
    assert_eq!(bool_meta["delog.multiplier"], "1");

    for batch in iter {
        assert_eq!(batch.unwrap().schema(), schema);
    }
}

#[test]
fn a_range_outside_the_data_yields_no_batches_but_keeps_the_schema() {
    let fixture = topic_fixture_with_offset(0);
    let query = ReadQuery {
        fields: None,
        start_ns: Some(10_000_000),
        end_ns: None,
    };
    let iter = TopicBatchIter::new(fixture.guard, fixture.topic, query).unwrap();
    assert_eq!(iter.schema().fields().len(), 5);
    assert_eq!(iter.count(), 0);
}

#[test]
fn a_single_instant_range_is_inclusive() {
    let fixture = topic_fixture_with_offset(0);
    let query = ReadQuery {
        fields: None,
        start_ns: Some(1_300_000),
        end_ns: Some(1_300_000),
    };
    let batches = TopicBatchIter::new(fixture.guard, fixture.topic, query)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(column_by_name(&batches, "__delog_time_ns"), vec![1_300_000]);
}

#[test]
fn unknown_field_handles_are_not_found() {
    let fixture = topic_fixture_with_offset(0);
    let query = ReadQuery {
        fields: Some(vec![OpaqueId::new("missing")]),
        start_ns: None,
        end_ns: None,
    };
    let error = TopicBatchIter::new(fixture.guard, fixture.topic, query).unwrap_err();
    assert_eq!(error.code(), "not_found");
}

#[test]
fn duplicate_field_handles_are_invalid_input() {
    let fixture = topic_fixture_with_offset(0);
    let query = ReadQuery {
        fields: Some(vec![fixture.i16_field.clone(), fixture.i16_field.clone()]),
        start_ns: None,
        end_ns: None,
    };
    let error = TopicBatchIter::new(fixture.guard, fixture.topic, query).unwrap_err();
    assert_eq!(error.code(), "invalid_input");
}

#[test]
fn fields_of_another_topic_are_invalid_input() {
    let fixture = topic_fixture_with_offset(0);
    let query = ReadQuery {
        fields: Some(vec![fixture.other_topic_field.clone()]),
        start_ns: None,
        end_ns: None,
    };
    let error = TopicBatchIter::new(fixture.guard, fixture.topic, query).unwrap_err();
    assert_eq!(error.code(), "invalid_input");
}

#[test]
fn inverted_ranges_are_invalid_input() {
    let fixture = topic_fixture_with_offset(0);
    let query = ReadQuery {
        fields: None,
        start_ns: Some(2_000_000),
        end_ns: Some(1_000_000),
    };
    let error = TopicBatchIter::new(fixture.guard, fixture.topic, query).unwrap_err();
    assert_eq!(error.code(), "invalid_input");
}

#[test]
fn negative_boundaries_filter_across_chunks() {
    let fixture = topic_fixture_with_offset(-2_000);
    let query = ReadQuery {
        fields: Some(vec![fixture.i16_field.clone()]),
        start_ns: Some(-900_000),
        end_ns: Some(-700_000),
    };
    let batches = TopicBatchIter::new(fixture.guard, fixture.topic, query)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(batches.len(), 2);
    assert_eq!(
        column_by_name(&batches, "__delog_time_ns"),
        vec![-900_000, -800_000, -700_000]
    );
    assert_eq!(
        column_by_name(&batches, "__source_time_ns"),
        vec![1_100_000, 1_200_000, 1_300_000]
    );
}

#[test]
fn extreme_bounds_select_everything() {
    let fixture = topic_fixture_with_offset(-5_000);
    let query = ReadQuery {
        fields: None,
        start_ns: Some(i64::MIN),
        end_ns: Some(i64::MAX),
    };
    let batches = TopicBatchIter::new(fixture.guard, fixture.topic, query)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        column_by_name(&batches, "__delog_time_ns"),
        vec![-4_000_000, -3_900_000, -3_800_000, -3_700_000, -3_600_000]
    );
}

#[test]
fn raw_source_time_that_overflows_nanoseconds_is_internal() {
    let raw = i64::MAX / 1000 + 1;
    let fixture = build_fixture(-1_000, &[vec![raw]]);
    let result = TopicBatchIter::new(fixture.guard, fixture.topic, all_fields())
        .unwrap()
        .collect::<Result<Vec<_>, _>>();
    assert_eq!(result.unwrap_err().code(), "internal");
}

#[test]
fn negative_raw_source_time_that_overflows_nanoseconds_is_internal() {
    let raw = i64::MIN / 1000 - 1;
    let fixture = build_fixture(1_000, &[vec![raw]]);
    let result = TopicBatchIter::new(fixture.guard, fixture.topic, all_fields())
        .unwrap()
        .collect::<Result<Vec<_>, _>>();
    assert_eq!(result.unwrap_err().code(), "internal");
}

#[test]
fn cancellation_between_chunks_stops_the_iterator() {
    let fixture = topic_fixture_with_offset(0);
    let mut iter = TopicBatchIter::new(fixture.guard(), fixture.topic, all_fields()).unwrap();

    assert!(iter.next().unwrap().is_ok());
    fixture
        .leases
        .close(&fixture.lease.id, &fixture.lease.client)
        .unwrap();
    assert_eq!(iter.next().unwrap().unwrap_err().code(), "snapshot_expired");
    assert!(iter.next().is_none());
}

fn many_chunk_fixture(chunk_count: i64) -> Fixture {
    let chunks: Vec<Vec<i64>> = (0..chunk_count)
        .map(|c| (0..4).map(|r| c * 100 + r).collect())
        .collect();
    build_fixture(0, &chunks)
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn assert_stays(what: &str, window: Duration, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        assert!(condition(), "{what} changed during the observation window");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn decode(bytes: &[u8]) -> (arrow::datatypes::SchemaRef, Vec<RecordBatch>) {
    let reader = StreamReader::try_new(bytes, None).unwrap();
    let schema = reader.schema();
    let batches = reader.collect::<Result<Vec<_>, _>>().unwrap();
    (schema, batches)
}

const CHANNEL_CAPACITY: u64 = 2;

async fn block_after_first_chunk(
    fixture: &Fixture,
    stats: &Arc<ArrowStreamStats>,
    stream: &mut axum::body::BodyDataStream,
    idle_readers: usize,
) -> Vec<u8> {
    let first = stream.next().await.unwrap().unwrap();
    let blocked_at = 1 + CHANNEL_CAPACITY;
    wait_until("the producer to fill the channel", || {
        stats.chunks_sent() == blocked_at
    })
    .await;
    assert_stays("the blocked producer", Duration::from_millis(150), || {
        stats.chunks_sent() == blocked_at && !stats.is_finished()
    })
    .await;
    assert_eq!(fixture.active_readers(), idle_readers + 1);
    first.to_vec()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_the_lease_stops_a_blocked_producer() {
    let fixture = many_chunk_fixture(64);
    let idle = fixture.active_readers();
    let iter = TopicBatchIter::new(fixture.guard(), fixture.topic, all_fields()).unwrap();
    let (body, stats) = arrow_body_with_stats(iter);
    let mut stream = body.into_data_stream();

    block_after_first_chunk(&fixture, &stats, &mut stream, idle).await;

    fixture
        .leases
        .close(&fixture.lease.id, &fixture.lease.client)
        .unwrap();
    wait_until("the producer to exit", || stats.is_finished()).await;
    assert!(!stats.is_complete());
    assert_eq!(stats.chunks_sent(), 1 + CHANNEL_CAPACITY);

    let mut saw_error = false;
    while let Some(item) = stream.next().await {
        if item.is_err() {
            saw_error = true;
        }
    }
    assert!(saw_error);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_body_releases_the_producer_and_the_lease_guard() {
    let fixture = many_chunk_fixture(64);
    let idle = fixture.active_readers();
    let iter = TopicBatchIter::new(fixture.guard(), fixture.topic, all_fields()).unwrap();
    let (body, stats) = arrow_body_with_stats(iter);
    let mut stream = body.into_data_stream();

    block_after_first_chunk(&fixture, &stats, &mut stream, idle).await;

    drop(stream);
    wait_until("the producer to exit", || stats.is_finished()).await;
    wait_until("the lease guard to drop", || {
        fixture.active_readers() == idle
    })
    .await;
    assert!(!stats.is_complete());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_consumer_decodes_the_same_batches_as_the_iterator() {
    let fixture = many_chunk_fixture(64);
    let idle = fixture.active_readers();
    let query = || ReadQuery {
        fields: Some(vec![fixture.f64_field.clone(), fixture.bool_field.clone()]),
        start_ns: Some(100_000),
        end_ns: None,
    };
    let iter = TopicBatchIter::new(fixture.guard(), fixture.topic, query()).unwrap();
    let (body, stats) = arrow_body_with_stats(iter);
    let mut stream = body.into_data_stream();

    let mut bytes = block_after_first_chunk(&fixture, &stats, &mut stream, idle).await;
    while let Some(item) = stream.next().await {
        bytes.extend_from_slice(&item.unwrap());
    }
    wait_until("the producer to exit", || stats.is_finished()).await;
    assert!(stats.is_complete());

    let direct_iter = TopicBatchIter::new(fixture.guard(), fixture.topic, query()).unwrap();
    let direct_schema = direct_iter.schema().clone();
    let direct = direct_iter.collect::<Result<Vec<_>, _>>().unwrap();
    wait_until("every lease guard to drop", || {
        fixture.active_readers() == idle
    })
    .await;

    let (schema, batches) = decode(&bytes);
    assert_eq!(schema, direct_schema);
    assert_eq!(schema.metadata()["delog.topic"], "IMU");
    assert_eq!(
        schema.field_with_name("F64").unwrap().metadata()["delog.unit"],
        "m"
    );
    assert_eq!(batches, direct);
    assert_eq!(batches.len(), 63);
}

#[tokio::test]
async fn an_empty_range_streams_a_schema_only_ipc_stream() {
    let fixture = topic_fixture_with_offset(0);
    let query = ReadQuery {
        fields: Some(vec![fixture.i16_field.clone()]),
        start_ns: Some(-10_000_000),
        end_ns: Some(-9_000_000),
    };
    let iter = TopicBatchIter::new(fixture.guard(), fixture.topic, query).unwrap();
    let expected = iter.schema().clone();
    let mut stream = arrow_body(iter).into_data_stream();

    let mut bytes = Vec::new();
    while let Some(item) = stream.next().await {
        bytes.extend_from_slice(&item.unwrap());
    }
    let (schema, batches) = decode(&bytes);
    assert_eq!(schema, expected);
    assert!(batches.is_empty());
}
