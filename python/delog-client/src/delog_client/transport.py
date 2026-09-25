from __future__ import annotations

import io
import json
import secrets
from collections.abc import Callable, Generator, Iterator, Mapping
from contextlib import contextmanager
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

import httpx
import pyarrow
import pyarrow.ipc

from .errors import (
    DeLOGError,
    InternalError,
    InvalidInputError,
    NotFoundError,
    SnapshotExpiredError,
    UnavailableError,
    from_envelope,
)
from .models import WIRE_HANDLE, RequestStatus

if TYPE_CHECKING:
    from _typeshed import WriteableBuffer

    from .models import Instance

ARROW_STREAM = "application/vnd.apache.arrow.stream"
JSON = "application/json"
MAX_ERROR_BODY = 64 * 1024
IDEMPOTENCY_KEY = "Idempotency-Key"
REQUEST_ID = "x-request-id"
MAX_KEY_LENGTH = 128
MAX_ATTEMPTS = 3
UNSENT = (httpx.ConnectError, httpx.ConnectTimeout, httpx.PoolTimeout)


def new_idempotency_key() -> str:
    return secrets.token_urlsafe(24)


def checked_idempotency_key(key: Any) -> str:
    if key is None:
        return new_idempotency_key()
    if (
        not isinstance(key, str)
        or not 1 <= len(key) <= MAX_KEY_LENGTH
        or not all("!" <= char <= "~" for char in key)
    ):
        raise InvalidInputError(
            f"idempotency_key must be 1 to {MAX_KEY_LENGTH} visible ASCII characters"
        )
    return key


def encode_json(body: Any) -> bytes:
    try:
        text = json.dumps(body, separators=(",", ":"), ensure_ascii=False, allow_nan=False)
    except (TypeError, ValueError) as error:
        raise InvalidInputError(f"the request cannot be encoded as JSON: {error}") from None
    return text.encode("utf-8")


class UploadBody(httpx.SyncByteStream):
    failure: BaseException | None = None


class _Lost(Exception):
    def __init__(self, request_id: str | None, sent: bool) -> None:
        super().__init__("the connection to DeLOG was lost")
        self.request_id = request_id
        self.sent = sent


def _http_transport() -> httpx.BaseTransport | None:
    return None


class Secret:
    __slots__ = ("_value",)

    def __init__(self, value: str) -> None:
        self._value = value

    def reveal(self) -> str:
        return self._value

    def __repr__(self) -> str:
        return "Secret([REDACTED])"

    __str__ = __repr__

    def __reduce__(self) -> Any:
        raise TypeError("secrets cannot be pickled")


@dataclass(frozen=True)
class Registration:
    client_id: str
    owner_id: str
    owner_name: str


def _open_http(endpoint: str, timeout: float) -> httpx.Client:
    return httpx.Client(
        base_url=f"http://{endpoint}",
        timeout=timeout,
        trust_env=False,
        follow_redirects=False,
        transport=_http_transport(),
    )


def _auth(secret: Secret, extra: Mapping[str, str] | None = None) -> dict[str, str]:
    headers = {"Authorization": f"Bearer {secret.reveal()}"}
    if extra:
        headers.update(extra)
    return headers


def _send(http: httpx.Client, request: httpx.Request, *, stream: bool = False) -> httpx.Response:
    try:
        return http.send(request, stream=stream)
    except httpx.HTTPError as error:
        raise UnavailableError(
            f"could not reach DeLOG at {request.url.netloc.decode()}: {type(error).__name__}"
        ) from None


def _error_from(response: httpx.Response) -> DeLOGError:
    try:
        return from_envelope(response.status_code, response.content[:MAX_ERROR_BODY])
    except httpx.ResponseNotRead:
        pass
    body = bytearray()
    try:
        for chunk in response.iter_raw():
            body.extend(chunk[: MAX_ERROR_BODY - len(body)])
            if len(body) >= MAX_ERROR_BODY:
                break
    except httpx.HTTPError:
        pass
    return from_envelope(response.status_code, bytes(body))


