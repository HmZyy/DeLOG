from __future__ import annotations

import json

import httpx
import pyarrow as pa
import pytest

from delog_client import (
    Client,
    ConflictError,
    InvalidInputError,
    NotFoundError,
    RequestStatus,
    UnavailableError,
)

from conftest import FakeControl, FakeInstance


def mutation_requests(server: FakeInstance) -> list[httpx.Request]:
    return [
        r
        for r in server.requests
        if r.url.path.startswith(("/v1/control", "/v1/publications"))
        and r.url.path != "/v1/control/state"
    ]


def test_every_mutation_gets_a_fresh_random_key(
    client: Client, control: FakeControl, server: FakeInstance, arrow_table: pa.Table
) -> None:
    publication = client.publish_topic("attitude_error", arrow_table)
    window = client.windows.open("w")
    window.workspace.add_plot(direction="vertical")
    client.markers.add(1000, label="a")
    client.markers.add(1000, label="a")
    client.playback.set(speed=2.0)
    with client.batch():
        client.playback.set(speed=1.0)
    publication.remove()
    client.remove_owned()
    keys = [r.headers["idempotency-key"] for r in mutation_requests(server)]
    assert len(keys) == 9
    assert len(set(keys)) == len(keys)
    assert all(16 <= len(key) <= 128 and key.isascii() for key in keys)
    assert len(control.executed) == 7


def test_queries_carry_no_key(client: Client, server: FakeInstance) -> None:
    client.layouts.list()
    client.layouts.current()
    client.vehicle_profiles.list()
    client.vehicle_profiles.load("quad")
    client.state()
    assert all("idempotency-key" not in r.headers for r in server.requests)


