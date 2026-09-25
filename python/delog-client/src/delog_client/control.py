from __future__ import annotations

import enum
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from types import TracebackType
from typing import TYPE_CHECKING, Any, ClassVar, TypeAlias

from .errors import InternalError, InvalidInputError, StaleHandleError
from .models import (
    Field,
    FieldPath,
    LayoutLoadReport,
    PlaybackState,
    PublishedField,
    Source,
    VehicleProfile,
    finite_number,
    nanoseconds,
    wire_bool,
    wire_float,
    wire_handle,
    wire_int,
    wire_list,
    wire_names,
    wire_object,
    wire_optional_str,
    wire_str,
)
from .publication import Publication
from .transport import checked_idempotency_key

if TYPE_CHECKING:
    from .client import Client

FieldRef: TypeAlias = Field | PublishedField
TraceField: TypeAlias = Field | PublishedField | FieldPath
Point: TypeAlias = tuple[int, float]
SourceRef: TypeAlias = Publication | Source

BATCHABLE = frozenset(
    {
        "workspace_equalize",
        "scene_set_visible",
        "playback_set",
        "marker_set",
        "marker_remove",
        "vehicle_remove",
    }
)

DEFAULT_VEHICLE_COLOR = "#5AAAFFFF"
DEFAULT_PATH_COLOR = "#FFAA3CFF"


class _Unset(enum.Enum):
    TOKEN = 0


UNSET = _Unset.TOKEN


def _result(payload: Any, kind: str) -> dict[str, Any]:
    raw = wire_object(payload, "control result")
    if raw.get("kind") != kind:
        raise InternalError(
            f"DeLOG returned a {raw.get('kind')!r} result where {kind!r} was expected"
        )
    return raw


def _resource(payload: Any) -> str:
    return wire_handle(_result(payload, "resource").get("handle"), "control result")


def _optional_report(payload: Any) -> LayoutLoadReport | None:
    raw = wire_object(payload, "control result")
    if raw.get("kind") == "unit":
        return None
    return LayoutLoadReport.from_payload(_result(raw, "load_report"))


def _text(value: Any, what: str) -> str:
    if not isinstance(value, str):
        raise InvalidInputError(f"{what} must be a string")
    return value


def _flag(value: Any, what: str) -> bool:
    if not isinstance(value, bool):
        raise InvalidInputError(f"{what} must be True or False")
    return value


def _put(command: dict[str, Any], key: str, value: Any) -> None:
    if value is not None:
        command[key] = value


def _optional_text(command: dict[str, Any], key: str, value: Any) -> None:
    if value is not None:
        command[key] = _text(value, key)


def _optional_number(command: dict[str, Any], key: str, value: Any) -> None:
    if value is not None:
        command[key] = finite_number(value, key)


def _point(value: Any, what: str) -> dict[str, Any]:
    if not isinstance(value, Sequence) or isinstance(value, str) or len(value) != 2:
        raise InvalidInputError(f"{what} must be a (time_ns, y) pair")
    return {
        "time_ns": nanoseconds(value[0], f"{what} time_ns"),
        "y": finite_number(value[1], f"{what} y"),
    }


def _field_handle(client: Client, field: Any, what: str = "field") -> str:
    if isinstance(field, PublishedField):
        publication = field.publication
        if publication._client is not client:
            raise InvalidInputError(f"{what} {field.name!r} was published by a different client")
        publication._live()
        return field.handle
    if isinstance(field, Field):
        snapshot = field.topic._snapshot
        if snapshot.client is not client:
            raise InvalidInputError(f"{what} {field.name!r} belongs to a different client")
        if snapshot.closed:
            raise StaleHandleError(
                f"{what} {field.name!r} belongs to a closed snapshot; look it up again"
            )
        return field.handle
    raise InvalidInputError(f"{what} must be a snapshot Field or a published field")


def _source_handle(client: Client, source: Any) -> str:
    if isinstance(source, Publication):
        if source._client is not client:
            raise InvalidInputError("the vehicle source was published by a different client")
        return source._live()
    if isinstance(source, Source):
        snapshots = {topic._snapshot for topic in source.topics}
        if any(snapshot.closed for snapshot in snapshots):
            raise StaleHandleError(
                f"source {source.label!r} belongs to a closed snapshot; look it up again"
            )
        return source.handle
    raise InvalidInputError("source must be a Publication or a snapshot Source")


def _field_source(field: Any) -> str:
    if isinstance(field, PublishedField):
        return field.publication._live()
    if isinstance(field, Field):
        return field.topic.source.handle
    raise InvalidInputError("vehicle fields must be snapshot Fields or published fields")


def _style(
    color: str | None,
    stroke_px: float | None,
    fill_opacity: float | None,
    font_px: float | None,
    arrow: bool | None,
) -> dict[str, Any]:
    style: dict[str, Any] = {}
    _optional_text(style, "color", color)
    _optional_number(style, "stroke_px", stroke_px)
    _optional_number(style, "fill_opacity", fill_opacity)
    _optional_number(style, "font_px", font_px)
    if arrow is not None:
        style["arrow"] = _flag(arrow, "arrow")
    return style