def _json_from(response: httpx.Response) -> Any:
    if not response.is_success:
        raise _error_from(response)
    try:
        return json.loads(response.content)
    except ValueError:
        raise InternalError("DeLOG returned a response that is not valid JSON") from None


def probe_instance(endpoint: str, bootstrap: Secret, timeout: float) -> Any:
    with _open_http(endpoint, timeout) as http:
        request = http.build_request("GET", "/v1/instance", headers=_auth(bootstrap))
        response = _send(http, request)
        return _json_from(response)


class ChunkReader(io.RawIOBase):
    def __init__(self, chunks: Iterator[bytes]) -> None:
        super().__init__()
        self._chunks = chunks
        self._pending = memoryview(b"")
        self._eof = False
        self.failure: Exception | None = None
        self.peak_pending_bytes = 0

    @property
    def pending_bytes(self) -> int:
        return len(self._pending)

    def readable(self) -> bool:
        return True

    def seekable(self) -> bool:
        return False

    def readinto(self, buffer: WriteableBuffer) -> int:
        view = memoryview(buffer).cast("B")
        filled = 0
        while filled < len(view):
            if not self._pending:
                if not self._pull():
                    break
            count = min(len(self._pending), len(view) - filled)
            view[filled : filled + count] = self._pending[:count]
            self._pending = self._pending[count:]
            filled += count
        return filled

    def _pull(self) -> bool:
        if self._eof:
            return False
        while True:
            try:
                chunk = next(self._chunks)
            except StopIteration:
                self._eof = True
                return False
            except httpx.HTTPError as error:
                self._eof = True
                self.failure = error
                raise OSError("the DeLOG data stream was interrupted") from None
            if chunk:
                self._pending = memoryview(chunk)
                self.peak_pending_bytes = max(self.peak_pending_bytes, len(chunk))
                return True


