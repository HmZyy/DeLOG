from __future__ import annotations

import httpx
import pyarrow as pa
import pytest

from delog_client import (
    AmbiguousError,
    Client,
    Field,
    InternalError,
    InvalidInputError,
    NotFoundError,
    Snapshot,
    SnapshotExpiredError,
    Source,
    StaleHandleError,
    Topic,
    UnavailableError,
)

from conftest import FakeInstance, arrow_bytes, attitude_schema, split, topic_dto


def data_requests(server: FakeInstance) -> list[httpx.Request]:
    return [r for r in server.requests if r.url.path.endswith("/data")]


def test_duplicate_topics_require_source_qualification(mock_client: Client) -> None:
    snapshot = mock_client.snapshot()
    with pytest.raises(AmbiguousError) as caught:
        snapshot.topic("vehicle_attitude")
    assert caught.value.candidates == [
        "flight/vehicle_attitude",
        "simulation/vehicle_attitude",
    ]
    assert snapshot.topic("vehicle_attitude", source="flight").source.label == "flight"


def test_catalog_models(mock_client: Client) -> None:
    with mock_client.snapshot() as snapshot:
        assert isinstance(snapshot, Snapshot)
        assert snapshot.epoch == 7
        sources = snapshot.sources()
        assert [s.label for s in sources] == ["simulation", "flight"]
        flight = sources[1]
        assert isinstance(flight, Source)
        assert flight.kind == "file"
        assert flight.offset_ns == -2000
        topic = flight.topics[0]
        assert isinstance(topic, Topic)
        assert topic.source is flight
        assert topic.row_count == 12
        assert topic.time_range_ns is not None
        assert (topic.time_range_ns.start_ns, topic.time_range_ns.end_ns) == (0, 11000)
        assert [f.name for f in topic.fields] == ["roll", "pitch", "yaw"]
        roll = topic.field("roll")
        assert isinstance(roll, Field)
        assert roll.topic is topic
        assert roll.arrow_type == "float32"
        assert roll.unit == "rad"
        assert roll.description == "roll angle"
        assert roll.multiplier == 1.0
        assert roll.selector.source == "flight"
        assert roll.selector.field == "roll"


def test_models_are_frozen(mock_client: Client) -> None:
    with mock_client.snapshot() as snapshot:
        topic = snapshot.topic("vehicle_attitude", source="flight")
        with pytest.raises(AttributeError):
            setattr(topic, "name", "other")
        repr(topic)
        repr(topic.source)
        repr(topic.fields[0])


def test_instance_topics_use_bracketed_names(mock_client: Client) -> None:
    with mock_client.snapshot() as snapshot:
        with pytest.raises(AmbiguousError) as caught:
            snapshot.topic("sensor_accel")
        assert caught.value.candidates == ["flight/sensor_accel[0]", "flight/sensor_accel[1]"]
        second = snapshot.topic("sensor_accel", instance=1)
        assert second.name == "sensor_accel[1]"
        assert second.base_name == "sensor_accel"
        assert second.instance == 1
        assert snapshot.topic("sensor_accel[1]") is second
        assert snapshot.topic("sensor_accel[1]", source="flight", instance=1) is second
        with pytest.raises(NotFoundError):
            snapshot.topic("sensor_accel[1]", instance=0)
        selector = second.field("x").selector
        assert (selector.topic, selector.instance) == ("sensor_accel[1]", 1)
        for candidate in caught.value.candidates:
            source, name = candidate.split("/", 1)
            assert snapshot.topic(name, source=source).name == name


def test_missing_topic_and_source_raise_not_found(mock_client: Client) -> None:
    with mock_client.snapshot() as snapshot:
        with pytest.raises(NotFoundError):
            snapshot.topic("missing")
        with pytest.raises(NotFoundError):
            snapshot.topic("vehicle_attitude", source="nowhere")
        with pytest.raises(NotFoundError):
            snapshot.topic("vehicle_attitude", source="flight").field("missing")
        assert snapshot.source("flight").label == "flight"
        with pytest.raises(NotFoundError):
            snapshot.source("nowhere")


def test_catalog_fetched_once(mock_client: Client, server: FakeInstance) -> None:
    with mock_client.snapshot() as snapshot:
        for _ in range(3):
            snapshot.topic("vehicle_attitude", source="flight")
            snapshot.sources()
    catalogs = [r for r in server.requests if r.url.path.endswith("/catalog")]
    assert len(catalogs) == 1
    assert catalogs[0].url.path == f"/v1/snapshots/{snapshot.lease_id}/catalog"


def test_snapshot_request_shape(mock_client: Client, server: FakeInstance) -> None:
    snapshot = mock_client.snapshot()
    create = next(r for r in server.requests if r.url.path == "/v1/snapshots")
    assert create.method == "POST"
    assert snapshot.lease_id in server.leases
    snapshot.close()