class _Resource:
    __slots__ = ("_client", "_handle", "_stale", "__weakref__")
    _kind: ClassVar[str] = "resource"

    def __init__(self, client: Client, handle: str) -> None:
        self._client = client
        self._handle = handle
        self._stale = False
        client._track(self)

    @property
    def handle(self) -> str:
        return self._handle

    @property
    def stale(self) -> bool:
        return self._stale

    def __repr__(self) -> str:
        return f"{type(self).__name__}(handle={self._handle!r}, stale={self._stale!r})"

    def _live(self) -> str:
        self._client._ensure_usable()
        if self._stale:
            raise StaleHandleError(f"this {self._kind} no longer exists; look it up again")
        return self._handle

    def _invalidate(self) -> None:
        self._stale = True


class Window(_Resource):
    __slots__ = ("_title", "_owner")
    _kind = "window"

    def __init__(
        self, client: Client, handle: str, title: str | None, owner: str | None = None
    ) -> None:
        super().__init__(client, handle)
        self._title = title
        self._owner = owner

    @property
    def title(self) -> str | None:
        return self._title

    @property
    def owner(self) -> str | None:
        return self._owner

    @property
    def workspace(self) -> Workspace:
        return Workspace(self._client, self)


def _plot_result(client: Client, payload: Any, known: Window | None) -> Plot:
    raw = _result(payload, "resource")
    handle = wire_handle(raw.get("handle"), "control result")
    returned = raw.get("window")
    if returned is None:
        return Plot(client, handle, None)
    window_handle = wire_handle(returned, "control result")
    if known is not None and known.handle == window_handle:
        return Plot(client, handle, known)
    return Plot(client, handle, Window(client, window_handle, None), window_handle=window_handle)


class Workspace:
    __slots__ = ("_client", "_window")

    def __init__(self, client: Client, window: Window | None = None) -> None:
        self._client = client
        self._window = window

    @property
    def window(self) -> Window | None:
        return self._window

    def add_plot(self, direction: str = "vertical", *, idempotency_key: str | None = None) -> Plot:
        command: dict[str, Any] = {"op": "workspace_add_plot"}
        if self._window is not None:
            command["window"] = self._window._live()
        command["direction"] = _text(direction, "direction")
        payload = self._client._mutate(command, idempotency_key=idempotency_key)
        return _plot_result(self._client, payload, self._window)

    def plots(self) -> list[Plot]:
        state = self._client.state()
        if self._window is None:
            return list(state.plots)
        return [p for p in state.plots if p._window_handle == self._window.handle]

    def equalize(self, *, idempotency_key: str | None = None) -> None:
        command: dict[str, Any] = {"op": "workspace_equalize"}
        if self._window is not None:
            command["window"] = self._window._live()
        self._client._submit(command, idempotency_key=idempotency_key)

    def set_scene_visible(self, visible: bool, *, idempotency_key: str | None = None) -> None:
        command = {"op": "scene_set_visible", "visible": _flag(visible, "visible")}
        self._client._submit(command, idempotency_key=idempotency_key)


class Plot(_Resource):
    __slots__ = ("_window", "_window_handle", "_label", "_owner")
    _kind = "plot"

    def __init__(
        self,
        client: Client,
        handle: str,
        window: Window | None,
        label: str | None = None,
        owner: str | None = None,
        window_handle: str | None = None,
    ) -> None:
        super().__init__(client, handle)
        self._window = window
        self._window_handle = window.handle if window is not None else window_handle
        self._label = label
        self._owner = owner

    @property
    def window(self) -> Window | None:
        return self._window

    @property
    def label(self) -> str | None:
        return self._label

    @property
    def owner(self) -> str | None:
        return self._owner

    @property
    def traces(self) -> TraceCollection:
        return TraceCollection(self)

    @property
    def annotations(self) -> AnnotationCollection:
        return AnnotationCollection(self)

    def split(self, direction: str = "vertical", *, idempotency_key: str | None = None) -> Plot:
        command = {
            "op": "workspace_split",
            "plot": self._live(),
            "direction": _text(direction, "direction"),
        }
        payload = self._client._mutate(command, idempotency_key=idempotency_key)
        return _plot_result(self._client, payload, self._window)

    def close(self, *, idempotency_key: str | None = None) -> None:
        handle = self._live()
        self._client._mutate(
            {"op": "workspace_close", "plot": handle}, idempotency_key=idempotency_key
        )
        self._client._invalidate(
            lambda r: (
                r._handle == handle
                or (isinstance(r, (Trace, Annotation)) and r._plot_handle == handle)
            )
        )


