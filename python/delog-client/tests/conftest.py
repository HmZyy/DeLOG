from __future__ import annotations

import base64
import io
import json
import os
import secrets
import subprocess
import sys
from collections.abc import Callable, Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import httpx
import pyarrow as pa
import pytest

import delog_client.transport as transport_module
from delog_client import Client, DeLOG

ARROW = "application/vnd.apache.arrow.stream"


def make_token() -> str:
    return base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode()


def make_id() -> str:
    return base64.urlsafe_b64encode(secrets.token_bytes(16)).rstrip(b"=").decode()


def dead_pid() -> int:
    child = subprocess.Popen([sys.executable, "-c", "pass"])
    child.wait()
    return child.pid


def arrow_bytes(batches: list[pa.RecordBatch], schema: pa.Schema) -> bytes:
    sink = io.BytesIO()
    with pa.ipc.new_stream(sink, schema) as writer:
        for batch in batches:
            writer.write_batch(batch)
    return sink.getvalue()


def split(data: bytes, size: int) -> list[bytes]:
    return [data[i : i + size] for i in range(0, len(data), size)]


def attitude_schema() -> pa.Schema:
    columns: list[pa.Field[Any]] = [
        pa.field("__delog_time_ns", pa.int64(), nullable=False),
        pa.field("__source_time_ns", pa.int64(), nullable=False),
        pa.field("roll", pa.float32()),
        pa.field("pitch", pa.float32()),
    ]
    return pa.schema(columns)


def attitude_batches(count: int = 3, rows: int = 4) -> list[pa.RecordBatch]:
    schema = attitude_schema()
    batches = []
    for index in range(count):
        base = index * rows
        batches.append(
            pa.record_batch(
                [
                    pa.array([(base + r) * 1000 for r in range(rows)], pa.int64()),
                    pa.array([(base + r) * 1000 - 5 for r in range(rows)], pa.int64()),
                    pa.array([float(base + r) for r in range(rows)], pa.float32()),
                    pa.array([float(-(base + r)) for r in range(rows)], pa.float32()),
                ],
                schema=schema,
            )
        )
    return batches


class TrackingStream(httpx.SyncByteStream):
    def __init__(self, chunks: list[bytes], fail_after: int | None = None) -> None:
        self.chunks = chunks
        self.fail_after = fail_after
        self.closed = False
        self.yielded = 0

    def __iter__(self) -> Iterator[bytes]:
        for index, chunk in enumerate(self.chunks):
            if self.fail_after is not None and index >= self.fail_after:
                raise httpx.RemoteProtocolError("peer closed connection")
            self.yielded += 1
            yield chunk

    def close(self) -> None:
        self.closed = True


def field_dto(source: str, topic: str, instance: int | None, name: str) -> dict[str, Any]:
    return {
        "handle": make_id(),
        "name": name,
        "selector": {"source": source, "topic": topic, "instance": instance, "field": name},
        "arrow_type": "float32",
        "unit": "rad",
        "description": f"{name} angle",
        "multiplier": 1.0,
    }


def topic_dto(
    source: str,
    name: str,
    base_name: str,
    instance: int | None,
    fields: list[str],
) -> dict[str, Any]:
    return {
        "handle": make_id(),
        "name": name,
        "base_name": base_name,
        "instance": instance,
        "row_count": 12,
        "time_range_ns": {"start_ns": 0, "end_ns": 11000},
        "fields": [field_dto(source, name, instance, f) for f in fields],
    }


