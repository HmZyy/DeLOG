from __future__ import annotations

import inspect
from typing import Any

import delog_client
from delog_client import Client, DeLOG

from conftest import FakeInstance

EXPORTS = [
    "AmbiguousError",
    "Annotation",
    "AnnotationCollection",
    "Client",
    "ConflictError",
    "ControlBatch",
    "ControlState",
    "DeLOG",
    "DeLOGError",
    "Field",
    "FieldPath",
    "FieldSelector",
    "ForbiddenError",
    "IncompatibleVersionError",
    "Instance",
    "InternalError",
    "InvalidInputError",
    "LayoutCollection",
    "LayoutFieldIssue",
    "LayoutLoadReport",
    "Marker",
    "MarkerCollection",
    "NotFoundError",
    "Playback",
    "PlaybackState",
    "Plot",
    "Publication",
    "PublishedField",
    "RemovalReport",
    "RequestStatus",
    "Snapshot",
    "SnapshotExpiredError",
    "Source",
    "StaleHandleError",
    "TimeRange",
    "Topic",
    "Trace",
    "TraceCollection",
    "UnavailableError",
    "Vehicle",
    "VehicleCollection",
    "VehicleProfile",
    "VehicleProfileCollection",
    "Window",
    "WindowCollection",
    "Workspace",
]


def _exports() -> list[tuple[str, Any]]:
    return [(name, getattr(delog_client, name)) for name in delog_client.__all__]


def _public_callables(cls: type) -> list[tuple[str, Any]]:
    members = []
    for name, member in vars(cls).items():
        if name.startswith("_") and name not in {"__init__", "__enter__", "__exit__"}:
            continue
        if isinstance(member, (staticmethod, classmethod)):
            member = member.__func__
        if isinstance(member, property):
            member = member.fget
        if inspect.isfunction(member):
            members.append((name, member))
    return members


def test_public_surface_is_deliberate() -> None:
    assert delog_client.__all__ == EXPORTS
    assert delog_client.__all__ == sorted(delog_client.__all__)


def test_every_export_reports_the_package_module() -> None:
    for name, value in _exports():
        assert value.__module__ == "delog_client", name
        assert value.__qualname__ == name, name


def test_documented_methods_are_fully_annotated() -> None:
    missing = []
    for class_name, cls in _exports():
        for method_name, method in _public_callables(cls):
            if method.__module__ == "builtins" or not method.__module__.startswith("delog_client"):
                continue
            signature = inspect.signature(method)
            where = f"{class_name}.{method_name}"
            if signature.return_annotation is inspect.Signature.empty:
                missing.append(f"{where} -> return")
            for parameter in list(signature.parameters.values())[1:]:
                if parameter.annotation is inspect.Parameter.empty:
                    missing.append(f"{where}({parameter.name})")
    assert missing == []


def test_secret_bearing_objects_redact_their_repr(
    server: FakeInstance, mock_client: Client
) -> None:
    [instance] = DeLOG.list_instances()
    secrets = [server.bootstrap, *server.client_tokens]
    assert secrets[1:], "the fake server issued no client token"
    for rendered in (repr(mock_client), str(mock_client), repr(instance), str(instance)):
        for secret in secrets:
            assert secret not in rendered