class TraceCollection:
    __slots__ = ("_plot",)

    def __init__(self, plot: Plot) -> None:
        self._plot = plot

    def add(
        self,
        field: FieldRef,
        *,
        mode: str = "line",
        color: str | None = None,
        width_px: float | None = None,
        idempotency_key: str | None = None,
    ) -> Trace:
        client = self._plot._client
        command: dict[str, Any] = {
            "op": "trace_add",
            "plot": self._plot._live(),
            "field": _field_handle(client, field),
        }
        _optional_text(command, "color", color)
        _optional_number(command, "width_px", width_px)
        command["mode"] = _text(mode, "mode")
        handle = _resource(client._mutate(command, idempotency_key=idempotency_key))
        return Trace(client, handle, self._plot, field, mode=mode, color=color, width_px=width_px)

    def clear(self, *, idempotency_key: str | None = None) -> None:
        plot = self._plot._live()
        self._plot._client._mutate(
            {"op": "trace_clear", "plot": plot}, idempotency_key=idempotency_key
        )
        self._plot._client._invalidate(lambda r: isinstance(r, Trace) and r._plot_handle == plot)

    def list(self) -> list[Trace]:
        plot = self._plot._live()
        return [t for t in self._plot._client.state().traces if t._plot_handle == plot]


class Trace(_Resource):
    __slots__ = (
        "_plot",
        "_plot_handle",
        "_field",
        "_mode",
        "_color",
        "_width_px",
        "_visible",
        "_owner",
    )
    _kind = "trace"

    def __init__(
        self,
        client: Client,
        handle: str,
        plot: Plot,
        field: TraceField,
        *,
        mode: str,
        color: str | None = None,
        width_px: float | None = None,
        visible: bool = True,
        owner: str | None = None,
    ) -> None:
        super().__init__(client, handle)
        self._plot = plot
        self._plot_handle = plot.handle
        self._field = field
        self._mode = mode
        self._color = color
        self._width_px = width_px
        self._visible = visible
        self._owner = owner

    @property
    def plot(self) -> Plot:
        return self._plot

    @property
    def field(self) -> TraceField:
        return self._field

    @property
    def mode(self) -> str:
        return self._mode

    @property
    def color(self) -> str | None:
        return self._color

    @property
    def width_px(self) -> float | None:
        return self._width_px

    @property
    def visible(self) -> bool:
        return self._visible

    @property
    def owner(self) -> str | None:
        return self._owner

    def set(
        self,
        *,
        color: str | None = None,
        width_px: float | None = None,
        mode: str | None = None,
        visible: bool | None = None,
        idempotency_key: str | None = None,
    ) -> None:
        command: dict[str, Any] = {"op": "trace_set", "trace": self._live()}
        _optional_text(command, "color", color)
        _optional_number(command, "width_px", width_px)
        _optional_text(command, "mode", mode)
        if visible is not None:
            command["visible"] = _flag(visible, "visible")
        self._client._mutate(command, idempotency_key=idempotency_key)
        self._color = color if color is not None else self._color
        self._width_px = width_px if width_px is not None else self._width_px
        self._mode = mode if mode is not None else self._mode
        self._visible = visible if visible is not None else self._visible

    def remove(self, *, idempotency_key: str | None = None) -> None:
        command = {"op": "trace_remove", "trace": self._live()}
        self._client._mutate(command, idempotency_key=idempotency_key)
        plot = self._plot_handle
        self._client._invalidate(lambda r: isinstance(r, Trace) and r._plot_handle == plot)


GEOMETRY_POINTS = {
    "text": ("at",),
    "segment": ("from", "to"),
    "rect": ("a", "b"),
    "ellipse": ("a", "b"),
}


def _geometry(kind: str, values: Sequence[Any]) -> dict[str, Any]:
    if kind == "h_line":
        if len(values) != 1:
            raise InvalidInputError("a horizontal line geometry is a single y value")
        return {"kind": "h_line", "y": finite_number(values[0], "y")}
    names = GEOMETRY_POINTS.get(kind)
    if names is None:
        raise InvalidInputError(f"unknown annotation geometry {kind!r}")
    if len(values) != len(names):
        raise InvalidInputError(f"a {kind} geometry needs {len(names)} (time_ns, y) point(s)")
    geometry: dict[str, Any] = {"kind": kind}
    for name, value in zip(names, values, strict=True):
        geometry[name] = _point(value, name)
    return geometry


