from __future__ import annotations

import os
import queue
import shutil
import subprocess
import sys
import threading
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

import pyarrow
import pytest

from delog_client import (
    AmbiguousError,
    ConflictError,
    DeLOG,
    InvalidInputError,
    UnavailableError,
)

READY_TIMEOUT = 20.0
EXIT_TIMEOUT = 10.0


@dataclass
class RustFixtureServer:
    process: subprocess.Popen[str]
    discovery_dir: Path
    instance_id: str


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[3]


def _binary_path(repo_root: Path) -> Path:
    name = "read_fixture_server.exe" if sys.platform == "win32" else "read_fixture_server"
    target = os.environ.get("CARGO_TARGET_DIR") or "target"
    return repo_root / target / "debug" / "examples" / name


def _read_ready_line(process: subprocess.Popen[str]) -> str:
    assert process.stdout is not None
    lines: queue.Queue[str] = queue.Queue(maxsize=1)

    def reader() -> None:
        assert process.stdout is not None
        lines.put(process.stdout.readline())

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    thread.join(READY_TIMEOUT)
    if thread.is_alive():
        process.kill()
        process.wait(timeout=EXIT_TIMEOUT)
        raise AssertionError("fixture server did not print READY before the timeout")
    line = lines.get()
    if not line:
        stderr = process.stderr.read() if process.stderr else ""
        process.wait(timeout=EXIT_TIMEOUT)
        raise AssertionError(f"fixture server exited before printing READY: {stderr}")
    return line


def _parse_ready(line: str) -> str:
    prefix, _, instance_id = line.strip().partition(" ")
    if prefix != "READY" or not instance_id:
        raise AssertionError(f"unexpected fixture server output: {line!r}")
    return instance_id


