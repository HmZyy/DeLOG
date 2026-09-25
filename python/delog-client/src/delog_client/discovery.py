from __future__ import annotations

import base64
import binascii
import json
import os
import re
import stat
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from . import _windows
from .errors import DeLOGError, ForbiddenError, IncompatibleVersionError, NotFoundError
from .models import Instance
from .transport import Secret, probe_instance

OVERRIDE_ENV = "DELOG_DISCOVERY_DIR"
DESCRIPTOR_FORMAT = 1
CLIENT_API_MAJOR = 1
CLIENT_API_MINOR = 0
MAX_DESCRIPTOR_BYTES = 64 * 1024
MAX_ID_LEN = 128
TOKEN_BYTES = 32
HEALTH_TIMEOUT = 2.0

REQUIRED_KEYS = frozenset(
    {
        "format",
        "instance_id",
        "label",
        "pid",
        "endpoint",
        "api_major",
        "api_min_minor",
        "api_max_minor",
        "bootstrap_token",
    }
)
OPTIONAL_KEYS = frozenset({"session_description"})
ID_PATTERN = re.compile(rf"[A-Za-z0-9_-]{{1,{MAX_ID_LEN}}}")
ENDPOINT_PATTERN = re.compile(r"127\.0\.0\.1:([0-9]{1,5})")
TOKEN_PATTERN = re.compile(r"[A-Za-z0-9_-]+")
WINDOWS_REPARSE_POINT = 0x400


@dataclass(frozen=True)
class Descriptor:
    instance: Instance
    bootstrap: Secret

    @property
    def compatible(self) -> bool:
        instance = self.instance
        return (
            instance.api_major == CLIENT_API_MAJOR
            and instance.api_min_minor <= CLIENT_API_MINOR <= instance.api_max_minor
        )


def discovery_root() -> Path | None:
    override = os.environ.get(OVERRIDE_ENV)
    if override:
        return Path(override)
    if sys.platform == "win32":
        local = os.environ.get("LOCALAPPDATA")
        if not local or not Path(local).is_absolute():
            return None
        return Path(local) / "DeLOG" / "runtime" / "instances"
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if runtime and Path(runtime).is_absolute():
        return Path(runtime) / "delog" / "instances"
    temp = Path(os.environ.get("TMPDIR", "/tmp"))
    base = temp / f"delog-runtime-{os.geteuid()}"
    if not base.is_absolute():
        return None
    return base / "instances"


def list_instances(timeout: float = HEALTH_TIMEOUT) -> list[Instance]:
    live = [
        descriptor.instance
        for descriptor in _descriptors()
        if descriptor.compatible and _healthy(descriptor, timeout)
    ]
    return sorted(live, key=lambda instance: (instance.label, instance.id))


def resolve(instance_id: str, timeout: float = HEALTH_TIMEOUT) -> Descriptor:
    if not isinstance(instance_id, str) or ID_PATTERN.fullmatch(instance_id) is None:
        raise NotFoundError(f"no DeLOG instance with id {instance_id!r}")
    root = discovery_root()
    descriptor = None
    if root is not None and _root_exists(root):
        descriptor = _read_descriptor(root / f"{instance_id}.json", instance_id)
    if descriptor is None:
        raise NotFoundError(f"no DeLOG instance with id {instance_id!r}")
    if not descriptor.compatible:
        instance = descriptor.instance
        raise IncompatibleVersionError(
            f"DeLOG instance {instance_id!r} speaks API {instance.api_major}."
            f"{instance.api_min_minor}-{instance.api_max_minor}; this client needs "
            f"{CLIENT_API_MAJOR}.{CLIENT_API_MINOR}"
        )
    if not _healthy(descriptor, timeout):
        raise NotFoundError(
            f"DeLOG instance {instance_id!r} is no longer running or did not respond"
        )
    return descriptor


def _descriptors() -> list[Descriptor]:
    root = discovery_root()
    if root is None or not _root_exists(root):
        return []
    found = []
    for name in sorted(os.listdir(root)):
        if name.startswith(".") or not name.endswith(".json"):
            continue
        instance_id = name[: -len(".json")]
        if ID_PATTERN.fullmatch(instance_id) is None:
            continue
        descriptor = _read_descriptor(root / name, instance_id)
        if descriptor is not None:
            found.append(descriptor)
    return found


def _root_exists(root: Path) -> bool:
    try:
        info = os.lstat(root)
    except FileNotFoundError:
        return False
    except OSError:
        raise ForbiddenError(f"discovery directory {root} cannot be inspected") from None
    if not _private_dir(root, info):
        raise ForbiddenError(f"discovery directory {root} failed the current-user permission check")
    return True


