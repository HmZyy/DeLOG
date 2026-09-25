from __future__ import annotations

import json
import os
import queue
import shutil
import subprocess
import sys
import threading
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pyarrow
import pyarrow.compute as pc
import pytest

from delog_client import Client, DeLOG, NotFoundError
from delog_client.control import ControlState, Window

READY_TIMEOUT = 20.0
REPLY_TIMEOUT = 10.0
EXIT_TIMEOUT = 10.0
EXAMPLE = "control_fixture_server"
OWNER = "external/flight-diagnosis"


@dataclass
class ControlFixtureServer:
    process: subprocess.Popen[str]
    discovery_dir: Path
    instance_id: str

    def recorded_calls(self) -> dict[str, Any]:
        assert self.process.stdin is not None
        self.process.stdin.write("calls\n")
        self.process.stdin.flush()
        reply = _read_line(self.process, REPLY_TIMEOUT, "the recorded calls")
        decoded = json.loads(reply)
        assert isinstance(decoded, dict)
        return decoded


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[3]


def _binary_path(repo_root: Path) -> Path:
    name = f"{EXAMPLE}.exe" if sys.platform == "win32" else EXAMPLE
    target = os.environ.get("CARGO_TARGET_DIR") or "target"
    return repo_root / target / "debug" / "examples" / name


def _read_line(process: subprocess.Popen[str], timeout: float, what: str) -> str:
    assert process.stdout is not None
    lines: queue.Queue[str] = queue.Queue(maxsize=1)

    def reader() -> None:
        assert process.stdout is not None
        lines.put(process.stdout.readline())

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    thread.join(timeout)
    if thread.is_alive():
        process.kill()
        process.wait(timeout=EXIT_TIMEOUT)
        raise AssertionError(f"control fixture did not print {what} before the timeout")
    line = lines.get()
    if not line:
        stderr = process.stderr.read() if process.stderr else ""
        process.wait(timeout=EXIT_TIMEOUT)
        raise AssertionError(f"control fixture exited before printing {what}: {stderr}")
    return line


def _parse_ready(line: str) -> str:
    prefix, _, instance_id = line.strip().partition(" ")
    if prefix != "READY" or not instance_id:
        raise AssertionError(f"unexpected control fixture output: {line!r}")
    return instance_id


@pytest.fixture
def control_server(
    tmp_path_factory: pytest.TempPathFactory, monkeypatch: pytest.MonkeyPatch
) -> Iterator[ControlFixtureServer]:
    if shutil.which("cargo") is None:
        pytest.skip("cargo is not on PATH")
    repo_root = _repo_root()
    build = subprocess.run(
        ["cargo", "build", "-p", "delog-remote", "--example", EXAMPLE],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    if build.returncode != 0:
        pytest.fail(f"cargo build failed:\n{build.stdout}")
    binary = _binary_path(repo_root)
    assert binary.exists(), f"expected control fixture binary at {binary}"

    discovery_dir = tmp_path_factory.mktemp("rust-control-discovery")
    discovery_dir.chmod(0o700)
    env = dict(os.environ)
    env["DELOG_TEST_DISCOVERY_DIR"] = str(discovery_dir)
    monkeypatch.setenv("DELOG_DISCOVERY_DIR", str(discovery_dir))

    process = subprocess.Popen(
        [str(binary)],
        cwd=repo_root,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        text=True,
    )
    try:
        instance_id = _parse_ready(_read_line(process, READY_TIMEOUT, "READY"))
        yield ControlFixtureServer(
            process=process, discovery_dir=discovery_dir, instance_id=instance_id
        )
    finally:
        assert process.stdin is not None
        process.stdin.close()
        try:
            process.wait(timeout=EXIT_TIMEOUT)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=EXIT_TIMEOUT)
        assert not list(discovery_dir.glob("*.json"))


def connect_fixture(server: ControlFixtureServer, *, name: str) -> Client:
    return DeLOG.connect(server.instance_id, name=name)


def compute_error(attitude: pyarrow.Table) -> pyarrow.Table:
    error = pc.subtract(attitude.column("roll_setpoint"), attitude.column("roll"))
    return pyarrow.table(
        {"__delog_time_ns": attitude.column("__delog_time_ns"), "error": error}
    )


def find_window(state: ControlState, title: str) -> Window | None:
    return next((window for window in state.windows if window.title == title), None)


def published_topics(client: Client, name: str) -> list[str]:
    with client.snapshot() as snapshot:
        return [
            topic.path
            for source in snapshot.sources()
            for topic in source.topics
            if topic.name == name
        ]


def test_publish_visualize_disconnect_reconnect_and_cleanup(
    control_server: ControlFixtureServer,
) -> None:
    client = connect_fixture(control_server, name="flight-diagnosis")
    with client.snapshot() as snapshot:
        attitude_topic = snapshot.topic("vehicle_attitude", source="flight")
        attitude = attitude_topic.read()
        position = snapshot.topic("vehicle_local_position", source="flight")
        vehicle = client.vehicles.add(
            position={
                "north": position.field("x"),
                "east": position.field("y"),
                "down": position.field("z"),
            },
            orientation={
                "euler": [
                    attitude_topic.field("roll"),
                    attitude_topic.field("pitch"),
                    attitude_topic.field("yaw"),
                ]
            },
            label="Diagnosis vehicle",
        )
    assert vehicle.handle
    derived = client.publish_topic("attitude_error", compute_error(attitude))
    assert derived.row_count == attitude.num_rows
    window = client.windows.open("Diagnosis")
    plot = window.workspace.add_plot()
    assert plot.window is window
    trace = plot.traces.add(derived.field("error"))
    annotation = plot.annotations.add_text(time_ns=2_000_000, value=0.5, text="divergence")
    assert trace.handle and annotation.handle
    client.close()

    again = connect_fixture(control_server, name="flight-diagnosis")
    assert again.owner_id == client.owner_id
    state = again.state()
    restored = find_window(state, "Diagnosis")
    assert restored is not None
    assert restored.owner == OWNER
    [restored_plot] = [p for p in state.plots if p.window is restored]
    assert restored_plot.owner == OWNER
    assert [t.field.path for t in state.traces] == ["flight-diagnosis/attitude_error/error"]
    assert [(a.label, a.owner) for a in state.annotations] == [("divergence", OWNER)]
    assert [(v.label, v.owner) for v in state.vehicles] == [("Diagnosis vehicle", OWNER)]
    assert len(published_topics(again, "attitude_error")) == 1

    report = again.remove_owned()
    assert report.publications == 1
    assert report.ui_resources == 5
    assert report.complete

    after = again.state()
    assert find_window(after, "Diagnosis") is None
    assert after.plots == () and after.traces == () and after.annotations == ()
    assert after.vehicles == ()
    assert published_topics(again, "attitude_error") == []
    with pytest.raises(NotFoundError):
        with again.snapshot() as snapshot:
            snapshot.topic("attitude_error")
    again.close()

    recorded = control_server.recorded_calls()
    calls = recorded["calls"]
    assert recorded["rejected"] == []
    assert calls, "the recording host saw no native requests"
    assert {call["access"] for call in calls} == {"safe"}
    assert {call["owner"] for call in calls} == {OWNER}
    assert {call["generation"] for call in calls} == {1}
    assert [call for call in calls if call["caller_owner"]] == []
    mutations = [call["request"] for call in calls if not call["query"]]
    assert mutations == [
        "vehicles.add",
        "workspace.open_window",
        "workspace.add_plot",
        "guarded.traces.add_returning",
        "guarded.annotations.add",
        "generation.remove_owned",
    ]