def default_catalog() -> dict[str, Any]:
    return {
        "snapshot_epoch": 7,
        "sources": [
            {
                "handle": make_id(),
                "label": "simulation",
                "kind": "file",
                "offset_ns": 0,
                "topics": [
                    topic_dto(
                        "simulation",
                        "vehicle_attitude",
                        "vehicle_attitude",
                        None,
                        ["roll", "pitch"],
                    ),
                ],
            },
            {
                "handle": make_id(),
                "label": "flight",
                "kind": "file",
                "offset_ns": -2000,
                "topics": [
                    topic_dto(
                        "flight",
                        "vehicle_attitude",
                        "vehicle_attitude",
                        None,
                        ["roll", "pitch", "yaw"],
                    ),
                    topic_dto("flight", "sensor_accel[0]", "sensor_accel", 0, ["x", "y"]),
                    topic_dto("flight", "sensor_accel[1]", "sensor_accel", 1, ["x", "y"]),
                ],
            },
        ],
    }


@dataclass
class FakeInstance:
    port: int
    instance_id: str = field(default_factory=make_id)
    label: str = "DeLOG"
    bootstrap: str = field(default_factory=make_token)
    api_major: int = 1
    api_min_minor: int = 0
    api_max_minor: int = 0
    session_description: str | None = "flight.ulg"
    reported_instance_id: str | None = None
    reported_api_major: int | None = None
    reachable: bool = True
    catalog: dict[str, Any] = field(default_factory=default_catalog)
    client_tokens: dict[str, str] = field(default_factory=dict)
    leases: set[str] = field(default_factory=set)
    requests: list[httpx.Request] = field(default_factory=list)
    streams: list[TrackingStream] = field(default_factory=list)
    data_chunks: list[bytes] | None = None
    data_fail_after: int | None = None
    data_content_type: str = ARROW
    overrides: dict[tuple[str, str], httpx.Response] = field(default_factory=dict)
    connected_names: set[str] = field(default_factory=set)
    control: FakeControl | None = None

    @property
    def endpoint(self) -> str:
        return f"127.0.0.1:{self.port}"

    def descriptor(self, pid: int | None = None) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "format": 1,
            "instance_id": self.instance_id,
            "label": self.label,
            "pid": os.getpid() if pid is None else pid,
            "endpoint": self.endpoint,
            "api_major": self.api_major,
            "api_min_minor": self.api_min_minor,
            "api_max_minor": self.api_max_minor,
            "bootstrap_token": self.bootstrap,
        }
        if self.session_description is not None:
            payload["session_description"] = self.session_description
        return payload

    def error(self, status: int, code: str, message: str, **extra: Any) -> httpx.Response:
        body = {
            "request_id": make_id(),
            "code": code,
            "message": message,
            "retryable": code == "unavailable",
        }
        body.update(extra)
        return httpx.Response(status, json=body)

    def bearer(self, request: httpx.Request) -> str | None:
        values = request.headers.get_list("authorization")
        if len(values) != 1 or not values[0].startswith("Bearer "):
            return None
        return values[0][len("Bearer ") :]

    def handle(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        if not self.reachable:
            raise httpx.ConnectError("connection refused", request=request)
        if "origin" in request.headers or request.headers.get_list("host") != [self.endpoint]:
            return self.error(403, "forbidden", "request is not from an allowed loopback client")
        token = self.bearer(request)
        method = request.method
        path = request.url.path
        override = self.overrides.get((method, path))
        if override is not None:
            return override
        parts = path.strip("/").split("/")
        if method == "POST" and parts == ["v1", "clients"]:
            if token != self.bootstrap:
                return self.error(403, "forbidden", "bootstrap authentication required")
            body = json.loads(request.content)
            if set(body) - {"name", "takeover"}:
                return self.error(400, "invalid_input", "invalid registration JSON")
            if body["name"] in self.connected_names and not body.get("takeover", False):
                return self.error(409, "conflict", "client name is already connected")
            self.connected_names.add(body["name"])
            client_id = make_id()
            client_token = make_token()
            self.client_tokens[client_token] = client_id
            return httpx.Response(
                200,
                json={
                    "client_id": client_id,
                    "owner_id": make_id(),
                    "owner_name": body["name"],
                    "token": client_token,
                },
            )
        if parts == ["v1", "instance"]:
            if token != self.bootstrap and token not in self.client_tokens:
                return self.error(403, "forbidden", "a valid bearer token is required")
            payload: dict[str, Any] = {
                "instance_id": self.reported_instance_id or self.instance_id,
                "label": self.label,
                "api_major": self.reported_api_major or self.api_major,
                "api_min_minor": self.api_min_minor,
                "api_max_minor": self.api_max_minor,
                "future_field": True,
            }
            if self.session_description is not None:
                payload["session_description"] = self.session_description
            return httpx.Response(200, json=payload)
        if token not in self.client_tokens:
            return self.error(403, "forbidden", "a valid bearer token is required")
        client_id = self.client_tokens[token]
        if method == "POST" and parts == ["v1", "snapshots"]:
            lease = make_id()
            self.leases.add(lease)
            return httpx.Response(200, json={"lease_id": lease})
        if method == "DELETE" and parts[:2] == ["v1", "clients"] and len(parts) == 3:
            if parts[2] != client_id:
                return self.error(403, "forbidden", "only self-disconnect is permitted")
            del self.client_tokens[token]
            return httpx.Response(200, json={})
        if method == "DELETE" and parts[:2] == ["v1", "snapshots"] and len(parts) == 3:
            self.leases.discard(parts[2])
            return httpx.Response(200, json={})
        if method == "GET" and parts[:2] == ["v1", "snapshots"] and len(parts) >= 4:
            if parts[2] not in self.leases:
                return self.error(410, "snapshot_expired", "snapshot lease expired")
            if parts[3:] == ["catalog"]:
                return httpx.Response(200, json=self.catalog)
            if parts[3] == "topics" and len(parts) == 6 and parts[5] == "data":
                chunks = self.data_chunks
                if chunks is None:
                    chunks = split(arrow_bytes(attitude_batches(), attitude_schema()), 64)
                stream = TrackingStream(chunks, self.data_fail_after)
                self.streams.append(stream)
                return httpx.Response(
                    200,
                    headers={"content-type": self.data_content_type},
                    stream=stream,
                )
        if self.control is not None:
            routed = self.control.handle(request, parts)
            if routed is not None:
                return routed
        return self.error(404, "not_found", "route not found")


class FakeNetwork:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.instances: dict[str, FakeInstance] = {}
        self.next_port = 40001

    def add(self, **kwargs: Any) -> FakeInstance:
        instance = FakeInstance(port=self.next_port, **kwargs)
        self.next_port += 1
        self.instances[instance.endpoint] = instance
        return instance

    def publish(
        self, instance: FakeInstance, pid: int | None = None, name: str | None = None
    ) -> Path:
        path = self.root / (name or f"{instance.instance_id}.json")
        self.write(path, json.dumps(instance.descriptor(pid)).encode())
        return path

    def write(self, path: Path, data: bytes, mode: int = 0o600) -> Path:
        path.write_bytes(data)
        os.chmod(path, mode)
        return path

    def handler(self, request: httpx.Request) -> httpx.Response:
        instance = self.instances.get(request.url.netloc.decode())
        if instance is None:
            raise httpx.ConnectError("connection refused", request=request)
        return instance.handle(request)


@pytest.fixture
def network(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> FakeNetwork:
    root = tmp_path / "instances"
    root.mkdir(mode=0o700)
    os.chmod(root, 0o700)
    monkeypatch.setenv("DELOG_DISCOVERY_DIR", str(root))
    fake = FakeNetwork(root)

    def factory() -> httpx.BaseTransport:
        return httpx.MockTransport(fake.handler)

    monkeypatch.setattr(transport_module, "_http_transport", factory)
    return fake


@pytest.fixture
def server(network: FakeNetwork) -> FakeInstance:
    instance = network.add(label="bench")
    network.publish(instance)
    return instance


@pytest.fixture
def mock_client(server: FakeInstance) -> Iterator[Client]:
    client = DeLOG.connect(server.instance_id, name="flight-diagnosis")
    yield client
    client.close()


QUERY_OPS = frozenset(
    {"layout_list", "layout_current", "vehicle_profile_list", "vehicle_profile_load"}
)
RESOURCE_OPS = frozenset(
    {
        "window_open",
        "workspace_add_plot",
        "workspace_split",
        "trace_add",
        "annotation_add",
        "marker_add",
        "vehicle_add",
        "vehicle_set",
        "vehicle_profile_apply",
    }
)
REMOVING_OPS = {
    "workspace_close": "plot",
    "trace_remove": "trace",
    "annotation_remove": "annotation",
    "marker_remove": "marker",
    "vehicle_remove": "vehicle",
}
ARROW_TYPES = {"float": "float32", "double": "float64", "bool": "boolean", "string": "utf8"}


def envelope(
    status: int, code: str, message: str, request_id: str | None = None, **extra: Any
) -> httpx.Response:
    body: dict[str, Any] = {
        "request_id": request_id or make_id(),
        "code": code,
        "message": message,
        "retryable": code == "unavailable",
    }
    body.update(extra)
    return httpx.Response(status, json=body)


@dataclass
class FakeRecord:
    request_id: str
    fingerprint: tuple[str, str, str]
    state: str
    status: int = 0
    body: Any = None


class FakeControl:
    def __init__(self) -> None:
        self.access = "full"
        self.records: dict[str, FakeRecord] = {}
        self.executed: list[dict[str, Any]] = []
        self.batches: list[list[dict[str, Any]]] = []
        self.uploads: list[tuple[str, pa.Table, dict[str, str]]] = []
        self.removed_publications: list[str] = []
        self.bodies: list[bytes] = []
        self.keys: list[str | None] = []
        self.lookups: list[str] = []
        self.failures: list[str] = []
        self.results: dict[str, Any] = {}
        self.errors: dict[str, httpx.Response] = {}
        self.stale: set[str] = set()
        self.publications: dict[str, dict[str, Any]] = {}
        self.lookup_failure: str | None = None
        self.main_window = make_id()
        self.plot_windows: dict[str, str] = {}
        self.state: dict[str, Any] = {
            "windows": [],
            "plots": [],
            "traces": [],
            "annotations": [],
            "markers": [],
            "vehicles": [],
            "layout_names": ["default"],
            "current_layout": "{}",
            "playback": {"speed": 1.0, "follow_live": False},
            "scene_visible": True,
        }

    def mutations(self) -> list[dict[str, Any]]:
        return self.executed

    def handle(self, request: httpx.Request, parts: list[str]) -> httpx.Response | None:
        method = request.method
        if method == "GET" and parts == ["v1", "control", "state"]:
            return httpx.Response(200, json=self.state)
        if method == "GET" and parts == ["v1", "requests", "by-key"]:
            return self._lookup(request, request.headers.get("idempotency-key"), None)
        if method == "GET" and parts[:2] == ["v1", "requests"] and len(parts) == 3:
            return self._lookup(request, None, parts[2])
        if method == "POST" and parts == ["v1", "control"]:
            command = json.loads(request.content)
            if command["op"] in QUERY_OPS and "idempotency-key" not in request.headers:
                self.keys.append(None)
                self.bodies.append(request.content)
                return self._respond(self._execute(command))
            return self._idempotent(request, lambda: self._execute(command))
        if method == "POST" and parts == ["v1", "control", "batch"]:
            commands = json.loads(request.content)["commands"]
            return self._idempotent(request, lambda: self._batch(commands))
        if method == "PUT" and parts[:2] == ["v1", "publications"] and len(parts) == 3:
            if request.headers.get("content-type") != ARROW:
                return envelope(400, "invalid_input", "publication requires Arrow")
            return self._idempotent(request, lambda: self._publish(request, parts[2]))
        if method == "DELETE" and parts[:2] == ["v1", "publications"] and len(parts) == 3:
            return self._idempotent(request, lambda: self._unpublish(parts[2]))
        return None

    def _respond(self, outcome: tuple[int, Any], request_id: str | None = None) -> httpx.Response:
        status, body = outcome
        headers = {"x-request-id": request_id} if request_id else {}
        return httpx.Response(status, json=body, headers=headers)

    def _lookup(
        self, request: httpx.Request, key: str | None, request_id: str | None
    ) -> httpx.Response:
        self.lookups.append(key or request_id or "")
        if self.lookup_failure is not None:
            raise httpx.ConnectError(self.lookup_failure, request=request)
        record = None
        if key is not None:
            record = self.records.get(key)
        else:
            record = next((r for r in self.records.values() if r.request_id == request_id), None)
        if record is None:
            return envelope(404, "not_found", "request not found")
        body: dict[str, Any] = {"request_id": record.request_id, "state": record.state}
        if record.state == "completed":
            body["status"] = record.status
            body["response"] = record.body
        if record.state == "unknown":
            body["error"] = record.body
        return httpx.Response(200, json=body)

    def _idempotent(
        self, request: httpx.Request, work: Callable[[], tuple[int, Any]]
    ) -> httpx.Response:
        content = request.read()
        key = request.headers.get("idempotency-key")
        self.keys.append(key)
        self.bodies.append(content)
        if key is None:
            return envelope(400, "invalid_input", "an Idempotency-Key header is required")
        fingerprint = (request.method, request.url.raw_path.decode(), content.hex())
        failure = self.failures.pop(0) if self.failures else None
        if failure == "connect":
            raise httpx.ConnectError("connection refused", request=request)
        if failure == "lost_request":
            raise httpx.ReadError("connection reset", request=request)
        record = self.records.get(key)
        if record is not None:
            if record.fingerprint != fingerprint:
                return envelope(409, "conflict", "Idempotency-Key was already used")
            if record.state == "in_flight":
                record.status, record.body = work()
                record.state = "completed"
            if record.state == "unknown":
                return httpx.Response(
                    503, json=record.body, headers={"x-request-id": record.request_id}
                )
            return self._respond((record.status, record.body), record.request_id)
        record = FakeRecord(make_id(), fingerprint, "in_flight")
        if failure == "lost_in_flight":
            self.records[key] = record
            raise httpx.ReadError("connection reset", request=request)
        if failure == "lost_unknown":
            record.state = "unknown"
            record.body = {
                "request_id": record.request_id,
                "code": "unavailable",
                "message": "the UI did not answer in time",
                "retryable": True,
                "completion": "unknown",
            }
            self.records[key] = record
            raise httpx.ReadError("connection reset", request=request)
        status, body = work()
        if isinstance(body, dict) and body.get("completion") == "not_started":
            return self._respond((status, body), record.request_id)
        record.state, record.status, record.body = "completed", status, body
        if isinstance(body, dict) and "code" in body:
            body["request_id"] = record.request_id
        self.records[key] = record
        if failure == "lost_response":
            raise httpx.ReadError("connection reset", request=request)
        if failure == "lost_body":
            return httpx.Response(
                status,
                headers={"x-request-id": record.request_id},
                stream=TrackingStream([b"{"], fail_after=0),
            )
        return self._respond((status, body), record.request_id)

    def _error(self, status: int, code: str, message: str, **extra: Any) -> tuple[int, Any]:
        body: dict[str, Any] = {
            "request_id": make_id(),
            "code": code,
            "message": message,
            "retryable": code == "unavailable",
        }
        body.update(extra)
        return status, body

    def _execute(self, command: dict[str, Any]) -> tuple[int, Any]:
        op = command["op"]
        if op in self.errors:
            response = self.errors[op]
            return response.status_code, json.loads(response.content)
        if self.access == "safe" and op not in QUERY_OPS:
            return self._error(
                403,
                "forbidden",
                "DeLOG is in safe mode; enable full control in the DeLOG UI",
                completion="not_started",
            )
        for value in command.values():
            if isinstance(value, str) and value in self.stale:
                return self._error(409, "stale_handle", "handle is stale")
        self.executed.append(command)
        if op in REMOVING_OPS:
            self.stale.add(command[REMOVING_OPS[op]])
        if op in self.results:
            return 200, self.results[op]
        if op in ("workspace_add_plot", "workspace_split"):
            handle = make_id()
            if op == "workspace_add_plot":
                window = command.get("window", self.main_window)
            else:
                window = self.plot_windows.get(command["plot"], self.main_window)
            self.plot_windows[handle] = window
            return 200, {"kind": "resource", "handle": handle, "window": window}
        if op in RESOURCE_OPS:
            return 200, {"kind": "resource", "handle": make_id()}
        if op in ("layout_list", "vehicle_profile_list"):
            return 200, {"kind": "names", "names": ["default", "quad"]}
        if op == "layout_current":
            return 200, {"kind": "layout", "json": '{"plots":[]}'}
        if op == "vehicle_profile_load":
            return 200, {
                "kind": "vehicle_profile",
                "profile": {
                    "name": command["name"],
                    "label": "Estimated vehicle",
                    "show": True,
                    "show_path": False,
                    "position": {"kind": "gps"},
                    "orientation": {"kind": "static"},
                    "model": "quad",
                    "color": "#5AAAFFFF",
                    "path_color": "#FFAA3CFF",
                    "scale": 1.5,
                },
            }
        if op == "remove_owned":
            return 200, {"kind": "removed", "ui_resources": 4, "publications": 2}
        return 200, {"kind": "unit"}

    def _batch(self, commands: list[dict[str, Any]]) -> tuple[int, Any]:
        self.batches.append(commands)
        for command in commands:
            status, body = self._execute(command)
            if status != 200:
                return status, body
        return 200, {"kind": "unit"}

    def _publish(self, request: httpx.Request, name: str) -> tuple[int, Any]:
        if self.access == "safe":
            return self._error(403, "forbidden", "publication requires full control")
        table = pa.ipc.open_stream(request.content).read_all()
        replace = request.url.params.get("replace") == "true"
        self.uploads.append((name, table, dict(request.url.params)))
        existing = self.publications.get(name)
        if existing is not None and not replace:
            return self._error(409, "conflict", f"topic {name!r} is already published")
        generation = 1 if existing is None else existing["generation"] + 1
        fields = []
        for column in table.schema:
            if column.name.startswith("__"):
                continue
            metadata = {k.decode(): v.decode() for k, v in (column.metadata or {}).items()}
            fields.append(
                {
                    "handle": make_id(),
                    "name": column.name,
                    "arrow_type": ARROW_TYPES.get(str(column.type), str(column.type)),
                    "unit": metadata.get("delog.unit"),
                    "description": metadata.get("delog.description"),
                    "multiplier": float(metadata.get("delog.multiplier", "1.0")),
                }
            )
        payload = {
            "handle": make_id(),
            "generation": generation,
            "topic": {
                "handle": make_id(),
                "name": name,
                "row_count": table.num_rows,
                "fields": fields,
            },
        }
        self.publications[name] = payload
        return (201 if generation == 1 else 200), payload

    def _unpublish(self, handle: str) -> tuple[int, Any]:
        self.removed_publications.append(handle)
        for publication in self.publications.values():
            if publication["handle"] == handle:
                self.stale.update(f["handle"] for f in publication["topic"]["fields"])
        return 200, {}


def derived_table(rows: int = 8) -> pa.Table:
    return pa.table(
        {
            "__delog_time_ns": pa.array([i * 1000 for i in range(rows)], pa.int64()),
            "error": pa.array([float(i) / 10 for i in range(rows)], pa.float64()),
            "limit": pa.array([1.0] * rows, pa.float32()),
        }
    )


@pytest.fixture
def control(server: FakeInstance) -> FakeControl:
    server.control = FakeControl()
    return server.control


@pytest.fixture
def client(control: FakeControl, mock_client: Client) -> Client:
    return mock_client


@pytest.fixture
def arrow_table() -> pa.Table:
    return derived_table()
