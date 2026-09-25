from __future__ import annotations

import io
import queue
import threading
from collections.abc import Callable, Iterable, Iterator, Mapping
from typing import TYPE_CHECKING, Any

import pyarrow

from .errors import InvalidInputError, NotFoundError, StaleHandleError
from .models import (
    PublishedField,
    finite_number,
    wire_float,
    wire_handle,
    wire_int,
    wire_list,
    wire_object,
    wire_optional_str,
    wire_str,
)
from .transport import UploadBody

if TYPE_CHECKING:
    from _typeshed import ReadableBuffer

    from .client import Client

TIME_COLUMN = "__delog_time_ns"
SOURCE_TIME_COLUMN = "__source_time_ns"
RESERVED_PREFIX = "__delog_"
UNIT_META = b"delog.unit"
DESCRIPTION_META = b"delog.description"
MULTIPLIER_META = b"delog.multiplier"
MAX_TOPIC_NAME = 128
CHUNK_BYTES = 256 * 1024
QUEUE_CHUNKS = 2
POLL_SECONDS = 0.05
JOIN_SECONDS = 2.0


class _Cancelled(Exception):
    pass


class _Failure:
    __slots__ = ("error",)

    def __init__(self, error: BaseException) -> None:
        self.error = error


_END = object()


class _QueueWriter(io.RawIOBase):
    def __init__(self, put: Callable[[bytes], None], chunk_size: int) -> None:
        super().__init__()
        self._put = put
        self._chunk_size = chunk_size
        self._pending = bytearray()

    def writable(self) -> bool:
        return True

    def write(self, data: ReadableBuffer) -> int:
        if self.closed:
            raise _Cancelled()
        view = memoryview(data).cast("B")
        written = len(view)
        while view:
            room = self._chunk_size - len(self._pending)
            self._pending += view[:room]
            view = view[room:]
            if len(self._pending) >= self._chunk_size:
                self._emit()
        return written

    def flush(self) -> None:
        return None

    def finish(self) -> None:
        if self._pending:
            self._emit()

    def _emit(self) -> None:
        chunk = bytes(self._pending)
        self._pending.clear()
        self._put(chunk)


class _ArrowUploadStream(UploadBody):
    def __init__(
        self,
        schema: pyarrow.Schema,
        batches: Iterable[pyarrow.RecordBatch],
        *,
        chunk_size: int = CHUNK_BYTES,
    ) -> None:
        self._schema = schema
        self._batches = batches
        self._queue: queue.Queue[object] = queue.Queue(maxsize=QUEUE_CHUNKS)
        self._cancel = threading.Event()
        self._writer = _QueueWriter(self._put, chunk_size)
        self._thread: threading.Thread | None = None
        self._started = False
        self.failure: BaseException | None = None

    @property
    def producer_alive(self) -> bool:
        return self._thread is not None and self._thread.is_alive()

    @property
    def queued_chunks(self) -> int:
        return self._queue.qsize()

    def __iter__(self) -> Iterator[bytes]:
        if self._started:
            raise RuntimeError("an Arrow upload stream can only be sent once")
        self._started = True
        thread = threading.Thread(target=self._produce, name="delog-arrow-upload", daemon=True)
        self._thread = thread
        thread.start()
        finished = False
        try:
            while True:
                item = self._next(thread)
                if item is _END:
                    finished = True
                    return
                if isinstance(item, _Failure):
                    raise item.error
                assert isinstance(item, bytes)
                yield item
        finally:
            if not finished:
                self.cancel()

    def close(self) -> None:
        self.cancel()

    def cancel(self) -> None:
        self._cancel.set()
        self._writer.close()
        self._discard_queued()
        if self._thread is not None:
            self._thread.join(JOIN_SECONDS)
        self._discard_queued()

    def _next(self, thread: threading.Thread) -> object:
        while True:
            try:
                return self._queue.get(timeout=POLL_SECONDS)
            except queue.Empty:
                if self._cancel.is_set():
                    raise _Cancelled() from None
                if not thread.is_alive() and self._queue.empty():
                    raise RuntimeError("the Arrow upload producer stopped unexpectedly") from None

    def _discard_queued(self) -> None:
        while True:
            try:
                self._queue.get_nowait()
            except queue.Empty:
                return

    def _put(self, item: object) -> None:
        while not self._cancel.is_set():
            try:
                self._queue.put(item, timeout=POLL_SECONDS)
                return
            except queue.Full:
                continue
        raise _Cancelled()

    def _produce(self) -> None:
        try:
            with pyarrow.ipc.new_stream(self._writer, self._schema) as writer:
                for batch in self._batches:
                    if self._cancel.is_set():
                        raise _Cancelled()
                    writer.write_batch(batch)
            self._writer.finish()
            self._put(_END)
        except BaseException as error:
            if self._cancel.is_set():
                return
            self.failure = error
            try:
                self._put(_Failure(error))
            except _Cancelled:
                pass