class AnnotationCollection:
    __slots__ = ("_plot",)

    def __init__(self, plot: Plot) -> None:
        self._plot = plot

    def add_text(
        self,
        time_ns: int,
        value: float,
        text: str,
        *,
        color: str | None = None,
        font_px: float | None = None,
        idempotency_key: str | None = None,
    ) -> Annotation:
        return self._add(
            _geometry("text", [(time_ns, value)]),
            text,
            _style(color, None, None, font_px, None),
            idempotency_key,
        )

    def add_segment(
        self,
        start: Point,
        end: Point,
        *,
        label: str = "",
        color: str | None = None,
        stroke_px: float | None = None,
        arrow: bool | None = None,
        idempotency_key: str | None = None,
    ) -> Annotation:
        return self._add(
            _geometry("segment", [start, end]),
            label,
            _style(color, stroke_px, None, None, arrow),
            idempotency_key,
        )

    def add_rect(
        self,
        a: Point,
        b: Point,
        *,
        label: str = "",
        color: str | None = None,
        stroke_px: float | None = None,
        fill_opacity: float | None = None,
        idempotency_key: str | None = None,
    ) -> Annotation:
        return self._add(
            _geometry("rect", [a, b]),
            label,
            _style(color, stroke_px, fill_opacity, None, None),
            idempotency_key,
        )

    def add_ellipse(
        self,
        a: Point,
        b: Point,
        *,
        label: str = "",
        color: str | None = None,
        stroke_px: float | None = None,
        fill_opacity: float | None = None,
        idempotency_key: str | None = None,
    ) -> Annotation:
        return self._add(
            _geometry("ellipse", [a, b]),
            label,
            _style(color, stroke_px, fill_opacity, None, None),
            idempotency_key,
        )

    def add_hline(
        self,
        y: float,
        *,
        label: str = "",
        color: str | None = None,
        stroke_px: float | None = None,
        idempotency_key: str | None = None,
    ) -> Annotation:
        return self._add(
            _geometry("h_line", [y]),
            label,
            _style(color, stroke_px, None, None, None),
            idempotency_key,
        )

    def list(self) -> list[Annotation]:
        plot = self._plot._live()
        return [a for a in self._plot._client.state().annotations if a._plot_handle == plot]

    def _add(
        self,
        geometry: dict[str, Any],
        label: str,
        style: dict[str, Any],
        idempotency_key: str | None,
    ) -> Annotation:
        client = self._plot._client
        command: dict[str, Any] = {
            "op": "annotation_add",
            "plot": self._plot._live(),
            "geometry": geometry,
            "label": _text(label, "label"),
        }
        if style:
            command["style"] = style
        handle = _resource(client._mutate(command, idempotency_key=idempotency_key))
        return Annotation(client, handle, self._plot, geometry, label, style.get("color"))


class Annotation(_Resource):
    __slots__ = ("_plot", "_plot_handle", "_geometry", "_label", "_color", "_owner")
    _kind = "annotation"

    def __init__(
        self,
        client: Client,
        handle: str,
        plot: Plot,
        geometry: dict[str, Any],
        label: str,
        color: str | None = None,
        owner: str | None = None,
    ) -> None:
        super().__init__(client, handle)
        self._plot = plot
        self._plot_handle = plot.handle
        self._geometry = geometry
        self._label = label
        self._color = color
        self._owner = owner

    @property
    def plot(self) -> Plot:
        return self._plot

    @property
    def kind(self) -> str:
        return str(self._geometry.get("kind"))

    @property
    def geometry(self) -> dict[str, Any]:
        return dict(self._geometry)

    @property
    def label(self) -> str:
        return self._label

    @property
    def color(self) -> str | None:
        return self._color

    @property
    def owner(self) -> str | None:
        return self._owner

    def set(
        self,
        *,
        label: str | None = None,
        geometry: Sequence[Any] | None = None,
        color: str | None = None,
        stroke_px: float | None = None,
        fill_opacity: float | None = None,
        font_px: float | None = None,
        arrow: bool | None = None,
        idempotency_key: str | None = None,
    ) -> None:
        command: dict[str, Any] = {"op": "annotation_set", "annotation": self._live()}
        _optional_text(command, "label", label)
        new_geometry = None if geometry is None else _geometry(self.kind, geometry)
        _put(command, "geometry", new_geometry)
        style = _style(color, stroke_px, fill_opacity, font_px, arrow)
        if style:
            command["style"] = style
        self._client._mutate(command, idempotency_key=idempotency_key)
        self._label = label if label is not None else self._label
        self._geometry = new_geometry if new_geometry is not None else self._geometry
        self._color = color if color is not None else self._color

    def remove(self, *, idempotency_key: str | None = None) -> None:
        handle = self._live()
        self._client._mutate(
            {"op": "annotation_remove", "annotation": handle}, idempotency_key=idempotency_key
        )
        self._client._invalidate(lambda r: r._handle == handle)