def test_caller_supplied_key_is_sent_verbatim_and_replays(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    first = client.markers.add(1000, label="a", idempotency_key="marker-a")
    second = client.markers.add(1000, label="a", idempotency_key="marker-a")
    requests = mutation_requests(server)
    assert [r.headers["idempotency-key"] for r in requests] == ["marker-a", "marker-a"]
    assert requests[0].content == requests[1].content
    assert first.handle == second.handle
    assert len(control.executed) == 1
    with pytest.raises(ConflictError):
        client.markers.add(2000, label="b", idempotency_key="marker-a")


def test_caller_keys_for_publications_and_batches(
    client: Client, server: FakeInstance, arrow_table: pa.Table
) -> None:
    publication = client.publish_topic("e", arrow_table, idempotency_key="pub-1")
    publication.remove(idempotency_key="pub-1-remove")
    with client.batch(idempotency_key="batch-1"):
        client.playback.set(speed=2.0)
    client.remove_owned(idempotency_key="cleanup-1")
    assert [r.headers["idempotency-key"] for r in mutation_requests(server)] == [
        "pub-1",
        "pub-1-remove",
        "batch-1",
        "cleanup-1",
    ]


@pytest.mark.parametrize("key", ["", "x" * 129, "has space", "café", "tab\there"])
def test_invalid_keys_are_rejected_before_http(
    client: Client, server: FakeInstance, arrow_table: pa.Table, key: str
) -> None:
    before = len(server.requests)
    with pytest.raises(InvalidInputError):
        client.markers.add(1000, idempotency_key=key)
    with pytest.raises(InvalidInputError):
        client.publish_topic("e", arrow_table, idempotency_key=key)
    with pytest.raises(InvalidInputError):
        client.request_status(idempotency_key=key)
    assert len(server.requests) == before


def test_lost_response_is_reconciled_by_key_without_reexecuting(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.failures.append("lost_response")
    marker = client.markers.add(1000, label="a")
    posts = mutation_requests(server)
    assert len(posts) == 1
    assert len(control.executed) == 1
    assert control.lookups == [posts[0].headers["idempotency-key"]]
    lookup = [r for r in server.requests if r.url.path == "/v1/requests/by-key"][0]
    assert lookup.headers["idempotency-key"] == posts[0].headers["idempotency-key"]
    assert "key" not in lookup.url.params
    assert marker.label == "a"


def test_lost_body_is_reconciled_by_request_id(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.failures.append("lost_body")
    client.playback.set(speed=2.0)
    request_id = next(iter(control.records.values())).request_id
    assert control.lookups == [request_id]
    assert len(mutation_requests(server)) == 1
    assert len(control.executed) == 1


def test_lost_request_is_resent_byte_identical_with_the_same_key(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.failures.append("lost_request")
    marker = client.markers.add(1000, label="a", color="#ff0000", note="n")
    posts = mutation_requests(server)
    assert len(posts) == 2
    assert posts[0].headers["idempotency-key"] == posts[1].headers["idempotency-key"]
    assert posts[0].content == posts[1].content
    assert len(control.executed) == 1
    assert marker.handle


def test_in_flight_json_request_is_resent_under_the_same_key(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.failures.append("lost_in_flight")
    client.playback.set(speed=2.0)
    posts = mutation_requests(server)
    assert len(posts) == 2
    assert posts[0].content == posts[1].content
    assert len(control.executed) == 1


def test_unknown_outcome_is_raised_and_never_duplicated(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.failures.append("lost_unknown")
    with pytest.raises(UnavailableError) as caught:
        client.windows.open("w")
    error = caught.value
    assert error.completion == "unknown"
    assert error.request_id is not None
    assert len(mutation_requests(server)) == 1
    assert control.executed == []
    status = client.request_status(error.request_id)
    assert isinstance(status, RequestStatus)
    assert status.state == "unknown"
    assert status.request_id == error.request_id
    assert isinstance(status.error, UnavailableError)
    assert status.error.completion == "unknown"
    key = mutation_requests(server)[0].headers["idempotency-key"]
    assert client.request_status(idempotency_key=key).request_id == error.request_id


def test_server_reported_unknown_is_not_retried(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.errors["playback_set"] = httpx.Response(
        503,
        json={
            "request_id": "req-timeout",
            "code": "unavailable",
            "message": "UI response timed out",
            "retryable": True,
            "completion": "unknown",
        },
    )
    with pytest.raises(UnavailableError) as caught:
        client.playback.set(speed=2.0)
    assert caught.value.completion == "unknown"
    assert len(mutation_requests(server)) == 1
    assert control.lookups == []


def test_connect_failure_is_not_started_without_lookup(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.failures.append("connect")
    with pytest.raises(UnavailableError) as caught:
        client.playback.set(speed=2.0)
    assert caught.value.completion == "not_started"
    assert control.lookups == []


def test_failed_lookup_reports_unknown(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.failures.append("lost_response")
    control.lookup_failure = "connection refused"
    with pytest.raises(UnavailableError) as caught:
        client.playback.set(speed=2.0)
    assert caught.value.completion == "unknown"
    assert len(mutation_requests(server)) == 1
    assert caught.value.details["idempotency_key"]


def test_request_status_for_a_completed_request(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    client.markers.add(1000, label="a", idempotency_key="k-1")
    status = client.request_status(idempotency_key="k-1")
    assert status.state == "completed"
    assert status.status_code == 200
    assert status.response == {"kind": "resource", "handle": control.records["k-1"].body["handle"]}
    assert status.error is None
    with pytest.raises(NotFoundError):
        client.request_status(idempotency_key="never-used")
    with pytest.raises(InvalidInputError):
        client.request_status()
    with pytest.raises(InvalidInputError):
        client.request_status("../requests/by-key")


def test_table_upload_is_replayed_from_a_fresh_stream(
    client: Client, control: FakeControl, server: FakeInstance, arrow_table: pa.Table
) -> None:
    control.failures.append("lost_request")
    publication = client.publish_topic("attitude_error", arrow_table, units={"error": "rad"})
    puts = [r for r in server.requests if r.method == "PUT"]
    assert len(puts) == 2
    assert puts[0].headers["idempotency-key"] == puts[1].headers["idempotency-key"]
    assert puts[0].content == puts[1].content
    assert str(puts[0].url) == str(puts[1].url)
    assert len(control.uploads) == 1
    assert publication.field("error").unit == "rad"


def test_lost_upload_response_returns_the_committed_publication(
    client: Client, control: FakeControl, server: FakeInstance, arrow_table: pa.Table
) -> None:
    control.failures.append("lost_response")
    publication = client.publish_topic("attitude_error", arrow_table)
    assert len([r for r in server.requests if r.method == "PUT"]) == 1
    assert publication.generation == 1
    assert publication.handle == control.publications["attitude_error"]["handle"]


def reader_for(table: pa.Table) -> pa.RecordBatchReader:
    return pa.RecordBatchReader.from_batches(table.schema, table.to_batches(2))


def test_one_shot_reader_is_not_replayed(
    client: Client, control: FakeControl, server: FakeInstance, arrow_table: pa.Table
) -> None:
    control.failures.append("lost_request")
    with pytest.raises(UnavailableError) as caught:
        client.publish_topic("attitude_error", reader_for(arrow_table))
    assert caught.value.completion == "not_started"
    assert "fresh" in caught.value.message
    assert len([r for r in server.requests if r.method == "PUT"]) == 1
    assert control.uploads == []
    client.publish_topic("attitude_error", reader_for(arrow_table))
    assert len(control.uploads) == 1


def test_in_flight_reader_upload_is_unknown(
    client: Client, control: FakeControl, server: FakeInstance, arrow_table: pa.Table
) -> None:
    control.failures.append("lost_in_flight")
    with pytest.raises(UnavailableError) as caught:
        client.publish_topic("attitude_error", reader_for(arrow_table))
    assert caught.value.completion == "unknown"
    assert caught.value.request_id is not None
    assert len([r for r in server.requests if r.method == "PUT"]) == 1


def test_lost_batch_is_resent_identically(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.failures.append("lost_request")
    with client.batch():
        client.playback.set(speed=2.0)
        client.workspace.equalize()
    posts = [r for r in server.requests if r.url.path == "/v1/control/batch"]
    assert len(posts) == 2
    assert posts[0].content == posts[1].content
    assert json.loads(posts[0].content)["commands"][1] == {"op": "workspace_equalize"}
    assert len(control.batches) == 1
