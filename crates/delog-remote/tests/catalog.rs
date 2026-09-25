use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array,
    Int64Array, LargeStringArray, StringArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::DataType;

use delog_core::chunk::Chunk;
use delog_core::identity::IdentityRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

use delog_remote::catalog;
use delog_remote::{FieldTypeDto, SourceKindDto};

fn gps_schema() -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            "GPS",
            [FieldSchema::new("Lat", DataType::Int32, Some("deg"), 1e-7)
                .unwrap()
                .with_description("latitude")],
        )
        .unwrap(),
    )
}

fn telemetry_gps_schema() -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            "GPS",
            [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
        )
        .unwrap(),
    )
}

fn all_types_schema() -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            "IMU[0]",
            [
                FieldSchema::new("I8", DataType::Int8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("I16", DataType::Int16, None::<String>, 1.0).unwrap(),
                FieldSchema::new("I32", DataType::Int32, None::<String>, 1.0).unwrap(),
                FieldSchema::new("I64", DataType::Int64, None::<String>, 1.0).unwrap(),
                FieldSchema::new("U8", DataType::UInt8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("U16", DataType::UInt16, None::<String>, 1.0).unwrap(),
                FieldSchema::new("U32", DataType::UInt32, None::<String>, 1.0).unwrap(),
                FieldSchema::new("U64", DataType::UInt64, None::<String>, 1.0).unwrap(),
                FieldSchema::new("F32", DataType::Float32, Some("m/s"), 0.5).unwrap(),
                FieldSchema::new("F64", DataType::Float64, Some("m"), 2.0).unwrap(),
                FieldSchema::new("B", DataType::Boolean, None::<String>, 1.0).unwrap(),
                FieldSchema::new("S", DataType::Utf8, None::<String>, 1.0).unwrap(),
                FieldSchema::new("LS", DataType::LargeUtf8, None::<String>, 1.0).unwrap(),
            ],
        )
        .unwrap(),
    )
}

fn all_types_chunk(schema: &TopicSchema) -> Arc<Chunk> {
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int8Array::from(vec![1, 2])),
        Arc::new(Int16Array::from(vec![1, 2])),
        Arc::new(Int32Array::from(vec![1, 2])),
        Arc::new(Int64Array::from(vec![1, 2])),
        Arc::new(UInt8Array::from(vec![1, 2])),
        Arc::new(UInt16Array::from(vec![1, 2])),
        Arc::new(UInt32Array::from(vec![1, 2])),
        Arc::new(UInt64Array::from(vec![1, 2])),
        Arc::new(Float32Array::from(vec![1.0, 2.0])),
        Arc::new(Float64Array::from(vec![1.0, 2.0])),
        Arc::new(BooleanArray::from(vec![true, false])),
        Arc::new(StringArray::from(vec!["a", "b"])),
        Arc::new(LargeStringArray::from(vec!["a", "b"])),
    ];
    Arc::new(Chunk::try_new(Int64Array::from(vec![500_000, 600_000]), cols, schema).unwrap())
}

