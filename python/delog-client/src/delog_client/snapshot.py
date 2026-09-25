from __future__ import annotations

import re
from types import TracebackType
from typing import TYPE_CHECKING, Any

from .errors import AmbiguousError, InternalError, NotFoundError, SnapshotExpiredError
from .models import I64_MAX, I64_MIN, Field, FieldSelector, Source, TimeRange, Topic

if TYPE_CHECKING:
    from .client import Client
    from .transport import Transport

HANDLE = re.compile(r"[A-Za-z0-9_-]{1,128}")


class Snapshot:
    __slots__ = ("_client", "_transport", "_lease_id", "_epoch", "_sources", "_closed")

    def __init__(self, client: Client, transport: Transport, lease_id: str, catalog: Any) -> None:
        self._client = client
        self._transport = transport
        self._lease_id = lease_id
        self._closed = False
        self._epoch, self._sources = _parse_catalog(self, catalog)

    @classmethod
    def from_payload(cls, client: Client, transport: Transport, payload: Any) -> Snapshot:
        lease_id = _handle(payload.get("lease_id") if isinstance(payload, dict) else None)
        try:
            catalog = transport.get_json(f"/v1/snapshots/{lease_id}/catalog")
            return cls(client, transport, lease_id, catalog)
        except BaseException:
            try:
                transport.delete(f"/v1/snapshots/{lease_id}")
            except Exception:
                pass
            raise

    @property
    def lease_id(self) -> str:
        return self._lease_id

    @property
    def epoch(self) -> int:
        return self._epoch

    @property
    def closed(self) -> bool:
        return self._closed

    @property
    def client(self) -> Client:
        return self._client

    def __enter__(self) -> Snapshot:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        if exc is None:
            self.close()
            return
        try:
            self.close()
        except Exception:
            pass

    def __repr__(self) -> str:
        return (
            f"Snapshot(lease_id={self._lease_id!r}, epoch={self._epoch!r}, closed={self._closed!r})"
        )

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self._transport.closed:
            return
        self._transport.delete(f"/v1/snapshots/{self._lease_id}")

    def sources(self) -> tuple[Source, ...]:
        return self._sources

    def source(self, label: str) -> Source:
        matches = [s for s in self._sources if s.label == label]
        if not matches:
            raise NotFoundError(f"no source labelled {label!r} in this snapshot")
        if len(matches) > 1:
            raise AmbiguousError.for_candidates("source", label, [s.label for s in matches])
        return matches[0]

    def topic(
        self,
        name: str,
        *,
        source: str | None = None,
        instance: int | None = None,
    ) -> Topic:
        matches = self._matching_topics(name, source=source, instance=instance)
        if not matches:
            raise NotFoundError.for_topic(name)
        if len(matches) != 1:
            raise AmbiguousError.for_topics(name, [topic.path for topic in matches])
        return matches[0]

    def _matching_topics(
        self, name: str, *, source: str | None, instance: int | None
    ) -> list[Topic]:
        matches: list[Topic] = []
        for candidate_source in self._sources:
            if source is None or candidate_source.label == source:
                matches.extend(_source_matches(candidate_source, name, instance))
        return matches

    def _ensure_open(self) -> None:
        self._transport.ensure_open()
        if self._closed:
            raise SnapshotExpiredError(
                f"snapshot {self._lease_id} is closed; create a new snapshot", retryable=False
            )


def _source_matches(source: Source, name: str, instance: int | None) -> list[Topic]:
    if instance is not None:
        return [
            topic
            for topic in source.topics
            if name in (topic.name, topic.base_name) and topic.instance == instance
        ]
    exact = [topic for topic in source.topics if topic.name == name]
    if exact:
        return exact
    return [topic for topic in source.topics if topic.base_name == name]


def _parse_catalog(snapshot: Snapshot, payload: Any) -> tuple[int, tuple[Source, ...]]:
    catalog = _object(payload)
    epoch = _int(catalog.get("snapshot_epoch"), 0, 2**64 - 1)
    sources = tuple(_parse_source(snapshot, raw) for raw in _list(catalog.get("sources")))
    return epoch, sources


def _parse_source(snapshot: Snapshot, payload: Any) -> Source:
    raw = _object(payload)
    source = Source(
        handle=_handle(raw.get("handle")),
        label=_str(raw.get("label")),
        kind=_str(raw.get("kind")),
        offset_ns=_int(raw.get("offset_ns"), I64_MIN, I64_MAX),
        topics=(),
    )
    topics = tuple(_parse_topic(snapshot, source, item) for item in _list(raw.get("topics")))
    object.__setattr__(source, "topics", topics)
    return source


def _parse_topic(snapshot: Snapshot, source: Source, payload: Any) -> Topic:
    raw = _object(payload)
    time_range = raw.get("time_range_ns")
    parsed_range = None
    if time_range is not None:
        bounds = _object(time_range)
        parsed_range = TimeRange(
            start_ns=_int(bounds.get("start_ns"), I64_MIN, I64_MAX),
            end_ns=_int(bounds.get("end_ns"), I64_MIN, I64_MAX),
        )
    topic = Topic(
        handle=_handle(raw.get("handle")),
        name=_str(raw.get("name")),
        base_name=_str(raw.get("base_name")),
        instance=_optional_int(raw.get("instance"), 0, 2**32 - 1),
        row_count=_int(raw.get("row_count"), 0, 2**64 - 1),
        time_range_ns=parsed_range,
        fields=(),
        source=source,
        _snapshot=snapshot,
    )
    fields = tuple(_parse_field(topic, item) for item in _list(raw.get("fields")))
    object.__setattr__(topic, "fields", fields)
    return topic


def _parse_field(topic: Topic, payload: Any) -> Field:
    raw = _object(payload)
    selector = _object(raw.get("selector"))
    multiplier = raw.get("multiplier")
    if isinstance(multiplier, bool) or not isinstance(multiplier, (int, float)):
        raise _malformed()
    return Field(
        handle=_handle(raw.get("handle")),
        name=_str(raw.get("name")),
        arrow_type=_str(raw.get("arrow_type")),
        unit=_optional_str(raw.get("unit")),
        description=_optional_str(raw.get("description")),
        multiplier=float(multiplier),
        selector=FieldSelector(
            source=_str(selector.get("source")),
            topic=_str(selector.get("topic")),
            instance=_optional_int(selector.get("instance"), 0, 2**32 - 1),
            field=_str(selector.get("field")),
        ),
        topic=topic,
    )


def _malformed() -> InternalError:
    return InternalError("DeLOG returned a malformed snapshot catalog")


def _object(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise _malformed()
    return value


def _list(value: Any) -> list[Any]:
    if not isinstance(value, list):
        raise _malformed()
    return value


def _str(value: Any) -> str:
    if not isinstance(value, str):
        raise _malformed()
    return value


def _optional_str(value: Any) -> str | None:
    return None if value is None else _str(value)


def _int(value: Any, low: int, high: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
        raise _malformed()
    return value


def _optional_int(value: Any, low: int, high: int) -> int | None:
    return None if value is None else _int(value, low, high)


def _handle(value: Any) -> str:
    if not isinstance(value, str) or HANDLE.fullmatch(value) is None:
        raise InternalError("DeLOG returned a malformed protocol handle")
    return value
