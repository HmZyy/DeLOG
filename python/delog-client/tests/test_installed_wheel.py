from __future__ import annotations

import email
import os
import shutil
import subprocess
import sys
import tarfile
import textwrap
import zipfile
from collections.abc import Iterator
from pathlib import Path

import pytest
import test_rust_control_server as rust_fixture

PROJECT = Path(__file__).resolve().parents[1]
VERSION = "0.1.0"

control_server = rust_fixture.control_server

SCRIPT = textwrap.dedent(
    """
    import sys
    from pathlib import Path

    import pyarrow as pa
    import pyarrow.compute as pc

    import delog_client
    from delog_client import DeLOG

    source_tree = Path(sys.argv[1])
    assert source_tree not in Path(delog_client.__file__).resolve().parents
    assert (Path(delog_client.__file__).parent / "py.typed").is_file()

    [instance] = DeLOG.list_instances()
    client = DeLOG.connect(instance.id, name="flight-diagnosis")
    with client.snapshot() as snapshot:
        attitude = snapshot.topic("vehicle_attitude", source="flight").read()
    error = pc.subtract(attitude.column("roll_setpoint"), attitude.column("roll"))
    derived = client.publish_topic(
        "attitude_error",
        pa.table({"__delog_time_ns": attitude.column("__delog_time_ns"), "error": error}),
    )
    window = client.windows.open("Diagnosis")
    plot = window.workspace.add_plot()
    plot.traces.add(derived.field("error"))
    client.close()

    again = DeLOG.connect(instance.id, name="flight-diagnosis")
    state = again.state()
    assert [w.title for w in state.windows if w.owner] == ["Diagnosis"]
    report = again.remove_owned()
    assert report.complete and report.publications == 1
    again.close()
    print("WHEEL-OK")
    """
)


def _uv() -> str:
    uv = shutil.which("uv")
    if uv is None:
        pytest.skip("uv is not on PATH")
    return uv


def _run(*args: str, cwd: Path, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        args, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    )
    assert result.returncode == 0, result.stdout
    return result.stdout


@pytest.fixture(scope="module")
def artifacts(tmp_path_factory: pytest.TempPathFactory) -> Iterator[tuple[Path, Path]]:
    prebuilt = os.environ.get("DELOG_CLIENT_DIST")
    if prebuilt:
        out = Path(prebuilt).resolve()
    else:
        out = tmp_path_factory.mktemp("dist")
        _run(_uv(), "build", "--out-dir", str(out), str(PROJECT), cwd=PROJECT)
    [wheel] = out.glob("*.whl")
    [sdist] = out.glob("*.tar.gz")
    yield wheel, sdist


def test_wheel_and_sdist_carry_typed_package_and_metadata(artifacts: tuple[Path, Path]) -> None:
    wheel, sdist = artifacts
    assert wheel.name == f"delog_client-{VERSION}-py3-none-any.whl"
    with zipfile.ZipFile(wheel) as archive:
        names = archive.namelist()
        metadata = email.message_from_bytes(
            archive.read(f"delog_client-{VERSION}.dist-info/METADATA")
        )
    package = [name for name in names if not name.startswith(f"delog_client-{VERSION}.dist-info/")]
    assert "delog_client/py.typed" in package
    assert "delog_client/__init__.py" in package
    assert all(name.startswith("delog_client/") for name in package), package
    assert not [name for name in names if "__pycache__" in name or name.endswith(".json")]

    assert metadata["Name"] == "delog-client"
    assert metadata["Requires-Python"] == ">=3.11"
    assert metadata["License-Expression"] == "MIT"
    classifiers = metadata.get_all("Classifier") or []
    for minor in (11, 12, 13, 14):
        assert f"Programming Language :: Python :: 3.{minor}" in classifiers
    assert "Typing :: Typed" in classifiers
    urls = metadata.get_all("Project-URL") or []
    assert any(url.startswith("Repository, https://github.com/HmZyy/DeLOG") for url in urls)
    assert "delog-client" in (metadata.get_payload() or "")

    with tarfile.open(sdist) as archive:
        members = [name.split("/", 1)[1] for name in archive.getnames() if "/" in name]
    assert "src/delog_client/py.typed" in members
    assert "README.md" in members
    assert "pyproject.toml" in members
    assert not [name for name in members if name.startswith("tests") or name == "uv.lock"]


def test_installed_wheel_discovers_reads_publishes_and_controls_outside_the_source_tree(
    artifacts: tuple[Path, Path],
    control_server: rust_fixture.ControlFixtureServer,
    tmp_path: Path,
) -> None:
    wheel, _ = artifacts
    uv = _uv()
    venv = tmp_path / "venv"
    _run(uv, "venv", "--python", sys.executable, str(venv), cwd=tmp_path)
    python = venv / ("Scripts/python.exe" if sys.platform == "win32" else "bin/python")
    _run(uv, "pip", "install", "--python", str(python), str(wheel), cwd=tmp_path)

    work = tmp_path / "work"
    work.mkdir()
    script = work / "diagnose.py"
    script.write_text(SCRIPT)
    env = {key: value for key, value in os.environ.items() if key != "PYTHONPATH"}
    env["DELOG_DISCOVERY_DIR"] = str(control_server.discovery_dir)
    output = _run(str(python), "-I", str(script), str(PROJECT), cwd=work, env=env)
    assert output.strip().endswith("WHEEL-OK"), output
