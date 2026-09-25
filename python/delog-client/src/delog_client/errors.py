from __future__ import annotations

import json
from collections.abc import Sequence
from typing import TYPE_CHECKING, Any, ClassVar

if TYPE_CHECKING:
    from .models import RemovalReport

COMPLETIONS = frozenset({"not_started", "committed", "unknown"})


class DeLOGError(Exception):
    wire_code: ClassVar[str] = "unknown"
    default_retryable: ClassVar[bool] = False

    def __init__(
        self,
        message: str,
        *,
        code: str | None = None,
        request_id: str | None = None,
        retryable: bool | None = None,
        completion: str | None = None,
        details: Any = None,
        status_code: int | None = None,
    ) -> None:
        super().__init__(message)
        self.message = message
        self.code = code if code is not None else type(self).wire_code
        self.request_id = request_id
        self.retryable = type(self).default_retryable if retryable is None else retryable
        self.completion = completion
        self.details = details
        self.status_code = status_code
        self.report: RemovalReport | None = None

    def __str__(self) -> str:
        if self.request_id is None:
            return self.message
        return f"{self.message} (request {self.request_id})"

    def __repr__(self) -> str:
        return (
            f"{type(self).__name__}(code={self.code!r}, message={self.message!r}, "
            f"request_id={self.request_id!r}, retryable={self.retryable!r}, "
            f"completion={self.completion!r})"
        )


class InvalidInputError(DeLOGError):
    wire_code = "invalid_input"


class NotFoundError(DeLOGError):
    wire_code = "not_found"

    @classmethod
    def for_topic(cls, name: str) -> NotFoundError:
        return cls(f"no topic named {name!r} in this snapshot")


class AmbiguousError(DeLOGError):
    wire_code = "ambiguous"

    def __init__(self, message: str, **kwargs: Any) -> None:
        super().__init__(message, **kwargs)
        candidates = self.details.get("candidates") if isinstance(self.details, dict) else None
        if isinstance(candidates, list) and all(isinstance(c, str) for c in candidates):
            self.candidates: list[str] = list(candidates)
        else:
            self.candidates = []

    @classmethod
    def for_candidates(cls, what: str, name: str, candidates: Sequence[str]) -> AmbiguousError:
        ordered = sorted(candidates)
        return cls(
            f"{what} {name!r} is ambiguous; qualify it to choose one of: {', '.join(ordered)}",
            details={"candidates": ordered},
        )

    @classmethod
    def for_topics(cls, name: str, candidates: Sequence[str]) -> AmbiguousError:
        return cls.for_candidates("topic", name, candidates)


class StaleHandleError(DeLOGError):
    wire_code = "stale_handle"


class ForbiddenError(DeLOGError):
    wire_code = "forbidden"


class SnapshotExpiredError(DeLOGError):
    wire_code = "snapshot_expired"


class ConflictError(DeLOGError):
    wire_code = "conflict"


class UnavailableError(DeLOGError):
    wire_code = "unavailable"
    default_retryable = True


class InternalError(DeLOGError):
    wire_code = "internal"


class IncompatibleVersionError(DeLOGError):
    wire_code = "incompatible_version"


ERRORS_BY_CODE: dict[str, type[DeLOGError]] = {
    kind.wire_code: kind
    for kind in (
        InvalidInputError,
        NotFoundError,
        AmbiguousError,
        StaleHandleError,
        ForbiddenError,
        SnapshotExpiredError,
        ConflictError,
        UnavailableError,
        InternalError,
    )
}

ERRORS_BY_STATUS: dict[int, type[DeLOGError]] = {
    400: InvalidInputError,
    403: ForbiddenError,
    404: NotFoundError,
    409: ConflictError,
    410: SnapshotExpiredError,
    503: UnavailableError,
}


def from_envelope(status_code: int, body: bytes) -> DeLOGError:
    envelope = _parse_envelope(body)
    if envelope is None:
        kind = ERRORS_BY_STATUS.get(status_code, InternalError)
        return kind(f"DeLOG returned HTTP {status_code}", status_code=status_code)
    code, message, request_id, retryable, details, completion = envelope
    kind = ERRORS_BY_CODE.get(code, DeLOGError)
    return kind(
        message,
        code=code,
        request_id=request_id,
        retryable=retryable,
        completion=completion,
        details=details,
        status_code=status_code,
    )


def _parse_envelope(
    body: bytes,
) -> tuple[str, str, str, bool, Any, str | None] | None:
    try:
        payload = json.loads(body)
    except ValueError:
        return None
    if not isinstance(payload, dict):
        return None
    code = payload.get("code")
    message = payload.get("message")
    request_id = payload.get("request_id")
    retryable = payload.get("retryable")
    completion = payload.get("completion")
    if not (
        isinstance(code, str)
        and isinstance(message, str)
        and isinstance(request_id, str)
        and isinstance(retryable, bool)
    ):
        return None
    if completion not in COMPLETIONS:
        completion = None
    return code, message, request_id, retryable, payload.get("details"), completion
