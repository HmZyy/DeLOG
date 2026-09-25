from __future__ import annotations

import json
import math
import re
from collections.abc import Generator, Iterable
from dataclasses import dataclass
from dataclasses import field as dataclass_field
from typing import TYPE_CHECKING, Any

import pyarrow

from .errors import (
    AmbiguousError,
    DeLOGError,
    InternalError,
    InvalidInputError,
    NotFoundError,
    StaleHandleError,
    from_envelope,
)

if TYPE_CHECKING:
    from .publication import Publication
    from .snapshot import Snapshot

I64_MIN = -(2**63)
I64_MAX = 2**63 - 1


@dataclass(frozen=True)
class Instance:
    id: str
    label: str
    pid: int
    endpoint: str
    api_major: int
    api_min_minor: int
    api_max_minor: int
    session_description: str | None = None

    @property
    def loaded_file(self) -> str | None:
        return self.session_description


@dataclass(frozen=True)
class TimeRange:
    start_ns: int
    end_ns: int


@dataclass(frozen=True)
class FieldSelector:
    source: str
    topic: str
    instance: int | None
    field: str


@dataclass(frozen=True)
class Field:
    handle: str
    name: str
    arrow_type: str
    unit: str | None
    description: str | None
    multiplier: float
    selector: FieldSelector
    topic: Topic = dataclass_field(repr=False, compare=False)


@dataclass(frozen=True)
class Topic:
    handle: str
    name: str
    base_name: str
    instance: int | None
    row_count: int
    time_range_ns: TimeRange | None
    fields: tuple[Field, ...]
    source: Source = dataclass_field(repr=False, compare=False)
    _snapshot: Snapshot = dataclass_field(repr=False, compare=False)

    @property
    def path(self) -> str:
        return f"{self.source.label}/{self.name}"

    def field(self, name: str) -> Field:
        matches = [f for f in self.fields if f.name == name]
        if not matches:
            raise NotFoundError(f"topic {self.path!r} has no field named {name!r}")
        if len(matches) > 1:
            raise AmbiguousError.for_candidates(
                "field", name, [f"{self.path}/{f.name}" for f in matches]
            )
        return matches[0]

    def iter_batches(
        self,
        *,
        fields: Iterable[str | Field] | None = None,
        start_ns: int | None = None,
        end_ns: int | None = None,
    ) -> Generator[pyarrow.RecordBatch, None, None]:
        self._snapshot._ensure_open()
        handles = self._resolve_field_handles(fields)
        params = _range_params(start_ns, end_ns)
        if handles is not None:
            params["fields"] = ",".join(handles)
        return self._snapshot._transport.iter_arrow(self._data_path, params)

    def read(
        self,
        *,
        fields: Iterable[str | Field] | None = None,
        start_ns: int | None = None,
        end_ns: int | None = None,
    ) -> pyarrow.Table:
        self._snapshot._ensure_open()
        handles = self._resolve_field_handles(fields)
        params = _range_params(start_ns, end_ns)
        if handles is not None:
            params["fields"] = ",".join(handles)
        return self._snapshot._transport.read_arrow_table(self._data_path, params)

    @property
    def _data_path(self) -> str:
        return f"/v1/snapshots/{self._snapshot.lease_id}/topics/{self.handle}/data"

    def _resolve_field_handles(self, fields: Iterable[str | Field] | None) -> list[str] | None:
        if fields is None:
            return None
        if isinstance(fields, str):
            fields = [fields]
        handles: list[str] = []
        for item in fields:
            if isinstance(item, Field):
                handle = self._own_field(item).handle
            elif isinstance(item, str):
                handle = self.field(item).handle
            else:
                raise InvalidInputError("fields must be field names or Field objects")
            if handle in handles:
                raise InvalidInputError(f"field {item!r} is requested more than once")
            handles.append(handle)
        if not handles:
            raise InvalidInputError("an explicit field selection must name at least one field")
        return handles

    def _own_field(self, candidate: Field) -> Field:
        if candidate.topic._snapshot is not self._snapshot:
            raise StaleHandleError(
                f"field {candidate.name!r} belongs to a different snapshot; look it up again"
            )
        for own in self.fields:
            if own.handle == candidate.handle:
                return own
        raise InvalidInputError(
            f"field {candidate.selector.field!r} belongs to topic "
            f"{candidate.topic.path!r}, not {self.path!r}"
        )


