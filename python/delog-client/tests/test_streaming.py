from __future__ import annotations

from collections.abc import Iterator

import pyarrow as pa
import pytest

from delog_client import Client, InternalError, UnavailableError
from delog_client.transport import ChunkReader

from conftest import FakeInstance, arrow_bytes, attitude_batches, attitude_schema, split


class CountingChunks:
    def __init__(self, chunks: list[bytes]) -> None:
        self.chunks = chunks
        self.pulled = 0

    def __iter__(self) -> Iterator[bytes]:
        for chunk in self.chunks:
            self.pulled += len(chunk)
            yield chunk


@pytest.mark.parametrize("request_size", [1, 3, 7, 8, 20, 64])
def test_readinto_retains_at_most_one_chunk(request_size: int) -> None:
    data = bytes(range(256)) * 4
    chunks = [data[i : i + 7] for i in range(0, len(data), 7)]
    chunks.insert(3, b"")
    source = CountingChunks(chunks)
    reader = ChunkReader(iter(source))
    delivered = bytearray()
    while True:
        buffer = bytearray(request_size)
        count = reader.readinto(buffer)
        assert count is not None
        if count == 0:
            break
        delivered.extend(buffer[:count])
        assert source.pulled - len(delivered) <= 7
        assert reader.pending_bytes <= 7
    assert bytes(delivered) == data
    assert reader.peak_pending_bytes <= 7


def test_readinto_fills_request_across_chunks() -> None:
    reader = ChunkReader(iter([b"ab", b"cd", b"ef"]))
    buffer = bytearray(5)
    assert reader.readinto(buffer) == 5
    assert bytes(buffer) == b"abcde"
    assert reader.pending_bytes == 1
    assert reader.read(10) == b"f"
    assert reader.read(10) == b""


def test_reader_is_readable_and_not_seekable() -> None:
    reader = ChunkReader(iter([]))
    assert reader.readable()
    assert not reader.seekable()


def test_iter_batches_streams_incrementally(mock_client: Client, server: FakeInstance) -> None:
    batches = attitude_batches(count=50, rows=8)
    chunks = split(arrow_bytes(batches, attitude_schema()), 97)
    server.data_chunks = chunks
    with mock_client.snapshot() as snapshot:
        topic = snapshot.topic("vehicle_attitude", source="flight")
        iterator = topic.iter_batches()
        first = next(iterator)
        stream = server.streams[-1]
        assert first.num_rows == 8
        assert stream.yielded < len(chunks)
        rest = list(iterator)
    assert len(rest) == 49
    assert pa.Table.from_batches([first, *rest]).equals(pa.Table.from_batches(batches))
    assert stream.closed


def test_early_close_releases_stream(mock_client: Client, server: FakeInstance) -> None:
    server.data_chunks = split(arrow_bytes(attitude_batches(count=20), attitude_schema()), 50)
    with mock_client.snapshot() as snapshot:
        iterator = snapshot.topic("vehicle_attitude", source="flight").iter_batches()
        next(iterator)
        stream = server.streams[-1]
        assert not stream.closed
        iterator.close()
        assert stream.closed
        assert stream.yielded < len(server.data_chunks)


def test_break_out_of_loop_releases_stream(mock_client: Client, server: FakeInstance) -> None:
    server.data_chunks = split(arrow_bytes(attitude_batches(count=20), attitude_schema()), 50)
    with mock_client.snapshot() as snapshot:
        topic = snapshot.topic("vehicle_attitude", source="flight")
        iterator = topic.iter_batches()
        for _ in iterator:
            break
        iterator.close()
        assert server.streams[-1].closed


def test_read_closes_stream(mock_client: Client, server: FakeInstance) -> None:
    with mock_client.snapshot() as snapshot:
        snapshot.topic("vehicle_attitude", source="flight").read()
    assert server.streams[-1].closed


def test_interrupted_stream_is_unavailable_and_closed(
    mock_client: Client, server: FakeInstance
) -> None:
    server.data_chunks = split(arrow_bytes(attitude_batches(count=10), attitude_schema()), 40)
    server.data_fail_after = 5
    with mock_client.snapshot() as snapshot:
        topic = snapshot.topic("vehicle_attitude", source="flight")
        with pytest.raises(UnavailableError):
            list(topic.iter_batches())
        assert server.streams[-1].closed
        with pytest.raises(UnavailableError):
            topic.read()
        assert server.streams[-1].closed


def test_truncated_stream_is_internal(mock_client: Client, server: FakeInstance) -> None:
    data = arrow_bytes(attitude_batches(count=3), attitude_schema())
    server.data_chunks = split(data[: len(data) // 2], 32)
    with mock_client.snapshot() as snapshot:
        with pytest.raises(InternalError):
            snapshot.topic("vehicle_attitude", source="flight").read()
    assert server.streams[-1].closed


def test_wrong_content_type_is_internal(mock_client: Client, server: FakeInstance) -> None:
    server.data_content_type = "application/json"
    with mock_client.snapshot() as snapshot:
        with pytest.raises(InternalError):
            list(snapshot.topic("vehicle_attitude", source="flight").iter_batches())
    assert server.streams[-1].closed


def test_generator_defers_request_until_iterated(mock_client: Client, server: FakeInstance) -> None:
    with mock_client.snapshot() as snapshot:
        iterator = snapshot.topic("vehicle_attitude", source="flight").iter_batches()
        assert server.streams == []
        iterator.close()
        assert server.streams == []