@pytest.fixture(scope="module")
def rust_fixture_server(
    tmp_path_factory: pytest.TempPathFactory,
) -> Iterator[RustFixtureServer]:
    if shutil.which("cargo") is None:
        pytest.skip("cargo is not on PATH")
    repo_root = _repo_root()
    build = subprocess.run(
        ["cargo", "build", "-p", "delog-remote", "--example", "read_fixture_server"],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    if build.returncode != 0:
        pytest.fail(f"cargo build failed:\n{build.stdout}")
    binary = _binary_path(repo_root)
    assert binary.exists(), f"expected fixture binary at {binary}"

    discovery_dir = tmp_path_factory.mktemp("rust-fixture-discovery")
    discovery_dir.chmod(0o700)
    env = dict(os.environ)
    env["DELOG_TEST_DISCOVERY_DIR"] = str(discovery_dir)

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
        instance_id = _parse_ready(_read_ready_line(process))
        yield RustFixtureServer(
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


def test_python_reads_real_rust_arrow_stream(
    rust_fixture_server: RustFixtureServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("DELOG_DISCOVERY_DIR", str(rust_fixture_server.discovery_dir))
    instance = DeLOG.list_instances()[0]
    client = DeLOG.connect(instance.id, name="pytest-reader")
    try:
        with client.snapshot() as snapshot:
            topic = snapshot.topic("mixed", source="flight")
            batches = list(topic.iter_batches(fields=["count", "armed"]))
            assert batches[0].schema.field("count").type == pyarrow.int16()
            assert batches[0].schema.field("armed").type == pyarrow.bool_()

            full_batches = list(topic.iter_batches())
            assert [batch.num_rows for batch in full_batches] == [3, 2]

            table = topic.read()
            assert table.num_rows == 5
            assert table.column("__source_time_ns").to_pylist() == [
                1_000_000_000,
                1_010_000_000,
                1_020_000_000,
                1_030_000_000,
                1_040_000_000,
            ]
            assert table.column("__delog_time_ns").to_pylist() == [
                500_000_000,
                510_000_000,
                520_000_000,
                530_000_000,
                540_000_000,
            ]
            assert table.column("label").to_pylist() == ["a", "b", "c", "d", "e"]
            assert table.column("count").type == pyarrow.int16()
            assert table.column("armed").type == pyarrow.bool_()

            windowed = topic.read(start_ns=510_000_000, end_ns=530_000_000)
            assert windowed.column("__delog_time_ns").to_pylist() == [
                510_000_000,
                520_000_000,
                530_000_000,
            ]
            assert windowed.column("count").to_pylist() == [11, 12, 13]
            assert windowed.column("armed").to_pylist() == [False, True, False]
            assert windowed.column("label").to_pylist() == ["b", "c", "d"]
    finally:
        client.close()


def test_duplicate_topic_names_are_ambiguous_against_the_real_catalog(
    rust_fixture_server: RustFixtureServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("DELOG_DISCOVERY_DIR", str(rust_fixture_server.discovery_dir))
    instance = DeLOG.list_instances()[0]
    client = DeLOG.connect(instance.id, name="pytest-ambiguity")
    try:
        with client.snapshot() as snapshot:
            with pytest.raises(AmbiguousError) as excinfo:
                snapshot.topic("GPS")
            assert excinfo.value.candidates == ["chase/GPS", "flight/GPS"]
    finally:
        client.close()


def test_binary_path_honours_cargo_target_dir(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    name = "read_fixture_server.exe" if sys.platform == "win32" else "read_fixture_server"
    repo_root = tmp_path / "repo"
    monkeypatch.delenv("CARGO_TARGET_DIR", raising=False)
    assert _binary_path(repo_root) == repo_root / "target" / "debug" / "examples" / name
    monkeypatch.setenv("CARGO_TARGET_DIR", "")
    assert _binary_path(repo_root) == repo_root / "target" / "debug" / "examples" / name
    shared = tmp_path / "shared-target"
    monkeypatch.setenv("CARGO_TARGET_DIR", str(shared))
    assert _binary_path(repo_root) == shared / "debug" / "examples" / name
    monkeypatch.setenv("CARGO_TARGET_DIR", "build-output")
    assert _binary_path(repo_root) == repo_root / "build-output" / "debug" / "examples" / name


def test_python_control_round_trips_against_the_real_server(
    rust_fixture_server: RustFixtureServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("DELOG_DISCOVERY_DIR", str(rust_fixture_server.discovery_dir))
    instance = DeLOG.list_instances()[0]
    client = DeLOG.connect(instance.id, name="pytest-control")
    try:
        client.playback.set(speed=2.0, follow_live=True, idempotency_key="real-playback")
        client.playback.set(speed=2.0, follow_live=True, idempotency_key="real-playback")
        status = client.request_status(idempotency_key="real-playback")
        assert status.state == "completed"
        assert status.response == {"kind": "unit"}
        with pytest.raises(ConflictError):
            client.playback.set(speed=3.0, idempotency_key="real-playback")
        with client.batch(idempotency_key="real-batch"):
            client.workspace.equalize()
            client.workspace.set_scene_visible(True)
            client.playback.set(follow_live=False)
        assert client.request_status(idempotency_key="real-batch").state == "completed"
    finally:
        client.close()


def test_python_arrow_upload_is_decoded_by_the_real_server(
    rust_fixture_server: RustFixtureServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("DELOG_DISCOVERY_DIR", str(rust_fixture_server.discovery_dir))
    instance = DeLOG.list_instances()[0]
    client = DeLOG.connect(instance.id, name="pytest-upload")
    rows = 200_000
    table = pyarrow.table(
        {
            "__delog_time_ns": pyarrow.array([i * 1000 for i in range(rows)], pyarrow.int64()),
            "error": pyarrow.array([float(i) for i in range(rows)], pyarrow.float64()),
        }
    )
    unaligned = pyarrow.table(
        {
            "__delog_time_ns": pyarrow.array([1500], pyarrow.int64()),
            "error": pyarrow.array([1.0], pyarrow.float64()),
        }
    )
    try:
        with pytest.raises(InvalidInputError, match="exact microseconds"):
            client.publish_topic("unaligned", unaligned)
        with pytest.raises(UnavailableError, match="ingest writer") as caught:
            client.publish_topic("attitude_error", table, units={"error": "rad"})
        assert caught.value.completion == "not_started"
    finally:
        client.close()