def test_context_manager_closes_lease(mock_client: Client, server: FakeInstance) -> None:
    with mock_client.snapshot() as snapshot:
        lease = snapshot.lease_id
        assert lease in server.leases
    assert lease not in server.leases
    assert snapshot.closed
    closing = server.requests[-1]
    assert (closing.method, closing.url.path) == ("DELETE", f"/v1/snapshots/{lease}")


def test_explicit_close_is_idempotent_and_blocks_reads(
    mock_client: Client, server: FakeInstance
) -> None:
    snapshot = mock_client.snapshot()
    topic = snapshot.topic("vehicle_attitude", source="flight")
    snapshot.close()
    snapshot.close()
    deletes = [r for r in server.requests if r.method == "DELETE"]
    assert len(deletes) == 1
    before = len(server.requests)
    with pytest.raises(SnapshotExpiredError):
        topic.read()
    with pytest.raises(SnapshotExpiredError):
        topic.iter_batches()
    assert len(server.requests) == before


def test_close_errors_do_not_mask_body_errors(mock_client: Client, server: FakeInstance) -> None:
    with pytest.raises(KeyError):
        with mock_client.snapshot():
            server.reachable = False
            raise KeyError("body")
    server.reachable = True


def test_close_error_surfaces_without_body_error(mock_client: Client, server: FakeInstance) -> None:
    snapshot = mock_client.snapshot()
    server.reachable = False
    with pytest.raises(UnavailableError):
        snapshot.close()
    server.reachable = True


def test_expired_snapshot_raises_typed_error(mock_client: Client, server: FakeInstance) -> None:
    snapshot = mock_client.snapshot()
    topic = snapshot.topic("vehicle_attitude", source="flight")
    server.leases.clear()
    with pytest.raises(SnapshotExpiredError) as caught:
        topic.read()
    assert not caught.value.retryable
    assert caught.value.request_id is not None


def test_catalog_failure_releases_lease(mock_client: Client, server: FakeInstance) -> None:
    server.catalog = {"snapshot_epoch": "bad", "sources": []}
    with pytest.raises(InternalError):
        mock_client.snapshot()
    assert server.leases == set()
    assert server.requests[-1].method == "DELETE"


def test_catalog_rejects_unsafe_handles(mock_client: Client, server: FakeInstance) -> None:
    server.catalog["sources"][0]["topics"][0]["handle"] = "../../clients"
    with pytest.raises(InternalError):
        mock_client.snapshot()
    assert server.leases == set()


def test_catalog_ignores_additive_fields(mock_client: Client, server: FakeInstance) -> None:
    server.catalog["future"] = 1
    server.catalog["sources"][0]["future"] = 2
    server.catalog["sources"][0]["topics"][0]["fields"][0]["future"] = 3
    with mock_client.snapshot() as snapshot:
        assert snapshot.topic("vehicle_attitude", source="simulation").fields[0].name == "roll"


def test_default_projection_omits_fields(mock_client: Client, server: FakeInstance) -> None:
    with mock_client.snapshot() as snapshot:
        topic = snapshot.topic("vehicle_attitude", source="flight")
        table = topic.read()
    request = data_requests(server)[0]
    assert request.url.path == f"/v1/snapshots/{snapshot.lease_id}/topics/{topic.handle}/data"
    assert dict(request.url.params) == {}
    assert request.headers["accept"] == "application/vnd.apache.arrow.stream"
    assert table.column_names[:2] == ["__delog_time_ns", "__source_time_ns"]
    assert table.num_rows == 12
    assert table.column("roll").to_pylist()[:3] == [0.0, 1.0, 2.0]


def test_projected_query_parameters(mock_client: Client, server: FakeInstance) -> None:
    with mock_client.snapshot() as snapshot:
        topic = snapshot.topic("vehicle_attitude", source="flight")
        yaw = topic.field("yaw")
        roll = topic.field("roll")
        topic.read(fields=[yaw, "roll"], start_ns=-5, end_ns=9000)
        list(topic.iter_batches(fields=("pitch",), end_ns=0))
        list(topic.iter_batches(start_ns=10))
    first, second, third = data_requests(server)
    assert first.url.params.get_list("fields") == [f"{yaw.handle},{roll.handle}"]
    assert first.url.params["start_ns"] == "-5"
    assert first.url.params["end_ns"] == "9000"
    assert dict(second.url.params) == {"fields": topic.field("pitch").handle, "end_ns": "0"}
    assert dict(third.url.params) == {"start_ns": "10"}