class Marker(_Resource):
    __slots__ = ("_time_ns", "_label", "_color", "_note", "_origin", "_owner")
    _kind = "marker"

    def __init__(
        self,
        client: Client,
        handle: str,
        time_ns: int,
        label: str,
        color: str | None = None,
        note: str | None = None,
        origin: str | None = None,
        owner: str | None = None,
    ) -> None:
        super().__init__(client, handle)
        self._time_ns = time_ns
        self._label = label
        self._color = color
        self._note = note
        self._origin = origin
        self._owner = owner

    @property
    def time_ns(self) -> int:
        return self._time_ns

    @property
    def label(self) -> str:
        return self._label

    @property
    def color(self) -> str | None:
        return self._color

    @property
    def note(self) -> str | None:
        return self._note

    @property
    def origin(self) -> str | None:
        return self._origin

    @property
    def owner(self) -> str | None:
        return self._owner

    def set(
        self,
        *,
        time_ns: int | None = None,
        label: str | None = None,
        color: str | None = None,
        note: str | None = None,
        idempotency_key: str | None = None,
    ) -> None:
        command: dict[str, Any] = {"op": "marker_set", "marker": self._live()}
        if time_ns is not None:
            command["time_ns"] = nanoseconds(time_ns, "time_ns")
        _optional_text(command, "label", label)
        _optional_text(command, "color", color)
        _optional_text(command, "note", note)

        def applied() -> None:
            self._time_ns = time_ns if time_ns is not None else self._time_ns
            self._label = label if label is not None else self._label
            self._color = color if color is not None else self._color
            self._note = note if note is not None else self._note

        self._client._submit(command, idempotency_key=idempotency_key, on_commit=applied)

    def remove(self, *, idempotency_key: str | None = None) -> None:
        handle = self._live()
        self._client._submit(
            {"op": "marker_remove", "marker": handle},
            idempotency_key=idempotency_key,
            on_commit=lambda: self._client._invalidate(lambda r: r._handle == handle),
        )


class MarkerCollection:
    __slots__ = ("_client",)

    def __init__(self, client: Client) -> None:
        self._client = client

    def add(
        self,
        time_ns: int,
        label: str = "",
        *,
        color: str | None = None,
        note: str | None = None,
        idempotency_key: str | None = None,
    ) -> Marker:
        command: dict[str, Any] = {
            "op": "marker_add",
            "time_ns": nanoseconds(time_ns, "time_ns"),
            "label": _text(label, "label"),
        }
        _optional_text(command, "color", color)
        _optional_text(command, "note", note)
        handle = _resource(self._client._mutate(command, idempotency_key=idempotency_key))
        return Marker(self._client, handle, time_ns, label, color, note)

    def list(self) -> list[Marker]:
        return list(self._client.state().markers)


def _vehicle_fields(value: Any, names: Sequence[str], what: str) -> list[Any]:
    if not isinstance(value, Mapping):
        raise InvalidInputError(f"{what} must be a mapping")
    missing = [name for name in names if name not in value]
    if missing:
        raise InvalidInputError(f"{what} is missing {', '.join(missing)}")
    return [value[name] for name in names]


def _position(client: Client, value: Any) -> tuple[dict[str, Any], Any]:
    if not isinstance(value, Mapping):
        raise InvalidInputError("position must be a mapping of field handles")
    keys = set(value)
    if {"lat", "lon", "alt"} <= keys:
        allowed = {"lat", "lon", "alt", "lat_lon_dege7", "alt_mm", "alt_offset_m"}
        if keys - allowed:
            raise InvalidInputError(f"unknown GPS position keys: {sorted(keys - allowed)}")
        lat, lon, alt = _vehicle_fields(value, ("lat", "lon", "alt"), "a GPS position")
        return {
            "kind": "gps",
            "lat": _field_handle(client, lat, "lat"),
            "lon": _field_handle(client, lon, "lon"),
            "alt": _field_handle(client, alt, "alt"),
            "lat_lon_dege7": _flag(value.get("lat_lon_dege7", False), "lat_lon_dege7"),
            "alt_mm": _flag(value.get("alt_mm", False), "alt_mm"),
            "alt_offset_m": finite_number(value.get("alt_offset_m", 0.0), "alt_offset_m"),
        }, lat
    if {"north", "east", "down"} <= keys:
        allowed = {"north", "east", "down", "reference"}
        if keys - allowed:
            raise InvalidInputError(f"unknown NED position keys: {sorted(keys - allowed)}")
        north, east, down = _vehicle_fields(value, ("north", "east", "down"), "an NED position")
        position: dict[str, Any] = {
            "kind": "ned",
            "north": _field_handle(client, north, "north"),
            "east": _field_handle(client, east, "east"),
            "down": _field_handle(client, down, "down"),
        }
        reference = value.get("reference")
        if reference is not None:
            position["reference"] = _reference(client, reference)
        return position, north
    raise InvalidInputError(
        "position needs lat/lon/alt (GPS) or north/east/down (NED) field handles"
    )


