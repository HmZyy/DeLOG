use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;

use delog_core::chunk::Chunk;
use delog_core::identity::{IdentityRegistry, TopicId};
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_core::store::TopicStore;

use delog_remote::{
    ClientId, ClientRegistry, LeaseId, LeaseManager, RegisterClientRequest, SecretToken,
};

struct Fixture {
    store: DataStore,
    identity: IdentityRegistry,
    topic: TopicId,
    schema: Arc<TopicSchema>,
}

fn schema() -> Arc<TopicSchema> {
    Arc::new(
        TopicSchema::new(
            "BARO",
            [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
        )
        .unwrap(),
    )
}

fn chunk(schema: &TopicSchema, times: Vec<i64>) -> Arc<Chunk> {
    let values: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![1.0; times.len()]))];
    Arc::new(Chunk::try_new(Int64Array::from(times), values, schema).unwrap())
}

fn fixture() -> Fixture {
    let mut identity = IdentityRegistry::new();
    let source = identity.add_source("flight");
    let topic = identity.add_topic(source, "BARO").unwrap();
    identity.add_field(topic, "Alt").unwrap();

    let schema = schema();
    let topic_store = Arc::new(
        TopicStore::from_chunks(Arc::clone(&schema), [chunk(&schema, vec![100, 200])]).unwrap(),
    );
    let snapshot = StoreSnapshot::from_registry(&identity, [(topic, topic_store)], 0).unwrap();

    Fixture {
        store: DataStore::from_snapshot(snapshot),
        identity,
        topic,
        schema,
    }
}

fn publish_second_epoch(fixture: &Fixture) {
    let topic_store = Arc::new(
        TopicStore::from_chunks(
            Arc::clone(&fixture.schema),
            [chunk(&fixture.schema, vec![300, 400])],
        )
        .unwrap(),
    );
    let snapshot =
        StoreSnapshot::from_registry(&fixture.identity, [(fixture.topic, topic_store)], 0).unwrap();
    fixture.store.publish(snapshot).unwrap();
}

fn register_client(registry: &mut ClientRegistry, bootstrap: &SecretToken, name: &str) -> ClientId {
    registry
        .register(
            bootstrap,
            bootstrap,
            RegisterClientRequest {
                name: name.to_owned(),
                takeover: false,
            },
        )
        .unwrap()
        .client_id
}

#[test]
fn lease_pins_one_epoch_and_expires_only_after_inactivity() {
    let store = fixture();
    let now = Instant::now();
    let leases = LeaseManager::new(Duration::from_secs(600));

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "c1");

    let created = leases.create(client, store.store.load(), now).unwrap();

    publish_second_epoch(&store);
    assert_eq!(
        leases
            .read(&created.id, &created.client, now)
            .unwrap()
            .snapshot()
            .epoch,
        0
    );

    leases.reap(now + Duration::from_secs(599));
    assert!(leases.read(&created.id, &created.client, now).is_ok());

    leases.reap(now + Duration::from_secs(1_200));
    assert_eq!(
        leases
            .read(&created.id, &created.client, now)
            .unwrap_err()
            .code(),
        "snapshot_expired"
    );
}

#[test]
fn read_with_the_wrong_client_is_forbidden() {
    let store = fixture();
    let now = Instant::now();
    let leases = LeaseManager::new(Duration::from_secs(600));

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let owner = register_client(&mut registry, &bootstrap, "owner");
    let intruder = register_client(&mut registry, &bootstrap, "intruder");

    let created = leases.create(owner, store.store.load(), now).unwrap();

    let error = leases.read(&created.id, &intruder, now).unwrap_err();
    assert_eq!(error.code(), "forbidden");
}