@dataclass(frozen=True)
class Source:
    handle: str
    label: str
    kind: str
    offset_ns: int
    topics: tuple[Topic, ...]


def _range_params(start_ns: int | None, end_ns: int | None) -> dict[str, str]:
    params: dict[str, str] = {}
    for key, value in (("start_ns", start_ns), ("end_ns", end_ns)):
        if value is None:
            continue
        if isinstance(value, bool) or not isinstance(value, int):
            raise InvalidInputError(f"{key} must be an integer number of nanoseconds")
        if not I64_MIN <= value <= I64_MAX:
            raise InvalidInputError(f"{key} must fit in a signed 64-bit integer")
        params[key] = str(value)
    if start_ns is not None and end_ns is not None and start_ns > end_ns:
        raise InvalidInputError(f"start_ns {start_ns} is after end_ns {end_ns}")
    return params


WIRE_HANDLE = re.compile(r"[A-Za-z0-9_-]{1,128}")
REQUEST_STATES = frozenset({"in_flight", "completed", "unknown"})


def _malformed(what: str) -> InternalError:
    return InternalError(f"DeLOG returned a malformed {what}")


def wire_object(value: Any, what: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise _malformed(what)
    return value


def wire_list(value: Any, what: str) -> list[Any]:
    if not isinstance(value, list):
        raise _malformed(what)
    return value


def wire_str(value: Any, what: str) -> str:
    if not isinstance(value, str):
        raise _malformed(what)
    return value


def wire_optional_str(value: Any, what: str) -> str | None:
    return None if value is None else wire_str(value, what)


def wire_bool(value: Any, what: str) -> bool:
    if not isinstance(value, bool):
        raise _malformed(what)
    return value


def wire_int(value: Any, what: str, low: int = I64_MIN, high: int = 2**64 - 1) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
        raise _malformed(what)
    return value


def wire_float(value: Any, what: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise _malformed(what)
    return float(value)


def wire_handle(value: Any, what: str) -> str:
    if not isinstance(value, str) or WIRE_HANDLE.fullmatch(value) is None:
        raise _malformed(what)
    return value


def wire_names(value: Any, what: str) -> tuple[str, ...]:
    return tuple(wire_str(item, what) for item in wire_list(value, what))


@dataclass(frozen=True)
class PublishedField:
    handle: str
    name: str
    arrow_type: str
    unit: str | None
    description: str | None
    multiplier: float
    publication: Publication = dataclass_field(repr=False, compare=False)


@dataclass(frozen=True)
class FieldPath:
    path: str

    @property
    def name(self) -> str:
        return self.path.rsplit("/", 1)[-1]


@dataclass(frozen=True)
class PlaybackState:
    speed: float
    follow_live: bool

    @classmethod
    def from_payload(cls, payload: Any) -> PlaybackState:
        raw = wire_object(payload, "playback state")
        return cls(
            speed=wire_float(raw.get("speed"), "playback state"),
            follow_live=wire_bool(raw.get("follow_live"), "playback state"),
        )


@dataclass(frozen=True)
class LayoutFieldIssue:
    field: str
    candidates: tuple[str, ...]


@dataclass(frozen=True)
class LayoutLoadReport:
    ambiguous: tuple[LayoutFieldIssue, ...]
    unresolved: tuple[str, ...]
    warnings: tuple[str, ...]

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> LayoutLoadReport:
        what = "layout load report"
        issues = []
        for item in wire_list(payload.get("ambiguous"), what):
            raw = wire_object(item, what)
            issues.append(
                LayoutFieldIssue(
                    field=wire_str(raw.get("field"), what),
                    candidates=wire_names(raw.get("candidates"), what),
                )
            )
        return cls(
            ambiguous=tuple(issues),
            unresolved=wire_names(payload.get("unresolved"), what),
            warnings=wire_names(payload.get("warnings"), what),
        )


@dataclass(frozen=True)
class VehicleProfile:
    name: str
    label: str
    show: bool
    show_path: bool
    position: Any
    orientation: Any
    model: str
    color: str
    path_color: str
    scale: float

    @classmethod
    def from_payload(cls, payload: Any) -> VehicleProfile:
        what = "vehicle profile"
        raw = wire_object(payload, what)
        return cls(
            name=wire_str(raw.get("name"), what),
            label=wire_str(raw.get("label"), what),
            show=wire_bool(raw.get("show"), what),
            show_path=wire_bool(raw.get("show_path"), what),
            position=raw.get("position"),
            orientation=raw.get("orientation"),
            model=wire_str(raw.get("model"), what),
            color=wire_str(raw.get("color"), what),
            path_color=wire_str(raw.get("path_color"), what),
            scale=wire_float(raw.get("scale"), what),
        )


@dataclass(frozen=True)
class RemovalReport:
    ui_resources: int | None
    publications: int
    failed: tuple[str, ...] = ()
    error: DeLOGError | None = dataclass_field(default=None, repr=False, compare=False)

    @property
    def complete(self) -> bool:
        return not self.failed and self.error is None

    @property
    def requires_user_action(self) -> bool:
        return self.error is not None and self.error.code == "forbidden"

    @classmethod
    def from_payload(cls, payload: Any) -> RemovalReport:
        what = "owner removal report"
        raw = wire_object(payload, what)
        if raw.get("kind") != "removed":
            raise _malformed(what)
        return cls(
            ui_resources=wire_int(raw.get("ui_resources"), what, 0),
            publications=wire_int(raw.get("publications"), what, 0),
        )

    @classmethod
    def from_error(cls, error: DeLOGError) -> RemovalReport | None:
        details = error.details
        if not isinstance(details, dict):
            return None
        failed = details.get("failed")
        ui = details.get("ui_resources")
        publications = details.get("publications")
        if not (
            isinstance(failed, list)
            and all(isinstance(side, str) for side in failed)
            and (ui is None or (isinstance(ui, int) and not isinstance(ui, bool)))
            and isinstance(publications, int)
            and not isinstance(publications, bool)
        ):
            return None
        return cls(
            ui_resources=ui,
            publications=publications,
            failed=tuple(failed),
            error=error,
        )


@dataclass(frozen=True)
class RequestStatus:
    request_id: str
    state: str
    status_code: int | None
    response: Any
    error: DeLOGError | None = dataclass_field(repr=False, compare=False)

    @classmethod
    def from_payload(cls, payload: Any) -> RequestStatus:
        what = "request status"
        raw = wire_object(payload, what)
        request_id = wire_str(raw.get("request_id"), what)
        state = raw.get("state")
        if state not in REQUEST_STATES:
            raise _malformed(what)
        status = raw.get("status")
        status_code = None if status is None else wire_int(status, what, 100, 599)
        response = raw.get("response")
        error: DeLOGError | None = None
        if state == "unknown":
            error = _stored_error(503, raw.get("error"))
        elif state == "completed":
            if status_code is None:
                raise _malformed(what)
            if status_code >= 400:
                error = _stored_error(status_code, response)
        return cls(request_id, str(state), status_code, response, error)

    def result(self) -> Any:
        if self.error is not None:
            raise self.error
        return self.response


def _stored_error(status: int, envelope: Any) -> DeLOGError:
    return from_envelope(status, json.dumps(envelope).encode())


def finite_number(value: Any, what: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise InvalidInputError(f"{what} must be a number")
    number = float(value)
    if not math.isfinite(number):
        raise InvalidInputError(f"{what} must be finite")
    return number


def nanoseconds(value: Any, what: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise InvalidInputError(f"{what} must be an integer number of nanoseconds")
    if not I64_MIN <= value <= I64_MAX:
        raise InvalidInputError(f"{what} must fit in a signed 64-bit integer")
    return value