def _reference(client: Client, value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise InvalidInputError("an NED reference must be a mapping")
    if set(value) == {"lat_deg", "lon_deg", "alt_m"}:
        return {
            "kind": "manual",
            "lat_deg": finite_number(value["lat_deg"], "lat_deg"),
            "lon_deg": finite_number(value["lon_deg"], "lon_deg"),
            "alt_m": finite_number(value["alt_m"], "alt_m"),
        }
    if set(value) == {"lat", "lon", "alt"}:
        return {
            "kind": "fields",
            "lat": _field_handle(client, value["lat"], "lat"),
            "lon": _field_handle(client, value["lon"], "lon"),
            "alt": _field_handle(client, value["alt"], "alt"),
        }
    raise InvalidInputError(
        "an NED reference needs lat_deg/lon_deg/alt_m values or lat/lon/alt field handles"
    )


def _orientation(client: Client, value: Any) -> dict[str, Any]:
    if value is None:
        return {"kind": "static"}
    if not isinstance(value, Mapping):
        raise InvalidInputError("orientation must be None or a mapping")
    keys = set(value)
    if keys == {"quaternion"}:
        parts = value["quaternion"]
        if not isinstance(parts, Sequence) or len(parts) != 4:
            raise InvalidInputError("quaternion must list the w, x, y and z fields")
        return {
            "kind": "quat",
            **{
                name: _field_handle(client, part, name)
                for name, part in zip(("w", "x", "y", "z"), parts, strict=True)
            },
        }
    if keys in ({"euler"}, {"euler", "degrees"}):
        parts = value["euler"]
        if not isinstance(parts, Sequence) or len(parts) != 3:
            raise InvalidInputError("euler must list the roll, pitch and yaw fields")
        return {
            "kind": "euler",
            **{
                name: _field_handle(client, part, name)
                for name, part in zip(("roll", "pitch", "yaw"), parts, strict=True)
            },
            "degrees": _flag(value.get("degrees", False), "degrees"),
        }
    raise InvalidInputError("orientation needs a 'quaternion' or an 'euler' entry")


class Vehicle(_Resource):
    __slots__ = ("_label", "_model", "_source", "_owner")
    _kind = "vehicle"

    def __init__(
        self,
        client: Client,
        handle: str,
        label: str,
        model: str | None = None,
        source: str | None = None,
        owner: str | None = None,
    ) -> None:
        super().__init__(client, handle)
        self._label = label
        self._model = model
        self._source = source
        self._owner = owner

    @property
    def label(self) -> str:
        return self._label

    @property
    def model(self) -> str | None:
        return self._model

    @property
    def source(self) -> str | None:
        return self._source

    @property
    def owner(self) -> str | None:
        return self._owner

    def set(
        self,
        *,
        label: str | None = None,
        show: bool | None = None,
        show_path: bool | None = None,
        position: Mapping[str, Any] | None = None,
        orientation: Mapping[str, Any] | None | _Unset = UNSET,
        model: str | None = None,
        color: str | None = None,
        path_color: str | None = None,
        scale: float | None = None,
        idempotency_key: str | None = None,
    ) -> Vehicle:
        handle = self._live()
        patch: dict[str, Any] = {}
        _optional_text(patch, "label", label)
        if show is not None:
            patch["show"] = _flag(show, "show")
        if show_path is not None:
            patch["show_path"] = _flag(show_path, "show_path")
        if position is not None:
            patch["position"] = _position(self._client, position)[0]
        if orientation is not UNSET:
            patch["orientation"] = _orientation(self._client, orientation)
        _optional_text(patch, "model", model)
        _optional_text(patch, "color", color)
        _optional_text(patch, "path_color", path_color)
        _optional_number(patch, "scale", scale)
        command = {"op": "vehicle_set", "vehicle": handle, "patch": patch}
        self._handle = _resource(self._client._mutate(command, idempotency_key=idempotency_key))
        self._label = label if label is not None else self._label
        self._model = model if model is not None else self._model
        return self

    def save_profile(self, name: str, *, idempotency_key: str | None = None) -> None:
        self._client.vehicle_profiles.save(name, self, idempotency_key=idempotency_key)

    def remove(self, *, idempotency_key: str | None = None) -> None:
        handle = self._live()
        self._client._submit(
            {"op": "vehicle_remove", "vehicle": handle},
            idempotency_key=idempotency_key,
            on_commit=lambda: self._client._invalidate(lambda r: r._handle == handle),
        )


class VehicleCollection:
    __slots__ = ("_client",)

    def __init__(self, client: Client) -> None:
        self._client = client

    def add(
        self,
        *,
        position: Mapping[str, Any],
        orientation: Mapping[str, Any] | None = None,
        source: SourceRef | None = None,
        label: str = "Vehicle",
        model: str = "quad",
        color: str = DEFAULT_VEHICLE_COLOR,
        path_color: str = DEFAULT_PATH_COLOR,
        scale: float = 1.0,
        show: bool = True,
        show_path: bool = True,
        idempotency_key: str | None = None,
    ) -> Vehicle:
        client = self._client
        client._ensure_usable()
        wire_position, first = _position(client, position)
        source_handle = _field_source(first) if source is None else _source_handle(client, source)
        command = {
            "op": "vehicle_add",
            "source": source_handle,
            "label": _text(label, "label"),
            "show": _flag(show, "show"),
            "show_path": _flag(show_path, "show_path"),
            "position": wire_position,
            "orientation": _orientation(client, orientation),
            "model": _text(model, "model"),
            "color": _text(color, "color"),
            "path_color": _text(path_color, "path_color"),
            "scale": finite_number(scale, "scale"),
        }
        handle = _resource(client._mutate(command, idempotency_key=idempotency_key))
        return Vehicle(client, handle, label, model)

    def list(self) -> list[Vehicle]:
        return list(self._client.state().vehicles)


class VehicleProfileCollection:
    __slots__ = ("_client",)

    def __init__(self, client: Client) -> None:
        self._client = client

    def list(self) -> list[str]:
        payload = self._client._query({"op": "vehicle_profile_list"})
        return list(wire_names(_result(payload, "names").get("names"), "profile names"))

    def load(self, name: str) -> VehicleProfile:
        payload = self._client._query({"op": "vehicle_profile_load", "name": _text(name, "name")})
        return VehicleProfile.from_payload(_result(payload, "vehicle_profile").get("profile"))

    def save(self, name: str, vehicle: Vehicle, *, idempotency_key: str | None = None) -> None:
        if not isinstance(vehicle, Vehicle):
            raise InvalidInputError("vehicle must be a Vehicle handle")
        command = {
            "op": "vehicle_profile_save",
            "name": _text(name, "name"),
            "vehicle": vehicle._live(),
        }
        self._client._mutate(command, idempotency_key=idempotency_key)

    def apply(self, name: str, source: SourceRef, *, idempotency_key: str | None = None) -> Vehicle:
        command = {
            "op": "vehicle_profile_apply",
            "name": _text(name, "name"),
            "source": _source_handle(self._client, source),
        }
        handle = _resource(self._client._mutate(command, idempotency_key=idempotency_key))
        return Vehicle(self._client, handle, name)

    def delete(self, name: str, *, idempotency_key: str | None = None) -> None:
        command = {"op": "vehicle_profile_delete", "name": _text(name, "name")}
        self._client._mutate(command, idempotency_key=idempotency_key)


class LayoutCollection:
    __slots__ = ("_client",)

    def __init__(self, client: Client) -> None:
        self._client = client

    def list(self) -> list[str]:
        payload = self._client._query({"op": "layout_list"})
        return list(wire_names(_result(payload, "names").get("names"), "layout names"))

    def current(self) -> str:
        payload = self._client._query({"op": "layout_current"})
        return wire_str(_result(payload, "layout").get("json"), "layout")

    def save(self, name: str, *, idempotency_key: str | None = None) -> None:
        self._unit({"op": "layout_save", "name": _text(name, "name")}, idempotency_key)

    def delete(self, name: str, *, idempotency_key: str | None = None) -> None:
        self._unit({"op": "layout_delete", "name": _text(name, "name")}, idempotency_key)

    def rename(self, old: str, new: str, *, idempotency_key: str | None = None) -> None:
        command = {"op": "layout_rename", "from": _text(old, "old"), "to": _text(new, "new")}
        self._unit(command, idempotency_key)

    def duplicate(self, name: str, copy: str, *, idempotency_key: str | None = None) -> None:
        command = {"op": "layout_duplicate", "from": _text(name, "name"), "to": _text(copy, "copy")}
        self._unit(command, idempotency_key)

    def export_file(self, name: str, path: str, *, idempotency_key: str | None = None) -> None:
        command = {"op": "layout_export", "name": _text(name, "name"), "path": _text(path, "path")}
        self._unit(command, idempotency_key)

    def load(self, name: str, *, idempotency_key: str | None = None) -> LayoutLoadReport | None:
        return self._replace({"op": "layout_load", "name": _text(name, "name")}, idempotency_key)

    def import_file(
        self, path: str, *, idempotency_key: str | None = None
    ) -> LayoutLoadReport | None:
        return self._replace({"op": "layout_import", "path": _text(path, "path")}, idempotency_key)

    def apply(self, json: str, *, idempotency_key: str | None = None) -> LayoutLoadReport | None:
        return self._replace({"op": "layout_apply", "json": _text(json, "json")}, idempotency_key)

    def clear(self, *, idempotency_key: str | None = None) -> LayoutLoadReport | None:
        return self._replace({"op": "layout_clear"}, idempotency_key)

    def _unit(self, command: dict[str, Any], idempotency_key: str | None) -> None:
        self._client._mutate(command, idempotency_key=idempotency_key)

    def _replace(
        self, command: dict[str, Any], idempotency_key: str | None
    ) -> LayoutLoadReport | None:
        payload = self._client._mutate(command, idempotency_key=idempotency_key)
        self._client._invalidate(lambda r: True)
        return _optional_report(payload)


class Playback:
    __slots__ = ("_client",)

    def __init__(self, client: Client) -> None:
        self._client = client

    def set(
        self,
        *,
        speed: float | None = None,
        follow_live: bool | None = None,
        idempotency_key: str | None = None,
    ) -> None:
        command: dict[str, Any] = {"op": "playback_set"}
        _optional_number(command, "speed", speed)
        if follow_live is not None:
            command["follow_live"] = _flag(follow_live, "follow_live")
        self._client._submit(command, idempotency_key=idempotency_key)

    def state(self) -> PlaybackState:
        return self._client.state().playback


class WindowCollection:
    __slots__ = ("_client",)

    def __init__(self, client: Client) -> None:
        self._client = client

    def open(self, title: str | None = None, *, idempotency_key: str | None = None) -> Window:
        command: dict[str, Any] = {"op": "window_open"}
        _optional_text(command, "title", title)
        handle = _resource(self._client._mutate(command, idempotency_key=idempotency_key))
        return Window(self._client, handle, title)

    def list(self) -> list[Window]:
        return list(self._client.state().windows)


@dataclass(frozen=True)
class ControlState:
    windows: tuple[Window, ...]
    plots: tuple[Plot, ...]
    traces: tuple[Trace, ...]
    annotations: tuple[Annotation, ...]
    markers: tuple[Marker, ...]
    vehicles: tuple[Vehicle, ...]
    layout_names: tuple[str, ...]
    current_layout: str
    playback: PlaybackState
    scene_visible: bool

    @classmethod
    def from_payload(cls, client: Client, payload: Any) -> ControlState:
        what = "control state"
        raw = wire_object(payload, what)

        def items(key: str) -> list[dict[str, Any]]:
            return [wire_object(item, what) for item in wire_list(raw.get(key), what)]

        def handle(item: dict[str, Any], key: str = "handle") -> str:
            return wire_handle(item.get(key), what)

        def owner(item: dict[str, Any]) -> str | None:
            return wire_optional_str(item.get("owner"), what)

        windows = {
            handle(item): Window(
                client, handle(item), wire_str(item.get("title"), what), owner(item)
            )
            for item in items("windows")
        }
        plots: dict[str, Plot] = {}
        for item in items("plots"):
            window_handle = handle(item, "window")
            plots[handle(item)] = Plot(
                client,
                handle(item),
                windows.get(window_handle),
                wire_str(item.get("label"), what),
                owner(item),
                window_handle=window_handle,
            )

        def plot_for(item: dict[str, Any]) -> Plot:
            key = handle(item, "plot")
            if key not in plots:
                plots[key] = Plot(client, key, None)
            return plots[key]

        traces = tuple(
            Trace(
                client,
                handle(item),
                plot_for(item),
                FieldPath(wire_str(item.get("field"), what)),
                mode=wire_str(item.get("mode"), what),
                color=wire_str(item.get("color"), what),
                width_px=wire_float(item.get("width_px"), what),
                visible=wire_bool(item.get("visible"), what),
                owner=owner(item),
            )
            for item in items("traces")
        )
        annotations = tuple(
            Annotation(
                client,
                handle(item),
                plot_for(item),
                wire_object(item.get("geometry"), what),
                wire_str(item.get("label"), what),
                wire_str(item.get("color"), what),
                owner(item),
            )
            for item in items("annotations")
        )
        markers = tuple(
            Marker(
                client,
                handle(item),
                wire_int(item.get("time_ns"), what),
                wire_str(item.get("label"), what),
                wire_str(item.get("color"), what),
                wire_str(item.get("note"), what),
                wire_str(item.get("origin"), what),
                owner(item),
            )
            for item in items("markers")
        )
        vehicles = tuple(
            Vehicle(
                client,
                handle(item),
                wire_str(item.get("label"), what),
                wire_str(item.get("model"), what),
                wire_str(item.get("source"), what),
                owner(item),
            )
            for item in items("vehicles")
        )
        return cls(
            windows=tuple(windows.values()),
            plots=tuple(plots.values()),
            traces=traces,
            annotations=annotations,
            markers=markers,
            vehicles=vehicles,
            layout_names=wire_names(raw.get("layout_names"), what),
            current_layout=wire_str(raw.get("current_layout"), what),
            playback=PlaybackState.from_payload(raw.get("playback")),
            scene_visible=wire_bool(raw.get("scene_visible"), what),
        )


class ControlBatch:
    __slots__ = ("_client", "_key", "_commands", "_callbacks", "_state")

    def __init__(self, client: Client, idempotency_key: str | None = None) -> None:
        self._client = client
        self._key = None if idempotency_key is None else checked_idempotency_key(idempotency_key)
        self._commands: list[dict[str, Any]] = []
        self._callbacks: list[Callable[[], None]] = []
        self._state = "new"

    def __len__(self) -> int:
        return len(self._commands)

    def __repr__(self) -> str:
        return f"ControlBatch(commands={len(self._commands)!r}, state={self._state!r})"

    def __enter__(self) -> ControlBatch:
        if self._state != "new":
            raise InvalidInputError("a ControlBatch can only be used once")
        self._client._begin_batch(self)
        self._state = "open"
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self._client._end_batch(self)
        commands, callbacks = self._commands, self._callbacks
        self._commands, self._callbacks = [], []
        if exc is not None:
            self._state = "discarded"
            return
        self._state = "sent"
        if not commands:
            return
        self._client._transport.post_mutation(
            "/v1/control/batch", {"commands": commands}, idempotency_key=self._key
        )
        for callback in callbacks:
            callback()

    def _add(self, command: dict[str, Any], on_commit: Callable[[], None] | None) -> None:
        if command.get("op") not in BATCHABLE:
            raise InvalidInputError(f"{command.get('op')} cannot be batched")
        self._commands.append(command)
        if on_commit is not None:
            self._callbacks.append(on_commit)
