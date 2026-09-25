#![allow(dead_code)]

use arrow::array::{ArrayRef, Int64Array};
use arrow::datatypes::DataType;
use delog_core::{
    chunk::Chunk,
    identity::IdentityRegistry,
    schema::{FieldSchema, TopicSchema},
    snapshot::{DataStore, StoreSnapshot},
    store::TopicStore,
};
use std::sync::Arc;

pub fn store(chunks: usize, rows: usize) -> Arc<DataStore> {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    identity.set_source_offset_us(source, -10).unwrap();
    let topic = identity.add_topic(source, "IMU").unwrap();
    identity.add_field(topic, "x").unwrap();
    identity.add_field(topic, "y").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "IMU",
            [
                FieldSchema::new("x", DataType::Int64, None::<String>, 1.0).unwrap(),
                FieldSchema::new("y", DataType::Int64, None::<String>, 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    let data = (0..chunks).map(|c| {
        Arc::new(
            Chunk::try_new(
                Int64Array::from_iter_values((0..rows).map(|r| (c * rows + r) as i64)),
                vec![
                    Arc::new(Int64Array::from(vec![7; rows])) as ArrayRef,
                    Arc::new(Int64Array::from(vec![11; rows])) as ArrayRef,
                ],
                &schema,
            )
            .unwrap(),
        )
    });
    let topic_store = Arc::new(TopicStore::from_chunks(schema.clone(), data).unwrap());
    Arc::new(DataStore::from_snapshot(
        StoreSnapshot::from_registry(&identity, [(topic, topic_store)], 0).unwrap(),
    ))
}

pub fn wide_store(topics: usize) -> Arc<DataStore> {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("wide");
    let mut stores = Vec::with_capacity(topics);
    for index in 0..topics {
        let name = format!("topic{index}");
        let topic = identity.add_topic(source, &name).unwrap();
        identity.add_field(topic, "x").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                name,
                [FieldSchema::new("x", DataType::Int64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        stores.push((
            topic,
            Arc::new(TopicStore::from_chunks(schema, []).unwrap()),
        ));
    }
    Arc::new(DataStore::from_snapshot(
        StoreSnapshot::from_registry(&identity, stores, 0).unwrap(),
    ))
}

#[derive(Default)]
pub struct RecordingControlHost {
    pub calls: std::sync::Mutex<Vec<delog_api::control::ControlRequest>>,
}

impl delog_api::control::AuthorizedControlHost for RecordingControlHost {
    fn call_as(
        &self,
        _principal: delog_api::control::ControlPrincipal,
        request: delog_api::control::ControlRequest,
    ) -> delog_api::Result<delog_api::control::ControlResponse> {
        self.calls.lock().unwrap().push(request);
        Ok(delog_api::control::ControlResponse::Unit)
    }
}

pub fn services(store: Arc<DataStore>) -> delog_remote::RemoteServices {
    delog_remote::RemoteServices {
        store,
        ingest: delog_core::ingest::ingest_channel().0,
        control: Arc::new(RecordingControlHost::default()),
        owners: Arc::new(delog_remote::OwnerRegistry::new()),
    }
}