class Publication:
    __slots__ = (
        "_client",
        "_handle",
        "_generation",
        "_topic_handle",
        "_name",
        "_row_count",
        "_fields",
        "_stale",
        "__weakref__",
    )

    def __init__(
        self,
        client: Client,
        handle: str,
        generation: int,
        topic_handle: str,
        name: str,
        row_count: int,
        fields: list[dict[str, Any]],
    ) -> None:
        self._client = client
        self._handle = handle
        self._generation = generation
        self._topic_handle = topic_handle
        self._name = name
        self._row_count = row_count
        self._stale = False
        what = "publication"
        self._fields = tuple(
            PublishedField(
                handle=wire_handle(raw.get("handle"), what),
                name=wire_str(raw.get("name"), what),
                arrow_type=wire_str(raw.get("arrow_type"), what),
                unit=wire_optional_str(raw.get("unit"), what),
                description=wire_optional_str(raw.get("description"), what),
                multiplier=wire_float(raw.get("multiplier"), what),
                publication=self,
            )
            for raw in fields
        )
        client._track_publication(self)

    @classmethod
    def from_payload(cls, client: Client, payload: Any) -> Publication:
        what = "publication"
        raw = wire_object(payload, what)
        topic = wire_object(raw.get("topic"), what)
        return cls(
            client,
            handle=wire_handle(raw.get("handle"), what),
            generation=wire_int(raw.get("generation"), what, 1),
            topic_handle=wire_handle(topic.get("handle"), what),
            name=wire_str(topic.get("name"), what),
            row_count=wire_int(topic.get("row_count"), what, 0),
            fields=[wire_object(item, what) for item in wire_list(topic.get("fields"), what)],
        )

    @property
    def handle(self) -> str:
        return self._handle

    @property
    def generation(self) -> int:
        return self._generation

    @property
    def topic_handle(self) -> str:
        return self._topic_handle

    @property
    def name(self) -> str:
        return self._name

    @property
    def row_count(self) -> int:
        return self._row_count

    @property
    def fields(self) -> tuple[PublishedField, ...]:
        return self._fields

    @property
    def stale(self) -> bool:
        return self._stale

    def __repr__(self) -> str:
        return (
            f"Publication(name={self._name!r}, generation={self._generation!r}, "
            f"rows={self._row_count!r}, stale={self._stale!r})"
        )

    def field(self, name: str) -> PublishedField:
        for candidate in self._fields:
            if candidate.name == name:
                return candidate
        raise NotFoundError(f"publication {self._name!r} has no field named {name!r}")

    def remove(self, *, idempotency_key: str | None = None) -> None:
        self._client._guard("Publication.remove()")
        handle = self._live()
        self._client._transport.delete_mutation(
            f"/v1/publications/{handle}", idempotency_key=idempotency_key
        )
        self._stale = True

    def _live(self) -> str:
        if self._stale:
            raise StaleHandleError(
                f"publication {self._name!r} was removed or replaced; publish it again"
            )
        return self._handle

    def _invalidate(self) -> None:
        self._stale = True


def validate_topic_name(name: Any) -> str:
    if not isinstance(name, str) or not name.strip() or len(name) > MAX_TOPIC_NAME:
        raise InvalidInputError(
            f"a publication topic name must be 1 to {MAX_TOPIC_NAME} characters"
        )
    if "/" in name:
        raise InvalidInputError("a publication topic name cannot contain '/'")
    if name in (".", ".."):
        raise InvalidInputError("a publication topic name cannot be '.' or '..'")
    return name


