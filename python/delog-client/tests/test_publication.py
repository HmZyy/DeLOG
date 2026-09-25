from __future__ import annotations

import io
import threading
import time
from collections.abc import Iterator
from typing import Any

import httpx
import pyarrow as pa
import pytest

import delog_client.transport as transport_module
from delog_client import (
    Client,
    ConflictError,
    DeLOG,
    ForbiddenError,
    InvalidInputError,
    NotFoundError,
    Publication,
    PublishedField,
    StaleHandleError,
)
from delog_client.publication import _ArrowUploadStream

from conftest import ARROW, FakeControl, FakeInstance, FakeNetwork, derived_table


def publication_requests(server: FakeInstance) -> list[httpx.Request]:
    return [r for r in server.requests if r.url.path.startswith("/v1/publications")]


def test_publish_then_plot_uses_returned_field_handle(
    client: Client, arrow_table: pa.Table
) -> None:
    publication = client.publish_topic(
        "attitude_error",
        arrow_table,
        units={"error": "rad"},
        replace=True,
    )
    window = client.windows.open("Flight diagnosis")
    plot = window.workspace.add_plot(direction="vertical")
    trace = plot.traces.add(publication.field("error"), mode="line")
    assert trace.field.name == "error"


def test_publish_sends_a_streamed_arrow_put_with_field_metadata(
    client: Client, control: FakeControl, server: FakeInstance, arrow_table: pa.Table
) -> None:
    publication = client.publish_topic(
        "attitude_error",
        arrow_table,
        units={"error": "rad"},
        descriptions={"error": "attitude error", "limit": "allowed error"},
        multipliers={"limit": 0.5},
    )
    [request] = publication_requests(server)
    assert request.method == "PUT"
    assert request.url.path == "/v1/publications/attitude_error"
    assert request.url.params["replace"] == "false"
    assert request.headers["content-type"] == ARROW
    assert request.headers["idempotency-key"]
    assert "owner" not in request.url.params
    name, table, _ = control.uploads[0]
    assert name == "attitude_error"
    assert table.num_rows == arrow_table.num_rows
    error_meta = table.schema.field("error").metadata
    limit_meta = table.schema.field("limit").metadata
    assert error_meta is not None and limit_meta is not None
    assert error_meta[b"delog.unit"] == b"rad"
    assert error_meta[b"delog.description"] == b"attitude error"
    assert b"delog.multiplier" not in error_meta
    assert limit_meta[b"delog.multiplier"] == b"0.5"
    assert table.column("error").to_pylist() == arrow_table.column("error").to_pylist()

    assert isinstance(publication, Publication)
    assert publication.name == "attitude_error"
    assert publication.generation == 1
    assert publication.row_count == arrow_table.num_rows
    assert [f.name for f in publication.fields] == ["error", "limit"]
    error = publication.field("error")
    assert isinstance(error, PublishedField)
    assert error.unit == "rad"
    assert error.description == "attitude error"
    assert error.arrow_type == "float64"
    assert publication.field("limit").multiplier == 0.5
    assert error.publication is publication
    with pytest.raises(NotFoundError):
        publication.field("missing")


def test_publish_accepts_a_record_batch_reader(
    client: Client, control: FakeControl, arrow_table: pa.Table
) -> None:
    reader = pa.RecordBatchReader.from_batches(arrow_table.schema, arrow_table.to_batches(3))
    publication = client.publish_topic("from_reader", reader, units={"error": "rad"})
    _, table, _ = control.uploads[0]
    assert table.column("error").to_pylist() == arrow_table.column("error").to_pylist()
    metadata = table.schema.field("error").metadata
    assert metadata is not None
    assert metadata[b"delog.unit"] == b"rad"
    assert publication.field("error").unit == "rad"


def test_replace_true_is_sent_and_conflict_otherwise(
    client: Client, control: FakeControl, server: FakeInstance, arrow_table: pa.Table
) -> None:
    first = client.publish_topic("attitude_error", arrow_table)
    with pytest.raises(ConflictError):
        client.publish_topic("attitude_error", arrow_table)
    second = client.publish_topic("attitude_error", arrow_table, replace=True)
    assert publication_requests(server)[-1].url.params["replace"] == "true"
    assert (first.generation, second.generation) == (1, 2)
    assert first.handle != second.handle


