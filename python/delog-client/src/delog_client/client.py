from __future__ import annotations

import weakref
from collections.abc import Callable, Mapping
from types import TracebackType
from typing import Any
from urllib.parse import quote

import pyarrow

from . import discovery
from .control import (
    ControlBatch,
    ControlState,
    LayoutCollection,
    MarkerCollection,
    Playback,
    VehicleCollection,
    VehicleProfileCollection,
    WindowCollection,
    Workspace,
    _Resource,
)
from .errors import DeLOGError, ForbiddenError, InvalidInputError, UnavailableError
from .models import Instance, RemovalReport, RequestStatus
from .publication import (
    Publication,
    _ArrowUploadStream,
    prepare_upload,
    validate_topic_name,
)
from .snapshot import Snapshot
from .transport import Transport, checked_idempotency_key


class Client:
    __slots__ = (
        "_instance",
        "_transport",
        "_closed",
        "_batch",
        "_resources",
        "_publications",
        "_windows",
        "_workspace",
        "_markers",
        "_vehicles",
        "_vehicle_profiles",
        "_layouts",
        "_playback",
    )

    def __init__(self, *, instance: Instance, transport: Transport) -> None:
        self._instance = instance
        self._transport = transport
        self._closed = False
        self._batch: ControlBatch | None = None
        self._resources: weakref.WeakSet[_Resource] = weakref.WeakSet()
        self._publications: weakref.WeakSet[Publication] = weakref.WeakSet()
        self._windows = WindowCollection(self)
        self._workspace = Workspace(self)
        self._markers = MarkerCollection(self)
        self._vehicles = VehicleCollection(self)
        self._vehicle_profiles = VehicleProfileCollection(self)
        self._layouts = LayoutCollection(self)
        self._playback = Playback(self)

    @property
    def instance(self) -> Instance:
        return self._instance

    @property
    def name(self) -> str:
        return self._transport.registration.owner_name

    @property
    def client_id(self) -> str:
        return self._transport.registration.client_id

    @property
    def owner_id(self) -> str:
        return self._transport.registration.owner_id

    @property
    def closed(self) -> bool:
        return self._closed

    @property
    def windows(self) -> WindowCollection:
        return self._windows

    @property
    def workspace(self) -> Workspace:
        return self._workspace

    @property
    def markers(self) -> MarkerCollection:
        return self._markers

    @property
    def vehicles(self) -> VehicleCollection:
        return self._vehicles

    @property
    def vehicle_profiles(self) -> VehicleProfileCollection:
        return self._vehicle_profiles

    @property
    def layouts(self) -> LayoutCollection:
        return self._layouts

    @property
    def playback(self) -> Playback:
        return self._playback

    def __repr__(self) -> str:
        return (
            f"Client(instance={self._instance.id!r}, name={self.name!r}, "
            f"client_id={self.client_id!r}, closed={self._closed!r})"
        )

    def __enter__(self) -> Client:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self.close()

    def snapshot(self) -> Snapshot:
        self._guard("snapshot()")
        payload = self._transport.post_json("/v1/snapshots", {})
        return Snapshot.from_payload(self, self._transport, payload)

    def publish_topic(
        self,
        name: str,
        data: pyarrow.Table | pyarrow.RecordBatchReader,
        *,
        units: Mapping[str, str] | None = None,
        descriptions: Mapping[str, str] | None = None,
        multipliers: Mapping[str, float] | None = None,
        replace: bool = False,
        idempotency_key: str | None = None,
    ) -> Publication:
        self._guard("publish_topic()")
        topic = validate_topic_name(name)
        if not isinstance(replace, bool):
            raise InvalidInputError("replace must be True or False")
        key = checked_idempotency_key(idempotency_key)
        schema, batches, replayable = prepare_upload(data, units, descriptions, multipliers)
        payload = self._transport.put_arrow(
            f"/v1/publications/{quote(topic, safe='')}",
            lambda: _ArrowUploadStream(schema, batches()),
            params={"replace": str(replace).lower()},
            idempotency_key=key,
            replayable=replayable,
        )
        return Publication.from_payload(self, payload)

    def state(self) -> ControlState:
        self._guard("state()")
        return ControlState.from_payload(self, self._transport.get_json("/v1/control/state"))

    def batch(self, *, idempotency_key: str | None = None) -> ControlBatch:
        self._guard("batch()")
        return ControlBatch(self, idempotency_key)

    def remove_owned(self, *, idempotency_key: str | None = None) -> RemovalReport:
        self._guard("remove_owned()")
        try:
            payload = self._transport.control_mutation(
                {"op": "remove_owned"}, idempotency_key=idempotency_key
            )
        except DeLOGError as error:
            report = RemovalReport.from_error(error)
            if report is not None:
                error.report = report
                if error.completion == "committed":
                    if "ui" not in report.failed:
                        self._invalidate(lambda resource: True)
                    if "publications" not in report.failed:
                        self._invalidate_publications()
            raise
        report = RemovalReport.from_payload(payload)
        self._invalidate(lambda resource: True)
        self._invalidate_publications()
        return report

    def request_status(
        self, request_id: str | None = None, *, idempotency_key: str | None = None
    ) -> RequestStatus:
        self._guard("request_status()")
        return self._transport.request_status(request_id, idempotency_key=idempotency_key)

    def _ensure_usable(self) -> None:
        if self._closed:
            raise ForbiddenError("this client is closed; connect again")

    def _guard(self, operation: str) -> None:
        self._ensure_usable()
        if self._batch is not None:
            raise InvalidInputError(
                f"{operation} cannot run inside client.batch(); a batch only accepts "
                "response-free commands, so run it before or after the batch"
            )

    def _mutate(self, command: dict[str, Any], *, idempotency_key: str | None = None) -> Any:
        self._guard(str(command.get("op")))
        return self._transport.control_mutation(command, idempotency_key=idempotency_key)

    def _submit(
        self,
        command: dict[str, Any],
        *,
        idempotency_key: str | None = None,
        on_commit: Callable[[], None] | None = None,
    ) -> None:
        self._ensure_usable()
        batch = self._batch
        if batch is not None:
            if idempotency_key is not None:
                raise InvalidInputError(
                    "commands inside client.batch() share the batch idempotency key; "
                    "pass idempotency_key to client.batch() instead"
                )
            batch._add(command, on_commit)
            return
        self._transport.control_mutation(command, idempotency_key=idempotency_key)
        if on_commit is not None:
            on_commit()

    def _query(self, command: dict[str, Any]) -> Any:
        self._guard(str(command.get("op")))
        return self._transport.control_query(command)

    def _begin_batch(self, batch: ControlBatch) -> None:
        self._guard("batch()")
        self._batch = batch

    def _end_batch(self, batch: ControlBatch) -> None:
        if self._batch is batch:
            self._batch = None

    def _track(self, resource: _Resource) -> None:
        self._resources.add(resource)

    def _track_publication(self, publication: Publication) -> None:
        self._publications.add(publication)

    def _invalidate(self, predicate: Callable[[_Resource], bool]) -> None:
        for resource in list(self._resources):
            if predicate(resource):
                resource._invalidate()

    def _invalidate_publications(self) -> None:
        for publication in list(self._publications):
            publication._invalidate()

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        try:
            self._transport.delete(f"/v1/clients/{self.client_id}")
        except (ForbiddenError, UnavailableError):
            pass
        finally:
            self._transport.close()


class DeLOG:
    @staticmethod
    def list_instances(*, timeout: float = discovery.HEALTH_TIMEOUT) -> list[Instance]:
        return discovery.list_instances(timeout)

    @staticmethod
    def connect(
        instance_id: str,
        *,
        name: str,
        takeover: bool = False,
        timeout: float = 10.0,
    ) -> Client:
        descriptor = discovery.resolve(instance_id, min(timeout, discovery.HEALTH_TIMEOUT))
        transport = Transport.connect(
            descriptor.instance,
            descriptor.bootstrap,
            name=name,
            takeover=takeover,
            timeout=timeout,
        )
        return Client(instance=descriptor.instance, transport=transport)