fn build_snapshot() -> StoreSnapshot {
    let mut identity = IdentityRegistry::new();

    let flight = identity.add_source("flight");
    identity.set_source_offset_us(flight, -1_000_000).unwrap();
    let flight_gps = identity.add_topic(flight, "GPS").unwrap();
    identity.add_field(flight_gps, "Lat").unwrap();
    let imu = identity.add_topic_instance(flight, "IMU", 0).unwrap();
    for name in [
        "I8", "I16", "I32", "I64", "U8", "U16", "U32", "U64", "F32", "F64", "B", "S", "LS",
    ] {
        identity.add_field(imu, name).unwrap();
    }

    let telemetry = identity.add_source("telemetry");
    let telemetry_gps = identity.add_topic(telemetry, "GPS").unwrap();
    identity.add_field(telemetry_gps, "Alt").unwrap();

    let ghost = identity.add_source("ghost");
    let ghost_topic = identity.add_topic(ghost, "DEAD").unwrap();
    identity.add_field(ghost_topic, "Nope").unwrap();
    identity.remove_source(ghost).unwrap();

    let flight_gps_schema = gps_schema();
    let flight_gps_chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![1_000_000, 2_000_000]),
            vec![Arc::new(Int32Array::from(vec![100, 200])) as ArrayRef],
            &flight_gps_schema,
        )
        .unwrap(),
    );
    let flight_gps_store =
        Arc::new(TopicStore::from_chunks(flight_gps_schema, [flight_gps_chunk]).unwrap());

    let imu_schema = all_types_schema();
    let imu_chunk = all_types_chunk(&imu_schema);
    let imu_store = Arc::new(TopicStore::from_chunks(imu_schema, [imu_chunk]).unwrap());

    let telemetry_gps_schema = telemetry_gps_schema();
    let telemetry_gps_chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![10_000, 20_000]),
            vec![Arc::new(Float64Array::from(vec![1.5, 2.5])) as ArrayRef],
            &telemetry_gps_schema,
        )
        .unwrap(),
    );
    let telemetry_gps_store =
        Arc::new(TopicStore::from_chunks(telemetry_gps_schema, [telemetry_gps_chunk]).unwrap());

    StoreSnapshot::from_registry(
        &identity,
        [
            (flight_gps, flight_gps_store),
            (imu, imu_store),
            (telemetry_gps, telemetry_gps_store),
        ],
        42,
    )
    .unwrap()
}

#[test]
fn catalog_lists_every_live_entity_exactly_once_with_correct_metadata() {
    let snapshot = build_snapshot();
    let (dto, handles) = catalog::build(&snapshot).unwrap();

    assert_eq!(dto.snapshot_epoch, 42);
    assert_eq!(dto.sources.len(), 2);
    assert!(dto.sources.iter().all(|s| s.label != "ghost"));

    let flight = dto.sources.iter().find(|s| s.label == "flight").unwrap();
    assert_eq!(flight.kind, SourceKindDto::File);
    assert_eq!(flight.offset_ns, -1_000_000_000);
    assert_eq!(flight.topics.len(), 2);
    assert!(flight.topics.iter().all(|t| t.name != "DEAD"));

    let flight_gps = flight.topics.iter().find(|t| t.name == "GPS").unwrap();
    assert_eq!(flight_gps.base_name, "GPS");
    assert_eq!(flight_gps.instance, None);
    assert_eq!(flight_gps.row_count, 2);
    let range = flight_gps.time_range_ns.unwrap();
    assert_eq!(range.start_ns, 0);
    assert_eq!(range.end_ns, 1_000_000_000);
    assert_eq!(flight_gps.fields.len(), 1);
    let lat = &flight_gps.fields[0];
    assert_eq!(lat.name, "Lat");
    assert_eq!(lat.arrow_type, FieldTypeDto::Int32);
    assert_eq!(lat.unit.as_deref(), Some("deg"));
    assert_eq!(lat.description.as_deref(), Some("latitude"));
    assert_eq!(lat.multiplier, 1e-7);
    assert_eq!(lat.selector.source, "flight");
    assert_eq!(lat.selector.topic, "GPS");
    assert_eq!(lat.selector.instance, None);
    assert_eq!(lat.selector.field, "Lat");

    let imu = flight.topics.iter().find(|t| t.name == "IMU[0]").unwrap();
    assert_eq!(imu.base_name, "IMU");
    assert_eq!(imu.instance, Some(0));
    assert_eq!(imu.row_count, 2);
    let imu_range = imu.time_range_ns.unwrap();
    assert_eq!(imu_range.start_ns, -500_000_000);
    assert_eq!(imu_range.end_ns, -400_000_000);
    assert_eq!(imu.fields.len(), 13);

    let by_name = |name: &str| imu.fields.iter().find(|f| f.name == name).unwrap();
    assert_eq!(by_name("I8").arrow_type, FieldTypeDto::Int8);
    assert_eq!(by_name("I16").arrow_type, FieldTypeDto::Int16);
    assert_eq!(by_name("I32").arrow_type, FieldTypeDto::Int32);
    assert_eq!(by_name("I64").arrow_type, FieldTypeDto::Int64);
    assert_eq!(by_name("U8").arrow_type, FieldTypeDto::Uint8);
    assert_eq!(by_name("U16").arrow_type, FieldTypeDto::Uint16);
    assert_eq!(by_name("U32").arrow_type, FieldTypeDto::Uint32);
    assert_eq!(by_name("U64").arrow_type, FieldTypeDto::Uint64);
    assert_eq!(by_name("F32").arrow_type, FieldTypeDto::Float32);
    assert_eq!(by_name("F32").unit.as_deref(), Some("m/s"));
    assert_eq!(by_name("F32").multiplier, 0.5);
    assert_eq!(by_name("F64").arrow_type, FieldTypeDto::Float64);
    assert_eq!(by_name("B").arrow_type, FieldTypeDto::Boolean);
    assert_eq!(by_name("S").arrow_type, FieldTypeDto::Utf8);
    assert_eq!(by_name("LS").arrow_type, FieldTypeDto::LargeUtf8);

    let telemetry = dto.sources.iter().find(|s| s.label == "telemetry").unwrap();
    assert_eq!(telemetry.topics.len(), 1);
    let telemetry_gps = &telemetry.topics[0];
    assert_eq!(telemetry_gps.name, "GPS");
    assert_eq!(telemetry_gps.row_count, 2);
    assert_ne!(telemetry_gps.handle, flight_gps.handle);

    let all_handles: Vec<_> = dto
        .sources
        .iter()
        .flat_map(|s| s.topics.iter())
        .flat_map(|t| t.fields.iter())
        .map(|f| f.handle.clone())
        .collect();
    let mut deduped = all_handles.clone();
    deduped.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    deduped.dedup();
    assert_eq!(all_handles.len(), deduped.len());

    for source in &dto.sources {
        assert!(handles.topic_id(&source.handle).is_none());
        for topic in &source.topics {
            assert!(handles.topic_id(&topic.handle).is_some());
            for field in &topic.fields {
                assert!(handles.field(&field.handle).is_some());
            }
        }
    }
}