def _private_dir(root: Path, info: os.stat_result) -> bool:
    if not stat.S_ISDIR(info.st_mode):
        return False
    if sys.platform == "win32":
        return not _reparse(info) and _windows.is_private(str(root))
    return info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0


def _private_file(path: Path, info: os.stat_result) -> bool:
    if not stat.S_ISREG(info.st_mode):
        return False
    if sys.platform == "win32":
        return not _reparse(info) and _windows.is_private(str(path))
    return info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0


def _reparse(info: os.stat_result) -> bool:
    return bool(getattr(info, "st_file_attributes", 0) & WINDOWS_REPARSE_POINT)


def _read_descriptor(path: Path, instance_id: str) -> Descriptor | None:
    data = _read_private(path)
    if data is None:
        return None
    descriptor = parse_descriptor(data)
    if descriptor is None or descriptor.instance.id != instance_id:
        return None
    return descriptor


def _read_private(path: Path) -> bytes | None:
    try:
        before = os.lstat(path)
    except OSError:
        return None
    if not _private_file(path, before):
        return None
    flags = os.O_RDONLY
    for extra in ("O_NOFOLLOW", "O_CLOEXEC", "O_NONBLOCK", "O_BINARY"):
        flags |= getattr(os, extra, 0)
    try:
        fd = os.open(path, flags)
    except OSError:
        return None
    try:
        after = os.fstat(fd)
        if not stat.S_ISREG(after.st_mode) or (after.st_dev, after.st_ino) != (
            before.st_dev,
            before.st_ino,
        ):
            return None
        if sys.platform != "win32" and not _private_file(path, after):
            return None
        chunks = []
        remaining = MAX_DESCRIPTOR_BYTES + 1
        while remaining > 0:
            chunk = os.read(fd, remaining)
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
    except OSError:
        return None
    finally:
        os.close(fd)
    data = b"".join(chunks)
    if len(data) > MAX_DESCRIPTOR_BYTES:
        return None
    return data


def parse_descriptor(data: bytes) -> Descriptor | None:
    try:
        payload = json.loads(data)
    except ValueError:
        return None
    if not isinstance(payload, dict):
        return None
    keys = set(payload)
    if not REQUIRED_KEYS <= keys or keys - REQUIRED_KEYS - OPTIONAL_KEYS:
        return None
    if _uint(payload["format"], 2**32 - 1) != DESCRIPTOR_FORMAT:
        return None
    instance_id = payload["instance_id"]
    label = payload["label"]
    description = payload.get("session_description")
    pid = _uint(payload["pid"], 2**32 - 1)
    endpoint = _endpoint(payload["endpoint"])
    versions = [
        _uint(payload[key], 2**16 - 1) for key in ("api_major", "api_min_minor", "api_max_minor")
    ]
    token = _token(payload["bootstrap_token"])
    if (
        not isinstance(instance_id, str)
        or ID_PATTERN.fullmatch(instance_id) is None
        or not isinstance(label, str)
        or (description is not None and not isinstance(description, str))
        or pid is None
        or endpoint is None
        or token is None
    ):
        return None
    major, min_minor, max_minor = versions
    if major is None or min_minor is None or max_minor is None:
        return None
    return Descriptor(
        instance=Instance(
            id=instance_id,
            label=label,
            pid=pid,
            endpoint=endpoint,
            api_major=major,
            api_min_minor=min_minor,
            api_max_minor=max_minor,
            session_description=description,
        ),
        bootstrap=token,
    )


def _uint(value: Any, high: int) -> int | None:
    if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= high:
        return None
    return value


def _endpoint(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    match = ENDPOINT_PATTERN.fullmatch(value)
    if match is None:
        return None
    port = int(match.group(1))
    if not 0 < port <= 65535:
        return None
    return f"127.0.0.1:{port}"


def _token(value: Any) -> Secret | None:
    if not isinstance(value, str) or TOKEN_PATTERN.fullmatch(value) is None:
        return None
    try:
        decoded = base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))
    except (binascii.Error, ValueError):
        return None
    if len(decoded) != TOKEN_BYTES:
        return None
    if base64.urlsafe_b64encode(decoded).rstrip(b"=").decode() != value:
        return None
    return Secret(value)


def _pid_alive(pid: int) -> bool:
    if sys.platform == "win32":
        return _windows.pid_alive(pid)
    if not 0 < pid <= 2**31 - 1:
        return True
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return True
    return True


def _healthy(descriptor: Descriptor, timeout: float) -> bool:
    instance = descriptor.instance
    if not _pid_alive(instance.pid):
        return False
    try:
        payload = probe_instance(instance.endpoint, descriptor.bootstrap, timeout)
    except DeLOGError:
        return False
    return (
        isinstance(payload, dict)
        and payload.get("instance_id") == instance.id
        and _uint(payload.get("api_major"), 2**16 - 1) == instance.api_major
    )
