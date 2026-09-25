from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path

import pytest
import test_rust_control_server as rust_fixture

from delog_client import DeLOG, FieldPath

REPO = Path(__file__).resolve().parents[3]
SCRIPTS = REPO / "scripts" / "external"
GUIDE = REPO / "docs" / "external_python_api.md"
MARKER = "<!-- test-example -->"

control_server = rust_fixture.control_server


def _run(script: str, *args: str, discovery: Path, cwd: Path) -> subprocess.CompletedProcess[str]:
    env = {key: value for key, value in os.environ.items() if key != "PYTHONPATH"}
    env["DELOG_DISCOVERY_DIR"] = str(discovery)
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    return subprocess.run(
        [sys.executable, str(SCRIPTS / script), *args],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
    )


def _bootstrap_token(discovery: Path) -> str:
    [descriptor] = discovery.glob("*.json")
    token = json.loads(descriptor.read_text())["bootstrap_token"]
    assert isinstance(token, str)
    return token


def _guide_blocks() -> list[tuple[bool, str]]:
    text = GUIDE.read_text()
    blocks = []
    for match in re.finditer(r"(?P<lead>[^\n]*)\n```python\n(?P<code>.*?)```", text, re.S):
        blocks.append((match.group("lead").strip() == MARKER, match.group("code")))
    return blocks


def test_catalog_prints_every_source_topic_and_field_without_data_or_secrets(
    control_server: rust_fixture.ControlFixtureServer, tmp_path: Path
) -> None:
    result = _run("catalog.py", "--pretty", discovery=control_server.discovery_dir, cwd=tmp_path)
    assert result.returncode == 0, result.stderr
    catalog = json.loads(result.stdout)
    assert catalog["instance"]["id"] == control_server.instance_id
    [flight] = [source for source in catalog["sources"] if source["label"] == "flight"]
    topics = {topic["name"]: topic for topic in flight["topics"]}
    attitude = topics["vehicle_attitude"]
    assert attitude["row_count"] == 3
    assert [field["name"] for field in attitude["fields"]] == [
        "roll",
        "pitch",
        "yaw",
        "roll_setpoint",
    ]
    assert attitude["fields"][0]["unit"] == "rad"
    assert "handle" not in json.dumps(catalog)
    assert _bootstrap_token(control_server.discovery_dir) not in result.stdout
    assert "token" not in result.stdout.lower()


def test_catalog_with_no_running_instance_asks_for_a_selection(tmp_path: Path) -> None:
    discovery = tmp_path / "empty"
    discovery.mkdir(mode=0o700)
    result = _run("catalog.py", discovery=discovery, cwd=tmp_path)
    assert result.returncode == 2
    assert "no running DeLOG instance" in result.stderr
    assert result.stdout == ""


def test_flight_diagnosis_publishes_gaps_marks_them_and_leaves_results_visible(
    control_server: rust_fixture.ControlFixtureServer, tmp_path: Path
) -> None:
    args = ["--source", "flight", "--topic", "vehicle_attitude", "--field", "roll"]
    result = _run(
        "flight_diagnosis.py",
        *args,
        "--gap-ms",
        "0.5",
        discovery=control_server.discovery_dir,
        cwd=tmp_path,
    )
    assert result.returncode == 0, result.stderr
    assert "2 gaps above 0.5 ms" in result.stdout
    assert not (SCRIPTS / "__pycache__").exists()
    assert _bootstrap_token(control_server.discovery_dir) not in result.stdout + result.stderr

    mutations = [
        call["request"] for call in control_server.recorded_calls()["calls"] if not call["query"]
    ]
    assert mutations == [
        "workspace.open_window",
        "workspace.add_plot",
        "guarded.traces.add_returning",
        "markers.append_returning",
        "guarded.annotations.add",
        "markers.append_returning",
        "guarded.annotations.add",
    ]

    client = DeLOG.connect(control_server.instance_id, name="flight-diagnosis")
    state = client.state()
    [window] = [window for window in state.windows if window.title == "Flight diagnosis"]
    assert window.owner == rust_fixture.OWNER
    paths = [trace.field.path for trace in state.traces if isinstance(trace.field, FieldPath)]
    assert paths == ["flight-diagnosis/diagnostic_sample_gaps/gap_ms"]
    assert [trace.field.name for trace in state.traces] == ["gap_ms"]
    assert len(state.traces) == 1
    assert len(state.annotations) == 2
    assert [marker.label for marker in state.markers] == ["gap 1.0 ms", "gap 1.0 ms"]
    with client.snapshot() as snapshot:
        gaps = snapshot.topic("diagnostic_sample_gaps").read(fields=["gap_ms"])
    assert gaps.column("gap_ms").to_pylist() == [1.0, 1.0]
    client.close()

    rerun = _run(
        "flight_diagnosis.py",
        *args,
        "--replace",
        discovery=control_server.discovery_dir,
        cwd=tmp_path,
    )
    assert rerun.returncode == 0, rerun.stderr
    assert "no gap above 100.0 ms" in rerun.stdout
    client = DeLOG.connect(control_server.instance_id, name="flight-diagnosis")
    state = client.state()
    assert any(annotation.label == "no gap above 100.0 ms" for annotation in state.annotations)
    with client.snapshot() as snapshot:
        assert [
            topic.name
            for source in snapshot.sources()
            for topic in source.topics
            if topic.name == "diagnostic_sample_gaps"
        ] == ["diagnostic_sample_gaps"]
    client.close()


def test_every_guide_python_block_compiles() -> None:
    blocks = _guide_blocks()
    assert len(blocks) >= 8
    for index, (_, code) in enumerate(blocks):
        compile(code, f"{GUIDE.name}[{index}]", "exec")


def test_marked_guide_examples_run_against_the_fixture(
    control_server: rust_fixture.ControlFixtureServer, tmp_path: Path
) -> None:
    marked = [code for runnable, code in _guide_blocks() if runnable]
    assert len(marked) >= 3
    env = {key: value for key, value in os.environ.items() if key != "PYTHONPATH"}
    env["DELOG_DISCOVERY_DIR"] = str(control_server.discovery_dir)
    for index, code in enumerate(marked):
        result = subprocess.run(
            [sys.executable, "-c", code],
            cwd=tmp_path,
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )
        assert result.returncode == 0, f"example {index} failed:\n{code}\n{result.stderr}"


@pytest.mark.parametrize("script", ["catalog.py", "flight_diagnosis.py"])
def test_scripts_document_their_arguments(script: str, tmp_path: Path) -> None:
    result = _run(script, "--help", discovery=tmp_path, cwd=tmp_path)
    assert result.returncode == 0, result.stderr
    assert "--instance" in result.stdout