def prepare_upload(
    data: Any,
    units: Mapping[str, str] | None,
    descriptions: Mapping[str, str] | None,
    multipliers: Mapping[str, float] | None,
) -> tuple[pyarrow.Schema, Callable[[], Iterable[pyarrow.RecordBatch]], bool]:
    if isinstance(data, pyarrow.Table):
        source_schema = data.schema
    elif isinstance(data, pyarrow.RecordBatchReader):
        source_schema = data.schema
    else:
        raise InvalidInputError("data must be a pyarrow.Table or pyarrow.RecordBatchReader")
    schema = _schema_with_metadata(source_schema, units, descriptions, multipliers)
    if isinstance(data, pyarrow.Table):
        table = pyarrow.Table.from_arrays(data.columns, schema=schema)
        return schema, table.to_batches, True
    reader = data

    def batches() -> Iterator[pyarrow.RecordBatch]:
        for batch in reader:
            yield pyarrow.RecordBatch.from_arrays(batch.columns, schema=schema)

    return schema, batches, False


def _schema_with_metadata(
    schema: pyarrow.Schema,
    units: Mapping[str, str] | None,
    descriptions: Mapping[str, str] | None,
    multipliers: Mapping[str, float] | None,
) -> pyarrow.Schema:
    names = list(schema.names)
    if len(set(names)) != len(names):
        raise InvalidInputError("a published topic cannot contain duplicate field names")
    if TIME_COLUMN not in names:
        raise InvalidInputError(f"a published topic requires an int64 {TIME_COLUMN} column")
    if schema.field(TIME_COLUMN).type != pyarrow.int64():
        raise InvalidInputError(f"{TIME_COLUMN} must have Arrow int64 type")
    if SOURCE_TIME_COLUMN in names and schema.field(SOURCE_TIME_COLUMN).type != pyarrow.int64():
        raise InvalidInputError(f"{SOURCE_TIME_COLUMN} must have Arrow int64 type")
    data_fields = [name for name in names if name not in (TIME_COLUMN, SOURCE_TIME_COLUMN)]
    for name in data_fields:
        if name.startswith(RESERVED_PREFIX):
            raise InvalidInputError(f"field {name!r} uses the reserved {RESERVED_PREFIX} namespace")
    if not data_fields:
        raise InvalidInputError("a published topic needs at least one data field")
    extra: dict[str, dict[bytes, bytes]] = {name: {} for name in data_fields}
    for option, mapping, meta in (
        ("units", units, UNIT_META),
        ("descriptions", descriptions, DESCRIPTION_META),
    ):
        for name, value in _entries(option, mapping, extra):
            if not isinstance(value, str):
                raise InvalidInputError(f"{option}[{name!r}] must be a string")
            extra[name][meta] = value.encode("utf-8")
    for name, value in _entries("multipliers", multipliers, extra):
        number = finite_number(value, f"multipliers[{name!r}]")
        extra[name][MULTIPLIER_META] = repr(number).encode("ascii")
    fields = []
    for column in schema:
        added = extra.get(column.name)
        if added:
            merged = _metadata(column.metadata)
            merged.update(_metadata(added))
            column = column.with_metadata(merged)
        fields.append(column)
    return pyarrow.schema(fields, metadata=_metadata(schema.metadata) or None)


def _metadata(metadata: Mapping[bytes, bytes] | None) -> dict[bytes | str, bytes | str]:
    return {key: value for key, value in (metadata or {}).items()}


def _entries(
    option: str, mapping: Mapping[str, Any] | None, known: Mapping[str, Any]
) -> list[tuple[str, Any]]:
    if mapping is None:
        return []
    if not isinstance(mapping, Mapping):
        raise InvalidInputError(f"{option} must be a mapping of field names")
    for name in mapping:
        if name not in known:
            raise InvalidInputError(f"{option} names {name!r}, which is not a data field")
    return list(mapping.items())