def test_safe_mode_publication_is_forbidden(
    client: Client, control: FakeControl, arrow_table: pa.Table
) -> None:
    control.access = "safe"
    with pytest.raises(ForbiddenError):
        client.publish_topic("attitude_error", arrow_table)


def _table(**columns: pa.Array[Any]) -> pa.Table:
    return pa.table(columns)


INVALID: list[tuple[str, Any, dict[str, Any]]] = [
    ("", derived_table(), {}),
    ("   ", derived_table(), {}),
    ("a/b", derived_table(), {}),
    ("x" * 129, derived_table(), {}),
    (".", derived_table(), {}),
    ("..", derived_table(), {}),
    ("ok", _table(error=pa.array([1.0])), {}),
    ("ok", _table(__delog_time_ns=pa.array([1.0]), error=pa.array([1.0])), {}),
    ("ok", _table(__delog_time_ns=pa.array([1000], pa.int64())), {}),
    (
        "ok",
        _table(
            __delog_time_ns=pa.array([1000], pa.int64()),
            __delog_extra=pa.array([1.0]),
            error=pa.array([1.0]),
        ),
        {},
    ),
    (
        "ok",
        _table(
            __delog_time_ns=pa.array([1000], pa.int64()),
            __source_time_ns=pa.array([1.0]),
            error=pa.array([1.0]),
        ),
        {},
    ),
    ("ok", derived_table(), {"units": {"missing": "rad"}}),
    ("ok", derived_table(), {"units": {"__delog_time_ns": "ns"}}),
    ("ok", derived_table(), {"units": {"error": 3}}),
    ("ok", derived_table(), {"descriptions": {"missing": "x"}}),
    ("ok", derived_table(), {"multipliers": {"error": float("nan")}}),
    ("ok", derived_table(), {"multipliers": {"error": True}}),
    ("ok", derived_table(), {"multipliers": {"error": "2"}}),
    ("ok", {"__delog_time_ns": [1000]}, {}),
]


@pytest.mark.parametrize(("name", "data", "options"), INVALID)
def test_invalid_publications_are_rejected_before_http(
    client: Client, server: FakeInstance, name: str, data: Any, options: dict[str, Any]
) -> None:
    with pytest.raises(InvalidInputError):
        client.publish_topic(name, data, **options)
    assert publication_requests(server) == []


def test_topic_names_needing_escapes_are_percent_encoded(
    client: Client, control: FakeControl, server: FakeInstance, arrow_table: pa.Table
) -> None:
    publication = client.publish_topic("attitude error[0]", arrow_table)
    [request] = publication_requests(server)
    assert request.url.raw_path.decode() == "/v1/publications/attitude%20error%5B0%5D?replace=false"
    assert control.uploads[0][0] == "attitude error[0]"
    assert publication.name == "attitude error[0]"


def test_duplicate_field_names_are_rejected_before_http(
    client: Client, server: FakeInstance
) -> None:
    table = pa.Table.from_arrays(
        [pa.array([1000], pa.int64()), pa.array([1.0]), pa.array([2.0])],
        names=["__delog_time_ns", "error", "error"],
    )
    with pytest.raises(InvalidInputError):
        client.publish_topic("dupes", table)
    assert publication_requests(server) == []


def test_removing_a_publication_stales_its_fields_locally(
    client: Client, control: FakeControl, server: FakeInstance, arrow_table: pa.Table
) -> None:
    publication = client.publish_topic("attitude_error", arrow_table)
    plot = client.windows.open("w").workspace.add_plot(direction="vertical")
    publication.remove()
    delete = publication_requests(server)[-1]
    assert delete.method == "DELETE"
    assert delete.url.path == f"/v1/publications/{publication.handle}"
    assert delete.headers["idempotency-key"]
    assert control.removed_publications == [publication.handle]
    assert publication.stale
    before = len(server.requests)
    with pytest.raises(StaleHandleError):
        plot.traces.add(publication.field("error"))
    with pytest.raises(StaleHandleError):
        publication.remove()
    assert len(server.requests) == before


def _drain(stream: _ArrowUploadStream) -> bytes:
    return b"".join(stream)


def big_table(rows: int = 50_000) -> pa.Table:
    return pa.table(
        {
            "__delog_time_ns": pa.array(range(0, rows * 1000, 1000), pa.int64()),
            "value": pa.array([float(i) for i in range(rows)], pa.float64()),
        }
    )


