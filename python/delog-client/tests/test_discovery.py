from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

import pytest

from delog_client import (
    Client,
    ConflictError,
    DeLOG,
    ForbiddenError,
    IncompatibleVersionError,
    Instance,
    NotFoundError,
    discovery,
)

from conftest import FakeInstance, FakeNetwork, dead_pid, make_id


def ids(instances: list[Instance]) -> list[str]:
    return [instance.id for instance in instances]


def test_lists_healthy_instance_with_descriptor_metadata(server: FakeInstance) -> None:
    instances = DeLOG.list_instances()
    assert len(instances) == 1
    instance = instances[0]
    assert instance.id == server.instance_id
    assert instance.label == "bench"
    assert instance.pid == os.getpid()
    assert instance.endpoint == server.endpoint
    assert instance.session_description == "flight.ulg"
    assert instance.loaded_file == "flight.ulg"
    assert (instance.api_major, instance.api_min_minor, instance.api_max_minor) == (1, 0, 0)


def test_health_check_uses_bootstrap_bearer_and_loopback_host(server: FakeInstance) -> None:
    DeLOG.list_instances()
    request = server.requests[-1]
    assert request.method == "GET"
    assert request.url.path == "/v1/instance"
    assert request.headers["authorization"] == f"Bearer {server.bootstrap}"
    assert request.headers.get_list("host") == [server.endpoint]
    assert "origin" not in request.headers


def test_results_sort_by_label_then_instance_id(network: FakeNetwork) -> None:
    first = network.add(label="zeta", instance_id="b-id")
    second = network.add(label="alpha", instance_id="z-id")
    third = network.add(label="zeta", instance_id="a-id")
    for instance in (first, second, third):
        network.publish(instance)
    assert ids(DeLOG.list_instances()) == ["z-id", "a-id", "b-id"]