#[test]
fn close_leaves_an_expired_tombstone_and_is_idempotent() {
    let store = fixture();
    let now = Instant::now();
    let ttl = Duration::from_secs(600);
    let leases = LeaseManager::new(ttl);

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "c1");
    let intruder = register_client(&mut registry, &bootstrap, "intruder");

    let created = leases.create(client, store.store.load(), now).unwrap();

    leases.close(&created.id, &created.client).unwrap();
    assert!(leases.list_active().is_empty());
    assert_eq!(
        leases
            .read(&created.id, &created.client, now)
            .unwrap_err()
            .code(),
        "snapshot_expired"
    );
    assert_eq!(
        leases.read(&created.id, &intruder, now).unwrap_err().code(),
        "forbidden"
    );
    assert_eq!(
        leases.close(&created.id, &intruder).unwrap_err().code(),
        "forbidden"
    );

    leases.close(&created.id, &created.client).unwrap();
    assert_eq!(
        leases
            .read(&created.id, &created.client, now)
            .unwrap_err()
            .code(),
        "snapshot_expired"
    );

    let purge = Instant::now() + ttl;
    assert_eq!(leases.reap(purge), 0);
    assert_eq!(
        leases
            .read(&created.id, &created.client, purge)
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[test]
fn close_with_the_wrong_client_is_forbidden_and_keeps_the_lease() {
    let store = fixture();
    let now = Instant::now();
    let leases = LeaseManager::new(Duration::from_secs(600));

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let owner = register_client(&mut registry, &bootstrap, "owner");
    let intruder = register_client(&mut registry, &bootstrap, "intruder");

    let created = leases.create(owner, store.store.load(), now).unwrap();

    let error = leases.close(&created.id, &intruder).unwrap_err();
    assert_eq!(error.code(), "forbidden");
    assert!(leases.read(&created.id, &created.client, now).is_ok());
}

#[test]
fn revoke_client_closes_every_lease_owned_by_that_client() {
    let store = fixture();
    let now = Instant::now();
    let leases = LeaseManager::new(Duration::from_secs(600));

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "c1");

    let first = leases
        .create(client.clone(), store.store.load(), now)
        .unwrap();
    let second = leases
        .create(client.clone(), store.store.load(), now)
        .unwrap();

    leases.revoke_client(&client);

    assert!(leases.list_active().is_empty());
    assert_eq!(
        leases.read(&first.id, &client, now).unwrap_err().code(),
        "snapshot_expired"
    );
    assert_eq!(
        leases.read(&second.id, &client, now).unwrap_err().code(),
        "snapshot_expired"
    );
}

#[test]
fn closing_a_lease_cancels_an_active_stream_immediately() {
    let store = fixture();
    let now = Instant::now();
    let leases = LeaseManager::new(Duration::from_secs(600));

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "c1");

    let created = leases.create(client, store.store.load(), now).unwrap();
    let guard = leases.read(&created.id, &created.client, now).unwrap();
    assert!(!guard.cancellation().is_cancelled());

    leases.close(&created.id, &created.client).unwrap();

    assert!(guard.cancellation().is_cancelled());
}

#[test]
fn reaping_an_expired_lease_cancels_any_active_stream() {
    let store = fixture();
    let now = Instant::now();
    let leases = LeaseManager::new(Duration::from_secs(600));

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "c1");

    let created = leases.create(client, store.store.load(), now).unwrap();
    let guard = leases.read(&created.id, &created.client, now).unwrap();

    let expired = leases.reap(now + Duration::from_secs(1_200));
    assert_eq!(expired, 1);
    assert!(guard.cancellation().is_cancelled());
}

