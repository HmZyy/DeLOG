from __future__ import annotations

import logging

import httpx
import pytest

from delog_client import (
    AmbiguousError,
    Client,
    ConflictError,
    DeLOG,
    DeLOGError,
    ForbiddenError,
    InternalError,
    InvalidInputError,
    NotFoundError,
    SnapshotExpiredError,
    StaleHandleError,
    UnavailableError,
)
from delog_client.errors import from_envelope

from conftest import FakeInstance, FakeNetwork

WIRE = [
    ("invalid_input", 400, InvalidInputError),
    ("not_found", 404, NotFoundError),
    ("ambiguous", 409, AmbiguousError),
    ("stale_handle", 409, StaleHandleError),
    ("forbidden", 403, ForbiddenError),
    ("snapshot_expired", 410, SnapshotExpiredError),
    ("conflict", 409, ConflictError),
    ("unavailable", 503, UnavailableError),
    ("internal", 500, InternalError),
]


@pytest.mark.parametrize(("code", "status", "kind"), WIRE)
def test_wire_codes_map_to_typed_exceptions(
    mock_client: Client, server: FakeInstance, code: str, status: int, kind: type[DeLOGError]
) -> None:
    server.overrides[("POST", "/v1/snapshots")] = httpx.Response(
        status,
        json={
            "request_id": "req-1",
            "code": code,
            "message": f"{code} happened",
            "retryable": code == "unavailable",
            "details": {"why": code},
            "completion": "unknown",
        },
    )
    with pytest.raises(kind) as caught:
        mock_client.snapshot()
    error = caught.value
    assert isinstance(error, DeLOGError)
    assert error.code == code
    assert error.message == f"{code} happened"
    assert error.request_id == "req-1"
    assert error.retryable is (code == "unavailable")
    assert error.details == {"why": code}
    assert error.completion == "unknown"
    assert error.status_code == status
    assert "req-1" in str(error)


def test_optional_envelope_fields_default(mock_client: Client, server: FakeInstance) -> None:
    server.overrides[("POST", "/v1/snapshots")] = httpx.Response(
        409,
        json={"request_id": "r", "code": "conflict", "message": "busy", "retryable": False},
    )
    with pytest.raises(ConflictError) as caught:
        mock_client.snapshot()
    assert caught.value.details is None
    assert caught.value.completion is None


def test_unknown_wire_code_keeps_code(mock_client: Client, server: FakeInstance) -> None:
    server.overrides[("POST", "/v1/snapshots")] = httpx.Response(
        418,
        json={
            "request_id": "r",
            "code": "teapot",
            "message": "short and stout",
            "retryable": False,
        },
    )
    with pytest.raises(DeLOGError) as caught:
        mock_client.snapshot()
    assert type(caught.value) is DeLOGError
    assert caught.value.code == "teapot"


@pytest.mark.parametrize(
    ("status", "kind"),
    [
        (400, InvalidInputError),
        (403, ForbiddenError),
        (404, NotFoundError),
        (410, SnapshotExpiredError),
        (503, UnavailableError),
        (502, InternalError),
    ],
)
def test_non_envelope_errors_fall_back_to_status(
    mock_client: Client, server: FakeInstance, status: int, kind: type[DeLOGError]
) -> None:
    server.overrides[("POST", "/v1/snapshots")] = httpx.Response(status, text="<html>nope</html>")
    with pytest.raises(kind) as caught:
        mock_client.snapshot()
    assert caught.value.request_id is None
    assert caught.value.status_code == status


def test_ambiguous_wire_details_expose_candidates(
    mock_client: Client, server: FakeInstance
) -> None:
    server.overrides[("POST", "/v1/snapshots")] = httpx.Response(
        409,
        json={
            "request_id": "r",
            "code": "ambiguous",
            "message": "pick one",
            "retryable": False,
            "details": {"candidates": ["b/x", "a/x"]},
        },
    )
    with pytest.raises(AmbiguousError) as caught:
        mock_client.snapshot()
    assert caught.value.candidates == ["b/x", "a/x"]


def test_malformed_success_payload_is_internal(mock_client: Client, server: FakeInstance) -> None:
    server.overrides[("POST", "/v1/snapshots")] = httpx.Response(200, json={"lease": 3})
    with pytest.raises(InternalError):
        mock_client.snapshot()


def test_transport_failures_are_unavailable_without_tokens(server: FakeInstance) -> None:
    client = DeLOG.connect(server.instance_id, name="job")
    token = next(iter(server.client_tokens))
    server.reachable = False
    with pytest.raises(UnavailableError) as caught:
        client.snapshot()
    error = caught.value
    assert error.retryable
    for text in (str(error), repr(error), repr(error.details)):
        assert token not in text
        assert server.bootstrap not in text
    assert error.__cause__ is None
    server.reachable = True
    client.close()


def test_errors_never_carry_request_headers(mock_client: Client, server: FakeInstance) -> None:
    token = next(iter(server.client_tokens))
    server.overrides[("POST", "/v1/snapshots")] = httpx.Response(
        403, json={"request_id": "r", "code": "forbidden", "message": "no", "retryable": False}
    )
    with pytest.raises(ForbiddenError) as caught:
        mock_client.snapshot()
    error = caught.value
    assert token not in repr(error)
    assert token not in str(error)
    assert token not in repr(vars(error))
    assert error.__cause__ is None


def test_logs_never_contain_tokens(server: FakeInstance, caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.DEBUG)
    instances = DeLOG.list_instances()
    client = DeLOG.connect(instances[0].id, name="job")
    token = next(iter(server.client_tokens))
    with client.snapshot() as snapshot:
        snapshot.topic("vehicle_attitude", source="flight").read()
    client.close()
    assert caplog.records
    for record in caplog.records:
        text = record.getMessage() + repr(record.args)
        assert server.bootstrap not in text
        assert token not in text


def test_disconnected_instance_errors_are_readable(network: FakeNetwork) -> None:
    instance = network.add(reachable=False)
    network.publish(instance)
    assert DeLOG.list_instances() == []
    with pytest.raises(NotFoundError) as caught:
        DeLOG.connect(instance.instance_id, name="job")
    assert instance.bootstrap not in str(caught.value)


@pytest.mark.parametrize("code, status, kind", WIRE)
def test_default_retryability_matches_the_wire(
    code: str, status: int, kind: type[DeLOGError]
) -> None:
    assert kind("x").retryable is (code == "unavailable")
    fallback = from_envelope(status, b"not json")
    assert fallback.retryable is (fallback.code == "unavailable")