def test_missing_directory_lists_nothing(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("DELOG_DISCOVERY_DIR", str(tmp_path / "absent"))
    assert DeLOG.list_instances() == []


def test_ignores_staging_and_foreign_names(network: FakeNetwork, server: FakeInstance) -> None:
    other = network.add()
    network.publish(other, name=f".{other.instance_id}.123.tmp")
    network.publish(network.add(), name="notes.txt")
    mismatched = network.add()
    network.publish(mismatched, name=f"{make_id()}.json")
    assert ids(DeLOG.list_instances()) == [server.instance_id]


def test_ignores_dead_process(network: FakeNetwork, server: FakeInstance) -> None:
    dead = network.add()
    network.publish(dead, pid=dead_pid())
    assert ids(DeLOG.list_instances()) == [server.instance_id]
    assert not any(r.url.netloc.decode() == dead.endpoint for r in dead.requests)


def test_ignores_unreachable_endpoint(network: FakeNetwork, server: FakeInstance) -> None:
    down = network.add(reachable=False)
    network.publish(down)
    assert ids(DeLOG.list_instances()) == [server.instance_id]


def test_ignores_mismatched_health_response(network: FakeNetwork, server: FakeInstance) -> None:
    wrong_id = network.add(reported_instance_id=make_id())
    wrong_major = network.add(reported_api_major=2)
    network.publish(wrong_id)
    network.publish(wrong_major)
    assert ids(DeLOG.list_instances()) == [server.instance_id]


def test_ignores_unsupported_api_versions(network: FakeNetwork, server: FakeInstance) -> None:
    major = network.add(api_major=2)
    minor = network.add(api_min_minor=1, api_max_minor=3)
    network.publish(major)
    network.publish(minor)
    assert ids(DeLOG.list_instances()) == [server.instance_id]
    assert major.requests == []


def test_connect_refuses_unsupported_api_major(network: FakeNetwork) -> None:
    major = network.add(api_major=2)
    network.publish(major)
    with pytest.raises(IncompatibleVersionError) as caught:
        DeLOG.connect(major.instance_id, name="job")
    assert major.bootstrap not in str(caught.value)
    assert major.requests == []


@pytest.mark.parametrize(
    "change",
    [
        {"extra": 1},
        {"format": 2},
        {"format": True},
        {"endpoint": "0.0.0.0:40001"},
        {"endpoint": "[::1]:40001"},
        {"endpoint": "127.0.0.1:0"},
        {"endpoint": "localhost:40001"},
        {"endpoint": "127.0.0.1:70000"},
        {"bootstrap_token": "short"},
        {"bootstrap_token": 12},
        {"pid": -1},
        {"pid": "12"},
        {"label": None},
        {"api_major": 70000},
        {"session_description": 3},
    ],
)
def test_rejects_malformed_descriptors(
    network: FakeNetwork, server: FakeInstance, change: dict[str, Any]
) -> None:
    bad = network.add()
    payload = bad.descriptor()
    payload.update(change)
    network.write(network.root / f"{bad.instance_id}.json", json.dumps(payload).encode())
    assert ids(DeLOG.list_instances()) == [server.instance_id]
    assert bad.requests == []


@pytest.mark.parametrize(
    "missing", ["format", "instance_id", "label", "pid", "endpoint", "api_major", "bootstrap_token"]
)
def test_rejects_descriptors_missing_keys(
    network: FakeNetwork, server: FakeInstance, missing: str
) -> None:
    bad = network.add()
    payload = bad.descriptor()
    del payload[missing]
    network.write(network.root / f"{bad.instance_id}.json", json.dumps(payload).encode())
    assert ids(DeLOG.list_instances()) == [server.instance_id]


def test_accepts_null_session_description(network: FakeNetwork) -> None:
    instance = network.add(session_description=None)
    payload = instance.descriptor()
    payload["session_description"] = None
    network.write(network.root / f"{instance.instance_id}.json", json.dumps(payload).encode())
    assert DeLOG.list_instances()[0].session_description is None


def test_rejects_oversized_and_invalid_json(network: FakeNetwork, server: FakeInstance) -> None:
    big = network.add()
    payload = big.descriptor()
    payload["label"] = "x" * (64 * 1024)
    network.write(network.root / f"{big.instance_id}.json", json.dumps(payload).encode())
    garbage = network.add()
    network.write(network.root / f"{garbage.instance_id}.json", b"{not json")
    assert ids(DeLOG.list_instances()) == [server.instance_id]


def test_rejects_files_other_users_could_read(network: FakeNetwork, server: FakeInstance) -> None:
    loose = network.add()
    path = network.publish(loose)
    os.chmod(path, 0o644)
    assert ids(DeLOG.list_instances()) == [server.instance_id]


def test_rejects_symlinks_and_directories(
    network: FakeNetwork, server: FakeInstance, tmp_path: Path
) -> None:
    linked = network.add()
    target = tmp_path / "elsewhere.json"
    network.write(target, json.dumps(linked.descriptor()).encode())
    (network.root / f"{linked.instance_id}.json").symlink_to(target)
    directory = network.add()
    (network.root / f"{directory.instance_id}.json").mkdir(mode=0o700)
    assert ids(DeLOG.list_instances()) == [server.instance_id]


def test_refuses_directory_other_users_can_access(
    network: FakeNetwork, server: FakeInstance
) -> None:
    os.chmod(network.root, 0o755)
    try:
        with pytest.raises(ForbiddenError):
            DeLOG.list_instances()
    finally:
        os.chmod(network.root, 0o700)


def test_default_root_prefers_xdg_runtime_dir(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("DELOG_DISCOVERY_DIR", raising=False)
    monkeypatch.setenv("XDG_RUNTIME_DIR", "/run/user/1000")
    assert discovery.discovery_root() == Path("/run/user/1000/delog/instances")


def test_default_root_falls_back_to_tmpdir(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("DELOG_DISCOVERY_DIR", raising=False)
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    monkeypatch.setenv("TMPDIR", "/var/tmp/me")
    expected = Path(f"/var/tmp/me/delog-runtime-{os.geteuid()}/instances")
    assert discovery.discovery_root() == expected
    monkeypatch.setenv("XDG_RUNTIME_DIR", "relative/run")
    assert discovery.discovery_root() == expected


def test_default_root_uses_tmp_without_tmpdir(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("DELOG_DISCOVERY_DIR", raising=False)
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    monkeypatch.delenv("TMPDIR", raising=False)
    assert discovery.discovery_root() == Path(f"/tmp/delog-runtime-{os.geteuid()}/instances")


def test_default_root_is_none_for_relative_tmpdir(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("DELOG_DISCOVERY_DIR", raising=False)
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    monkeypatch.setenv("TMPDIR", "")
    assert discovery.discovery_root() is None
    assert DeLOG.list_instances() == []


def test_override_directory_wins(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("XDG_RUNTIME_DIR", "/run/user/1000")
    monkeypatch.setenv("DELOG_DISCOVERY_DIR", str(tmp_path))
    assert discovery.discovery_root() == tmp_path


def test_connect_exchanges_bootstrap_for_client_token(server: FakeInstance) -> None:
    client = DeLOG.connect(server.instance_id, name="flight-diagnosis")
    registration = next(r for r in server.requests if r.url.path == "/v1/clients")
    assert registration.method == "POST"
    assert registration.headers["authorization"] == f"Bearer {server.bootstrap}"
    assert json.loads(registration.content) == {"name": "flight-diagnosis", "takeover": False}
    client.snapshot().close()
    later = [r for r in server.requests if r.url.path.startswith("/v1/snapshots")]
    client_token = next(iter(server.client_tokens))
    assert later and all(r.headers["authorization"] == f"Bearer {client_token}" for r in later)
    assert client.name == "flight-diagnosis"
    assert client.instance.id == server.instance_id
    client.close()


def test_connect_passes_takeover_and_maps_conflict(server: FakeInstance) -> None:
    first = DeLOG.connect(server.instance_id, name="job")
    with pytest.raises(ConflictError):
        DeLOG.connect(server.instance_id, name="job")
    second = DeLOG.connect(server.instance_id, name="job", takeover=True)
    registration = [r for r in server.requests if r.url.path == "/v1/clients"][-1]
    assert json.loads(registration.content) == {"name": "job", "takeover": True}
    second.close()
    first.close()


def test_connect_unknown_or_dead_instance_is_not_found(network: FakeNetwork) -> None:
    with pytest.raises(NotFoundError):
        DeLOG.connect(make_id(), name="job")
    dead = network.add()
    network.publish(dead, pid=dead_pid())
    with pytest.raises(NotFoundError):
        DeLOG.connect(dead.instance_id, name="job")


def reachable_strings(root: object) -> list[str]:
    seen: set[int] = set()
    found: list[str] = []
    stack: list[object] = [root]
    while stack:
        item = stack.pop()
        if id(item) in seen:
            continue
        seen.add(id(item))
        if isinstance(item, str):
            found.append(item)
        elif isinstance(item, bytes):
            found.append(item.decode("latin-1"))
        elif isinstance(item, dict):
            stack.extend(item.keys())
            stack.extend(item.values())
        elif isinstance(item, (list, tuple, set, frozenset)):
            stack.extend(item)
        elif type(item).__module__.startswith("delog_client"):
            stack.extend(getattr(item, "__dict__", {}).values())
            for slot in getattr(type(item), "__slots__", ()):
                if hasattr(item, slot):
                    stack.append(getattr(item, slot))
    return found


def test_connected_client_does_not_retain_bootstrap_token(server: FakeInstance) -> None:
    client = DeLOG.connect(server.instance_id, name="flight-diagnosis")
    client_token = next(iter(server.client_tokens))
    strings = reachable_strings(client)
    assert not any(server.bootstrap in s for s in strings)
    for text in (repr(client), str(client), repr(client.instance), str(client.instance)):
        assert server.bootstrap not in text
        assert client_token not in text
    client.close()


def test_instance_repr_has_no_token(server: FakeInstance) -> None:
    instance = DeLOG.list_instances()[0]
    assert server.bootstrap not in repr(instance)
    assert not any(server.bootstrap in s for s in reachable_strings(instance))


def test_client_close_disconnects_itself(server: FakeInstance) -> None:
    client: Client = DeLOG.connect(server.instance_id, name="job")
    client.close()
    disconnect = server.requests[-1]
    assert disconnect.method == "DELETE"
    assert disconnect.url.path == f"/v1/clients/{client.client_id}"
    assert server.client_tokens == {}
    client.close()
    assert server.requests[-1] is disconnect


def test_client_context_manager_closes(server: FakeInstance) -> None:
    with DeLOG.connect(server.instance_id, name="job") as client:
        assert server.client_tokens
    assert server.client_tokens == {}
    assert client.closed