#[test]
fn acquisition_at_or_after_the_idle_deadline_expires_without_reaping() {
    for elapsed in [Duration::from_secs(10), Duration::from_secs(11)] {
        let store = fixture();
        let now = Instant::now();
        let leases = LeaseManager::new(Duration::from_secs(10));
        let mut registry = ClientRegistry::new();
        let bootstrap = SecretToken::generate().unwrap();
        let owner = register_client(&mut registry, &bootstrap, "owner");
        let intruder = register_client(&mut registry, &bootstrap, "intruder");
        let created = leases
            .create(owner.clone(), store.store.load(), now)
            .unwrap();
        let guard = leases.read(&created.id, &owner, now).unwrap();
        assert_eq!(
            leases
                .read(&created.id, &intruder, now + elapsed)
                .unwrap_err()
                .code(),
            "forbidden"
        );
        assert!(!guard.cancellation().is_cancelled());
        assert_eq!(
            leases
                .read(&created.id, &owner, now + elapsed)
                .unwrap_err()
                .code(),
            "snapshot_expired"
        );
        assert!(guard.cancellation().is_cancelled());
        assert!(leases.list_active().is_empty());
        assert_eq!(
            leases
                .read(&created.id, &owner, now + elapsed + Duration::from_secs(1))
                .unwrap_err()
                .code(),
            "snapshot_expired"
        );
    }
}

#[test]
fn active_reader_count_tracks_outstanding_guards_and_lists_active_leases() {
    let store = fixture();
    let now = Instant::now();
    let leases = LeaseManager::new(Duration::from_secs(600));

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "c1");

    let created = leases
        .create(client.clone(), store.store.load(), now)
        .unwrap();

    let statuses = leases.list_active();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].id, created.id);
    assert_eq!(statuses[0].client, client);
    assert_eq!(statuses[0].active_readers, 0);

    let first_guard = leases.read(&created.id, &created.client, now).unwrap();
    let second_guard = leases.read(&created.id, &created.client, now).unwrap();
    assert_eq!(leases.list_active()[0].active_readers, 2);

    drop(first_guard);
    assert_eq!(leases.list_active()[0].active_readers, 1);

    drop(second_guard);
    assert_eq!(leases.list_active()[0].active_readers, 0);
}

#[test]
fn two_reads_of_one_lease_return_identical_handles() {
    let store = fixture();
    let now = Instant::now();
    let leases = LeaseManager::new(Duration::from_secs(600));

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "c1");

    let created = leases.create(client, store.store.load(), now).unwrap();

    let first = leases.read(&created.id, &created.client, now).unwrap();
    let second = leases.read(&created.id, &created.client, now).unwrap();

    let first_topic_handle = first.catalog().sources[0].topics[0].handle.clone();
    let second_topic_handle = second.catalog().sources[0].topics[0].handle.clone();
    assert_eq!(first_topic_handle, second_topic_handle);
    assert_eq!(
        first.topic_id(&first_topic_handle),
        second.topic_id(&second_topic_handle)
    );

    let first_field_handle = first.catalog().sources[0].topics[0].fields[0]
        .handle
        .clone();
    let second_field_handle = second.catalog().sources[0].topics[0].fields[0]
        .handle
        .clone();
    assert_eq!(first_field_handle, second_field_handle);
    assert_eq!(
        first.field(&first_field_handle).unwrap().field,
        second.field(&second_field_handle).unwrap().field
    );
}

#[test]
fn read_of_an_unknown_lease_is_not_found() {
    let store = fixture();
    let now = Instant::now();
    let leases = LeaseManager::new(Duration::from_secs(600));

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "c1");

    leases
        .create(client.clone(), store.store.load(), now)
        .unwrap();
    let unknown: LeaseId = serde_json::from_str("\"unknown-lease\"").unwrap();

    let error = leases.read(&unknown, &client, now).unwrap_err();
    assert_eq!(error.code(), "not_found");
    leases.close(&unknown, &client).unwrap();
}