#[test]
fn source_offset_that_overflows_nanoseconds_is_rejected() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("overflow");
    identity.set_source_offset_us(source, i64::MAX).unwrap();
    let snapshot = StoreSnapshot::from_registry(&identity, [], 0).unwrap();

    let error = catalog::build(&snapshot).unwrap_err();
    assert_eq!(error.code(), "internal");
}

#[test]
fn effective_range_addition_overflow_at_microsecond_level_is_rejected() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("overflow");
    identity.set_source_offset_us(source, -5).unwrap();
    let topic = identity.add_topic(source, "BARO").unwrap();
    identity.add_field(topic, "Alt").unwrap();

    let schema = Arc::new(
        TopicSchema::new(
            "BARO",
            [FieldSchema::new("Alt", DataType::Float64, None::<String>, 1.0).unwrap()],
        )
        .unwrap(),
    );
    let chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![i64::MIN + 2]),
            vec![Arc::new(Float64Array::from(vec![1.0])) as ArrayRef],
            &schema,
        )
        .unwrap(),
    );
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();

    let error = catalog::build(&snapshot).unwrap_err();
    assert_eq!(error.code(), "internal");
}

#[test]
fn effective_range_conversion_overflow_at_nanosecond_level_is_rejected() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("overflow");
    let topic = identity.add_topic(source, "BARO").unwrap();
    identity.add_field(topic, "Alt").unwrap();

    let schema = Arc::new(
        TopicSchema::new(
            "BARO",
            [FieldSchema::new("Alt", DataType::Float64, None::<String>, 1.0).unwrap()],
        )
        .unwrap(),
    );
    let overflow_us = i64::MAX / 1000 + 1;
    let chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![overflow_us]),
            vec![Arc::new(Float64Array::from(vec![1.0])) as ArrayRef],
            &schema,
        )
        .unwrap(),
    );
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();

    let error = catalog::build(&snapshot).unwrap_err();
    assert_eq!(error.code(), "internal");
}