class Transport:
    def __init__(self, http: httpx.Client, token: Secret, registration: Registration) -> None:
        self._http = http
        self._token = token
        self.registration = registration
        self._closed = False

    @property
    def closed(self) -> bool:
        return self._closed

    def ensure_open(self) -> None:
        if self._closed:
            raise SnapshotExpiredError(
                "the client is closed and its snapshots were released; connect again",
                retryable=False,
            )

    def __repr__(self) -> str:
        endpoint = str(self._http.base_url)
        return f"Transport(endpoint={endpoint!r}, client_id={self.registration.client_id!r})"

    @classmethod
    def connect(
        cls,
        instance: Instance,
        bootstrap: Secret,
        *,
        name: str,
        takeover: bool,
        timeout: float,
    ) -> Transport:
        http = _open_http(instance.endpoint, timeout)
        try:
            request = http.build_request(
                "POST",
                "/v1/clients",
                json={"name": name, "takeover": takeover},
                headers=_auth(bootstrap),
            )
            payload = _json_from(_send(http, request))
            if not isinstance(payload, dict):
                raise InternalError("DeLOG returned a malformed client registration")
            fields = [payload.get(key) for key in ("client_id", "owner_id", "owner_name", "token")]
            if not all(isinstance(value, str) and value for value in fields):
                raise InternalError("DeLOG returned a malformed client registration")
            client_id, owner_id, owner_name, token = (str(value) for value in fields)
        except BaseException:
            http.close()
            raise
        return cls(http, Secret(token), Registration(client_id, owner_id, owner_name))

    def get_json(self, path: str, headers: Mapping[str, str] | None = None) -> Any:
        return self._request_json("GET", path, headers=headers)

    def post_json(self, path: str, body: Any) -> Any:
        return self._request_json("POST", path, body)

    def delete(self, path: str) -> Any:
        return self._request_json("DELETE", path)

    def close(self) -> None:
        self._closed = True
        self._http.close()

    def _request_json(
        self,
        method: str,
        path: str,
        body: Any = None,
        headers: Mapping[str, str] | None = None,
    ) -> Any:
        self.ensure_open()
        request = self._http.build_request(
            method,
            path,
            json=body,
            headers=_auth(self._token, {"Accept": JSON, **(headers or {})}),
        )
        return _json_from(_send(self._http, request))

    def request_status(
        self, request_id: str | None = None, *, idempotency_key: str | None = None
    ) -> RequestStatus:
        if request_id is not None:
            if not isinstance(request_id, str) or WIRE_HANDLE.fullmatch(request_id) is None:
                raise InvalidInputError("request_id must be a request ID returned by DeLOG")
            return RequestStatus.from_payload(self.get_json(f"/v1/requests/{request_id}"))
        if idempotency_key is None:
            raise InvalidInputError("pass a request_id or an idempotency_key")
        key = checked_idempotency_key(idempotency_key)
        payload = self.get_json("/v1/requests/by-key", {IDEMPOTENCY_KEY: key})
        return RequestStatus.from_payload(payload)

    def control_query(self, command: Mapping[str, Any]) -> Any:
        return self._request_json("POST", "/v1/control", dict(command))

    def control_mutation(
        self, command: Mapping[str, Any], *, idempotency_key: str | None = None
    ) -> Any:
        return self.post_mutation("/v1/control", command, idempotency_key=idempotency_key)

    def post_mutation(self, path: str, body: Any, *, idempotency_key: str | None = None) -> Any:
        content = encode_json(body)
        return self.mutate(
            "POST", path, lambda: content, content_type=JSON, idempotency_key=idempotency_key
        )

    def delete_mutation(self, path: str, *, idempotency_key: str | None = None) -> Any:
        return self.mutate("DELETE", path, lambda: b"", idempotency_key=idempotency_key)

    def put_arrow(
        self,
        path: str,
        streams: Callable[[], UploadBody],
        *,
        params: Mapping[str, str],
        idempotency_key: str | None = None,
        replayable: bool = True,
    ) -> Any:
        return self.mutate(
            "PUT",
            path,
            streams,
            content_type=ARROW_STREAM,
            params=params,
            idempotency_key=idempotency_key,
            replayable=replayable,
        )

    def mutate(
        self,
        method: str,
        path: str,
        body: Callable[[], bytes | UploadBody],
        *,
        content_type: str | None = None,
        params: Mapping[str, str] | None = None,
        idempotency_key: str | None = None,
        replayable: bool = True,
    ) -> Any:
        key = checked_idempotency_key(idempotency_key)
        self.ensure_open()
        headers = {"Accept": JSON, IDEMPOTENCY_KEY: key}
        if content_type is not None:
            headers["Content-Type"] = content_type
        attempt = 0
        while True:
            attempt += 1
            try:
                return self._exchange(method, path, body(), headers, params)
            except _Lost as lost:
                if not lost.sent:
                    raise UnavailableError(
                        "could not reach DeLOG; the request was not sent",
                        completion="not_started",
                        details={"idempotency_key": key},
                    ) from None
                status = self._reconcile(key, lost.request_id)
            if status is None:
                if not replayable:
                    raise UnavailableError(
                        "the connection was lost before DeLOG started the request; "
                        "a RecordBatchReader cannot be replayed, so pass a fresh reader",
                        completion="not_started",
                        details={"idempotency_key": key},
                    )
                if attempt >= MAX_ATTEMPTS:
                    raise UnavailableError(
                        f"DeLOG did not receive the request after {attempt} attempts",
                        completion="not_started",
                        details={"idempotency_key": key},
                    )
                continue
            if status.state == "completed":
                return status.result()
            if status.state == "in_flight" and replayable and attempt < MAX_ATTEMPTS:
                continue
            raise UnavailableError(
                f"the outcome of request {status.request_id} is unknown and it was not "
                "repeated; check client.request_status() before acting again",
                request_id=status.request_id,
                completion="unknown",
                details={"idempotency_key": key, "state": status.state},
            )

    def _exchange(
        self,
        method: str,
        path: str,
        content: bytes | UploadBody,
        headers: Mapping[str, str],
        params: Mapping[str, str] | None,
    ) -> Any:
        upload = content if isinstance(content, UploadBody) else None
        request = self._http.build_request(
            method,
            path,
            params=dict(params) if params is not None else None,
            content=b"" if upload is not None else content,
            headers=_auth(self._token, headers),
        )
        if upload is not None:
            upload_headers = httpx.Headers(request.headers)
            upload_headers.pop("Content-Length", None)
            upload_headers["Transfer-Encoding"] = "chunked"
            request = httpx.Request(method, request.url, headers=upload_headers, stream=upload)
        try:
            try:
                response = self._http.send(request, stream=True)
            except httpx.HTTPError as error:
                _raise_upload_failure(upload)
                raise _Lost(None, sent=not isinstance(error, UNSENT)) from None
            except Exception:
                _raise_upload_failure(upload)
                raise
            try:
                request_id = response.headers.get(REQUEST_ID)
                try:
                    response.read()
                except httpx.HTTPError:
                    raise _Lost(request_id, sent=True) from None
            finally:
                response.close()
        finally:
            if upload is not None:
                upload.close()
        return _json_from(response)

    def _reconcile(self, key: str, request_id: str | None) -> RequestStatus | None:
        try:
            if request_id is not None:
                try:
                    return self.request_status(request_id)
                except NotFoundError:
                    pass
            try:
                return self.request_status(idempotency_key=key)
            except NotFoundError:
                return None
        except DeLOGError as error:
            raise UnavailableError(
                "the connection to DeLOG was lost and whether the request ran could not be "
                f"confirmed ({error.message}); check client.request_status() before acting again",
                request_id=request_id,
                completion="unknown",
                details={"idempotency_key": key},
            ) from None

    @contextmanager
    def _arrow_reader(
        self, path: str, params: Mapping[str, str]
    ) -> Iterator[tuple[pyarrow.ipc.RecordBatchStreamReader, ChunkReader]]:
        self.ensure_open()
        request = self._http.build_request(
            "GET",
            path,
            params=dict(params),
            headers=_auth(self._token, {"Accept": ARROW_STREAM, "Accept-Encoding": "identity"}),
        )
        response = _send(self._http, request, stream=True)
        try:
            if not response.is_success:
                raise _error_from(response)
            content_type = response.headers.get("content-type", "").split(";")[0].strip()
            if content_type != ARROW_STREAM:
                raise InternalError(
                    f"DeLOG returned {content_type or 'no content type'} instead of an Arrow stream"
                )
            source = ChunkReader(response.iter_raw())
            try:
                reader = pyarrow.ipc.open_stream(source)
            except (pyarrow.ArrowException, OSError) as error:
                raise _stream_error(source, error) from None
            yield reader, source
        finally:
            response.close()

    def iter_arrow(
        self, path: str, params: Mapping[str, str]
    ) -> Generator[pyarrow.RecordBatch, None, None]:
        with self._arrow_reader(path, params) as (reader, source):
            while True:
                try:
                    batch = reader.read_next_batch()
                except StopIteration:
                    return
                except (pyarrow.ArrowException, OSError) as error:
                    raise _stream_error(source, error) from None
                yield batch

    def read_arrow_table(self, path: str, params: Mapping[str, str]) -> pyarrow.Table:
        with self._arrow_reader(path, params) as (reader, source):
            try:
                return reader.read_all()
            except (pyarrow.ArrowException, OSError) as error:
                raise _stream_error(source, error) from None


def _raise_upload_failure(upload: UploadBody | None) -> None:
    if upload is None or upload.failure is None:
        return
    failure = upload.failure
    raise InvalidInputError(
        f"the Arrow upload could not be encoded: {failure}", completion="not_started"
    ) from failure


def _stream_error(source: ChunkReader, error: BaseException) -> DeLOGError:
    if source.failure is not None:
        return UnavailableError("the DeLOG data stream was interrupted before it completed")
    return InternalError(f"DeLOG returned an invalid Arrow stream: {type(error).__name__}")
