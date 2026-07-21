use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arrow::array::{ArrayRef, Float64Array, Int16Array, Int64Array};
use arrow::datatypes::DataType;
use delog_core::chunk::Chunk;
use delog_core::identity::{IdentityRegistry, SourceId, TopicId};
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

use crate::eval::{EvalCache, EvalReport, evaluate};
use crate::graph::{Graph, NodeId};

/// `evaluate()` with no script host wired up; used by tests that don't exercise script nodes
/// so they don't need to thread the `scripting`-gated parameter through every call site.
#[cfg(feature = "scripting")]
pub(crate) fn eval_no_host(
    graph: &Graph,
    snapshot: &StoreSnapshot,
    targets: &[NodeId],
    cancel: &AtomicBool,
    cache: &mut EvalCache,
) -> EvalReport {
    evaluate(graph, snapshot, targets, cancel, cache, None)
}

#[cfg(not(feature = "scripting"))]
pub(crate) fn eval_no_host(
    graph: &Graph,
    snapshot: &StoreSnapshot,
    targets: &[NodeId],
    cancel: &AtomicBool,
    cache: &mut EvalCache,
) -> EvalReport {
    evaluate(graph, snapshot, targets, cancel, cache)
}

fn numeric_topic(
    identity: &mut IdentityRegistry,
    source: SourceId,
    name: &str,
    times: Vec<i64>,
    fields: Vec<(&str, &str, Vec<f64>)>,
) -> (TopicId, Arc<TopicStore>) {
    let topic = identity.add_topic(source, name).unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            name,
            fields.iter().map(|(name, unit, _)| {
                identity.add_field(topic, *name).unwrap();
                FieldSchema::new(*name, DataType::Float64, Some(*unit), 1.0).unwrap()
            }),
        )
        .unwrap(),
    );
    let columns = fields
        .into_iter()
        .map(|(_, _, values)| Arc::new(Float64Array::from(values)) as ArrayRef)
        .collect();
    let chunk = Arc::new(Chunk::try_new(Int64Array::from(times), columns, &schema).unwrap());
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    (topic, store)
}

pub(crate) fn snapshot_two_sources() -> StoreSnapshot {
    let mut identity = IdentityRegistry::new();
    let flight_01 = identity.add_source("flight_01");
    let flight_02 = identity.add_source("flight_02");
    let stores = [
        numeric_topic(
            &mut identity,
            flight_01,
            "IMU[0]",
            vec![100, 200, 300],
            vec![("AccX", "m/s^2", vec![1.0, 2.0, 3.0])],
        ),
        numeric_topic(
            &mut identity,
            flight_01,
            "GPS",
            vec![100, 200, 300],
            vec![("Alt", "m", vec![10.0, 11.0, 12.0])],
        ),
        numeric_topic(
            &mut identity,
            flight_02,
            "IMU[0]",
            vec![100, 200, 300],
            vec![("AccX", "m/s^2", vec![4.0, 5.0, 6.0])],
        ),
        numeric_topic(
            &mut identity,
            flight_02,
            "GPS",
            vec![100, 200, 300],
            vec![("Alt", "m", vec![13.0, 14.0, 15.0])],
        ),
    ];
    StoreSnapshot::from_registry(&identity, stores, 7).unwrap()
}

pub(crate) fn snapshot_gps_baro() -> StoreSnapshot {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let stores = [
        numeric_topic(
            &mut identity,
            source,
            "IMU[0]",
            vec![100, 200, 300],
            vec![
                ("AccX", "m/s^2", vec![1.0, 2.0, f64::NAN]),
                ("AccY", "m/s^2", vec![10.0, 20.0, 30.0]),
                ("Other", "s", vec![4.0, 5.0, 6.0]),
            ],
        ),
        numeric_topic(
            &mut identity,
            source,
            "GPS",
            vec![100, 200, 300],
            vec![("Alt", "m", vec![1.0, -1.0, 0.0])],
        ),
        numeric_topic(
            &mut identity,
            source,
            "BARO",
            vec![150, 250],
            vec![("Alt", "m", vec![10.0, 20.0])],
        ),
    ];
    StoreSnapshot::from_registry(&identity, stores, 11).unwrap()
}

pub(crate) fn snapshot_scaled_i16() -> StoreSnapshot {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "SCALED").unwrap();
    identity.add_field(topic, "A").unwrap();
    identity.add_field(topic, "B").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "SCALED",
            [
                FieldSchema::new("A", DataType::Int16, Some("m"), 0.01).unwrap(),
                FieldSchema::new("B", DataType::Int16, Some("m"), 0.02).unwrap(),
            ],
        )
        .unwrap(),
    );
    let chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![100, 200]),
            vec![
                Arc::new(Int16Array::from(vec![100, 200])) as ArrayRef,
                Arc::new(Int16Array::from(vec![25, 50])) as ArrayRef,
            ],
            &schema,
        )
        .unwrap(),
    );
    let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
    StoreSnapshot::from_registry(&identity, [(topic, store)], 19).unwrap()
}

pub(crate) fn snapshot_duplicate_times() -> StoreSnapshot {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let stores = [
        numeric_topic(
            &mut identity,
            source,
            "SRC",
            vec![10, 20, 20, 30],
            vec![("Value", "m", vec![1.0, 2.0, 22.0, 3.0])],
        ),
        numeric_topic(
            &mut identity,
            source,
            "BASE",
            vec![25],
            vec![("Value", "m", vec![0.0])],
        ),
    ];
    StoreSnapshot::from_registry(&identity, stores, 13).unwrap()
}

pub(crate) fn snapshot_overlapping_chunks() -> StoreSnapshot {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "SRC").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "SRC",
            [FieldSchema::new("Value", DataType::Float64, Some("m"), 1.0).unwrap()],
        )
        .unwrap(),
    );
    identity.add_field(topic, "Value").unwrap();
    let chunk = |times, values| {
        Arc::new(
            Chunk::try_new(
                Int64Array::from(times),
                vec![Arc::new(Float64Array::from(values)) as ArrayRef],
                &schema,
            )
            .unwrap(),
        )
    };
    let source_store = Arc::new(
        TopicStore::from_chunks(
            Arc::clone(&schema),
            [
                chunk(vec![20, 30, 40], vec![2.0, 3.0, 4.0]),
                chunk(vec![10, 30, 50], vec![1.0, 33.0, 5.0]),
            ],
        )
        .unwrap(),
    );
    let base = numeric_topic(
        &mut identity,
        source,
        "BASE",
        vec![15, 30, 35, 45],
        vec![("Value", "m", vec![0.0; 4])],
    );
    StoreSnapshot::from_registry(&identity, [(topic, source_store), base], 17).unwrap()
}