def test_invalid_projection_is_rejected_locally(mock_client: Client, server: FakeInstance) -> None:
    with mock_client.snapshot() as snapshot:
        topic = snapshot.topic("vehicle_attitude", source="flight")
        other = snapshot.topic("vehicle_attitude", source="simulation")
        with pytest.raises(NotFoundError):
            topic.iter_batches(fields=["missing"])
        with pytest.raises(InvalidInputError):
            topic.iter_batches(fields=[other.field("roll")])
        with pytest.raises(InvalidInputError):
            topic.iter_batches(fields=[])
        with pytest.raises(InvalidInputError):
            topic.iter_batches(fields=["roll", topic.field("roll")])
        with pytest.raises(InvalidInputError):
            topic.iter_batches(start_ns=10, end_ns=5)
        with pytest.raises(InvalidInputError):
            topic.iter_batches(start_ns=True)
        with pytest.raises(InvalidInputError):
            topic.iter_batches(end_ns=2**63)
    assert data_requests(server) == []


def test_fields_from_an_older_snapshot_are_stale(mock_client: Client) -> None:
    with mock_client.snapshot() as old:
        roll = old.topic("vehicle_attitude", source="flight").field("roll")
    with mock_client.snapshot() as new:
        topic = new.topic("vehicle_attitude", source="flight")
        with pytest.raises(StaleHandleError):
            topic.iter_batches(fields=[roll])


def test_empty_stream_reads_schema_only_table(mock_client: Client, server: FakeInstance) -> None:
    server.data_chunks = split(arrow_bytes([], attitude_schema()), 16)
    with mock_client.snapshot() as snapshot:
        table = snapshot.topic("vehicle_attitude", source="flight").read()
    assert isinstance(table, pa.Table)
    assert table.num_rows == 0
    assert table.schema.names == attitude_schema().names


def add_bare_and_bracketed(server: FakeInstance) -> None:
    server.catalog["sources"].append(
        {
            "handle": "tlog-source",
            "label": "tlog",
            "kind": "file",
            "offset_ns": 0,
            "topics": [
                topic_dto("tlog", "HEARTBEAT", "HEARTBEAT", None, ["type"]),
                topic_dto("tlog", "HEARTBEAT[1]", "HEARTBEAT", 1, ["type"]),
            ],
        }
    )
    server.catalog["sources"][0]["topics"].append(
        topic_dto("simulation", "HEARTBEAT[2]", "HEARTBEAT", 2, ["type"])
    )


def test_bare_instance_zero_topic_resolves_by_exact_name(
    mock_client: Client, server: FakeInstance
) -> None:
    add_bare_and_bracketed(server)
    with mock_client.snapshot() as snapshot:
        bare = snapshot.topic("HEARTBEAT", source="tlog")
        assert (bare.name, bare.instance) == ("HEARTBEAT", None)
        with pytest.raises(AmbiguousError) as caught:
            snapshot.topic("HEARTBEAT")
        assert caught.value.candidates == ["simulation/HEARTBEAT[2]", "tlog/HEARTBEAT"]
        for candidate in caught.value.candidates:
            label, topic_name = candidate.split("/", 1)
            assert snapshot.topic(topic_name, source=label).path == candidate
        second = snapshot.topic("HEARTBEAT", instance=1)
        assert second.name == "HEARTBEAT[1]"
        assert snapshot.topic("HEARTBEAT", source="tlog", instance=1) is second
        assert snapshot.topic("HEARTBEAT[1]", source="tlog") is second
        assert snapshot.topic("HEARTBEAT", instance=2).source.label == "simulation"


def test_every_ambiguity_candidate_round_trips(mock_client: Client, server: FakeInstance) -> None:
    add_bare_and_bracketed(server)
    with mock_client.snapshot() as snapshot:
        queries = [
            ("vehicle_attitude", None),
            ("sensor_accel", None),
            ("sensor_accel", "flight"),
            ("HEARTBEAT", None),
        ]
        seen = 0
        for name, source in queries:
            try:
                snapshot.topic(name, source=source)
            except AmbiguousError as error:
                for candidate in error.candidates:
                    label, topic_name = candidate.split("/", 1)
                    assert snapshot.topic(topic_name, source=label).path == candidate
                    seen += 1
        assert seen >= 4


def test_snapshot_operations_after_client_close_are_typed(
    mock_client: Client, server: FakeInstance
) -> None:
    snapshot = mock_client.snapshot()
    topic = snapshot.topic("vehicle_attitude", source="flight")
    pending = topic.iter_batches()
    mock_client.close()
    before = len(server.requests)
    with pytest.raises(SnapshotExpiredError):
        topic.read()
    with pytest.raises(SnapshotExpiredError):
        topic.iter_batches()
    with pytest.raises(SnapshotExpiredError):
        next(pending)
    snapshot.close()
    assert snapshot.closed
    assert len(server.requests) == before


def test_with_block_survives_client_close(mock_client: Client, server: FakeInstance) -> None:
    with mock_client.snapshot() as snapshot:
        mock_client.close()
    assert snapshot.closed
    with pytest.raises(SnapshotExpiredError):
        snapshot.topic("vehicle_attitude", source="flight").read()