#[test]
fn reap_purges_expired_tombstones_after_a_further_grace_period() {
    let store = fixture();
    let now = Instant::now();
    let ttl = Duration::from_secs(600);
    let leases = LeaseManager::new(ttl);

    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "c1");

    let created = leases
        .create(client.clone(), store.store.load(), now)
        .unwrap();

    let expired_at = now + Duration::from_secs(1_200);
    assert_eq!(leases.reap(expired_at), 1);
    assert_eq!(
        leases
            .read(&created.id, &client, expired_at)
            .unwrap_err()
            .code(),
        "snapshot_expired"
    );

    let still_within_grace = expired_at + Duration::from_secs(599);
    assert_eq!(leases.reap(still_within_grace), 0);
    assert_eq!(
        leases
            .read(&created.id, &client, still_within_grace)
            .unwrap_err()
            .code(),
        "snapshot_expired"
    );

    let past_grace = expired_at + Duration::from_secs(600);
    assert_eq!(leases.reap(past_grace), 0);
    assert_eq!(
        leases
            .read(&created.id, &client, past_grace)
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[test]
fn touch_renews_an_active_lease_but_never_revives_an_idle_one() {
    let store = fixture();
    let now = Instant::now();
    let ttl = Duration::from_secs(10);
    let leases = LeaseManager::new(ttl);
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let owner = register_client(&mut registry, &bootstrap, "owner");
    let intruder = register_client(&mut registry, &bootstrap, "intruder");
    let created = leases
        .create(owner.clone(), store.store.load(), now)
        .unwrap();

    assert!(!leases.touch(&created.id, &intruder, now + Duration::from_secs(9)));
    assert!(leases.touch(&created.id, &owner, now + Duration::from_secs(9)));
    assert_eq!(leases.reap(now + Duration::from_secs(18)), 0);
    assert!(
        leases
            .read(&created.id, &owner, now + Duration::from_secs(18))
            .is_ok()
    );

    assert!(!leases.touch(&created.id, &owner, now + Duration::from_secs(28)));
    assert_eq!(leases.reap(now + Duration::from_secs(28)), 1);
    assert!(!leases.touch(&created.id, &owner, now + Duration::from_secs(28)));
}

#[test]
fn stream_cancellation_follows_the_lease_without_cancelling_it() {
    let store = fixture();
    let now = Instant::now();
    let leases = LeaseManager::new(Duration::from_secs(600));
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "c1");
    let created = leases.create(client, store.store.load(), now).unwrap();

    let mut first = leases.read(&created.id, &created.client, now).unwrap();
    let stream = first.stream_cancellation();
    stream.cancel();
    assert!(first.cancellation().is_cancelled());
    let second = leases.read(&created.id, &created.client, now).unwrap();
    assert!(!second.cancellation().is_cancelled());

    let mut third = leases.read(&created.id, &created.client, now).unwrap();
    let stream = third.stream_cancellation();
    leases.reap(now + Duration::from_secs(1_200));
    assert!(stream.is_cancelled());
    assert!(second.cancellation().is_cancelled());
}

#[test]
fn each_client_holds_at_most_sixteen_active_leases() {
    let store = fixture();
    let now = Instant::now();
    let ttl = Duration::from_secs(600);
    let leases = LeaseManager::new(ttl);
    let mut registry = ClientRegistry::new();
    let bootstrap = SecretToken::generate().unwrap();
    let client = register_client(&mut registry, &bootstrap, "greedy");
    let other = register_client(&mut registry, &bootstrap, "other");

    let created: Vec<_> = (0..16)
        .map(|_| {
            leases
                .create(client.clone(), store.store.load(), now)
                .unwrap()
        })
        .collect();
    let error = leases
        .create(client.clone(), store.store.load(), now)
        .err()
        .unwrap();
    assert_eq!(error.code(), "unavailable");
    assert!(error.retryable());
    assert!(!error.message().contains("greedy"));
    assert!(leases.ensure_capacity(&client, now).is_err());
    assert!(leases.create(other, store.store.load(), now).is_ok());

    leases.close(&created[0].id, &client).unwrap();
    assert!(leases.ensure_capacity(&client, now).is_ok());
    leases
        .create(client.clone(), store.store.load(), now)
        .unwrap();
    assert!(
        leases
            .create(client.clone(), store.store.load(), now)
            .is_err()
    );

    let later = now + ttl;
    assert!(leases.ensure_capacity(&client, later).is_ok());
    assert!(leases.create(client, store.store.load(), later).is_ok());
}
