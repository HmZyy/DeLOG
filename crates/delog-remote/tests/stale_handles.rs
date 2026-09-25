use arrow::datatypes::DataType;
use delog_core::identity::IdentityRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;
use delog_remote::control::handles::HandleTarget;
use delog_remote::control::map::{ControlFieldResolver, LiveFieldResolver};
use delog_remote::{
    ClientRegistry, LeaseManager, PublicationRegistry, RegisterClientRequest, SecretToken,
};
use delog_remote::{ControlHandleRegistry, NativePlotKey};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn a_removed_then_recreated_plot_does_not_revalidate_the_old_handle() {
    let mut handles = ControlHandleRegistry::new();
    let old = handles.register_plot(NativePlotKey::new(0, 7));
    handles.invalidate_plot(NativePlotKey::new(0, 7));
    let new = handles.register_plot(NativePlotKey::new(0, 7));
    assert_ne!(old, new);
    assert_eq!(
        handles.resolve_plot(&old).unwrap_err().code(),
        "stale_handle"
    );
}

#[test]
fn a_plot_replaced_at_the_same_tile_id_stales_the_old_handle_on_refresh() {
    let mut handles = ControlHandleRegistry::new();
    let old = handles.register_plot(NativePlotKey::with_instance_id(0, 7, 1));
    let live = std::collections::HashSet::from([HandleTarget::Plot {
        window: 0,
        tile: 7,
        instance_id: 2,
    }]);
    handles.retain_live(&live);
    let new = handles.register_plot(NativePlotKey::with_instance_id(0, 7, 2));
    assert_ne!(old, new);
    assert_eq!(
        handles.resolve_plot(&old).unwrap_err().code(),
        "stale_handle"
    );
}

#[test]
fn field_handle_expires_with_its_snapshot_lease() {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "BARO").unwrap();
    identity.add_field(topic, "Alt").unwrap();
    let schema = Arc::new(
        TopicSchema::new(
            "BARO",
            [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
        )
        .unwrap(),
    );
    let store = Arc::new(TopicStore::from_chunks(schema, []).unwrap());
    let snapshot = Arc::new(StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap());
    let leases = LeaseManager::new(Duration::from_secs(60));
    let mut clients = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let registered = clients
        .register(
            &bootstrap,
            &bootstrap,
            RegisterClientRequest {
                name: "c".into(),
                takeover: false,
            },
        )
        .unwrap();
    let client = registered.client_id;
    let now = Instant::now();
    let lease = leases
        .create(client.clone(), snapshot.clone(), now)
        .unwrap();
    let handle = leases
        .read(&lease.id, &client, now)
        .unwrap()
        .catalog()
        .sources[0]
        .topics[0]
        .fields[0]
        .handle
        .clone();
    let source_handle = leases
        .read(&lease.id, &client, now)
        .unwrap()
        .catalog()
        .sources[0]
        .handle
        .clone();
    let publications = PublicationRegistry::new();
    let resolver = LiveFieldResolver::new(
        &leases,
        &publications,
        snapshot.as_ref(),
        &client,
        &registered.owner_id,
        now,
    );
    let field = resolver.resolve(&handle).unwrap();
    assert_eq!(field.trace_path, "BARO.Alt");
    assert_eq!(field.vehicle_path, "flight/BARO/Alt");
    assert_eq!(
        resolver.resolve_source(&source_handle).unwrap().label,
        "flight"
    );
    leases.close(&lease.id, &client).unwrap();
    assert_eq!(
        resolver.resolve(&handle).unwrap_err().code(),
        "stale_handle"
    );
    assert_eq!(
        resolver.resolve_source(&source_handle).unwrap_err().code(),
        "stale_handle"
    );
}