def test_upload_stream_yields_bounded_chunks_that_decode_to_the_table() -> None:
    table = big_table()
    stream = _ArrowUploadStream(table.schema, table.to_batches(4096), chunk_size=4096)
    chunks = list(stream)
    assert len(chunks) > 10
    assert max(len(chunk) for chunk in chunks) <= 4096
    decoded = pa.ipc.open_stream(io.BytesIO(b"".join(chunks))).read_all()
    assert decoded.equals(table)
    assert not stream.producer_alive


def test_upload_producer_blocks_behind_a_two_chunk_queue() -> None:
    table = big_table()
    stream = _ArrowUploadStream(table.schema, table.to_batches(1024), chunk_size=1024)
    iterator = iter(stream)
    first = next(iterator)
    assert len(first) <= 1024
    time.sleep(0.2)
    assert stream.producer_alive
    assert stream.queued_chunks <= 2
    rest = b"".join(iterator)
    decoded = pa.ipc.open_stream(io.BytesIO(first + rest)).read_all()
    assert decoded.num_rows == table.num_rows


def test_closing_the_upload_stream_cancels_and_joins_the_producer() -> None:
    table = big_table()
    stream = _ArrowUploadStream(table.schema, table.to_batches(1024), chunk_size=1024)
    iterator = iter(stream)
    next(iterator)
    started = time.monotonic()
    stream.close()
    assert time.monotonic() - started < 2.0
    assert not stream.producer_alive
    assert stream.queued_chunks == 0


def test_upload_stream_can_only_be_sent_once() -> None:
    table = derived_table()
    stream = _ArrowUploadStream(table.schema, table.to_batches())
    _drain(stream)
    with pytest.raises(RuntimeError):
        _drain(stream)


def failing_reader(schema: pa.Schema, good: pa.RecordBatch) -> pa.RecordBatchReader:
    def batches() -> Iterator[pa.RecordBatch]:
        yield good
        raise ValueError("sensor file truncated")

    return pa.RecordBatchReader.from_batches(schema, batches())


def test_producer_errors_propagate_from_iteration() -> None:
    table = derived_table()
    reader = failing_reader(table.schema, table.to_batches()[0])
    stream = _ArrowUploadStream(table.schema, reader, chunk_size=64)
    with pytest.raises(Exception, match="sensor file truncated"):
        _drain(stream)
    assert not stream.producer_alive


def test_producer_error_fails_the_publication_without_retrying(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    table = derived_table()
    reader = failing_reader(table.schema, table.to_batches()[0])
    with pytest.raises(InvalidInputError) as caught:
        client.publish_topic("broken", reader)
    assert caught.value.completion == "not_started"
    assert "sensor file truncated" in str(caught.value)
    assert len(publication_requests(server)) <= 1
    assert control.lookups == []
    assert control.uploads == []


class EarlyRejectTransport(httpx.BaseTransport):
    def __init__(self, fallback: httpx.BaseTransport) -> None:
        self.fallback = fallback
        self.streams: list[Any] = []
        self.consumed = 0

    def handle_request(self, request: httpx.Request) -> httpx.Response:
        if request.method != "PUT":
            return self.fallback.handle_request(request)
        stream = request.stream
        assert isinstance(stream, httpx.SyncByteStream)
        self.streams.append(stream)
        for _ in stream:
            self.consumed += 1
            break
        return httpx.Response(
            422,
            json={
                "request_id": "req-early",
                "code": "invalid_input",
                "message": "upload exceeds the byte limit",
                "retryable": False,
            },
        )


def test_early_server_rejection_cancels_the_upload_producer(
    network: FakeNetwork, server: FakeInstance, monkeypatch: pytest.MonkeyPatch
) -> None:
    server.control = FakeControl()
    early = EarlyRejectTransport(httpx.MockTransport(network.handler))
    monkeypatch.setattr(transport_module, "_http_transport", lambda: early)
    client = DeLOG.connect(server.instance_id, name="early")
    try:
        with pytest.raises(InvalidInputError) as caught:
            client.publish_topic("huge", big_table())
        assert caught.value.request_id == "req-early"
        assert early.consumed == 1
        [stream] = early.streams
        assert isinstance(stream, _ArrowUploadStream)
        assert not stream.producer_alive
        assert stream.queued_chunks == 0
        assert not any(t.name == "delog-arrow-upload" for t in threading.enumerate())
    finally:
        client.close()
