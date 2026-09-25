# delog-client

Synchronous Python client for a running DeLOG desktop session. It discovers
DeLOG instances that have external access enabled, connects under an owner
name, reads immutable data snapshots as PyArrow tables or record batches,
publishes derived topics back into the session, and drives plots, traces,
annotations, markers, and vehicles.

Requires Python 3.11 or newer, `httpx`, and `pyarrow`. The package ships a
`py.typed` marker, so type checkers use its inline annotations.

## Installing

Install the wheel attached to a DeLOG release, or build one from this
directory:

```sh
uv build python/delog-client
uv pip install python/delog-client/dist/delog_client-0.1.0-py3-none-any.whl
```

The complete workflow guide lives in
[`docs/external_python_api.md`](../../docs/external_python_api.md).

## Discovering and connecting

```python
from delog_client import DeLOG

for instance in DeLOG.list_instances():
    print(instance.id, instance.label, instance.loaded_file)

client = DeLOG.connect(instance.id, name="flight-diagnosis")
```

`list_instances()` never chooses an instance for you. It reads descriptor
files, skips entries whose process has exited, whose endpoint does not answer,
or whose reported instance ID or API major version differs from the
descriptor, and returns the rest sorted by label and then instance ID.

`connect()` exchanges the descriptor's bootstrap token for a client token and
keeps only the client token. Pass `takeover=True` to replace an existing
connection that uses the same name. Tokens are sent only in `Authorization`
headers and never appear in `repr()`, exception messages, or logs.

Descriptors are read from:

- Linux and other Unix systems: `$XDG_RUNTIME_DIR/delog/instances`, or
  `$TMPDIR/delog-runtime-<euid>/instances` (with `/tmp` when `TMPDIR` is unset)
  if `XDG_RUNTIME_DIR` is not an absolute path;
- Windows: `%LOCALAPPDATA%\DeLOG\runtime\instances`.

Set `DELOG_DISCOVERY_DIR` to read another directory instead. The directory and
each descriptor must be private to the current user; anything else is
rejected.

## Reading snapshots

```python
with client.snapshot() as snapshot:
    for source in snapshot.sources():
        for topic in source.topics:
            print(source.label, topic.name, topic.row_count)

    attitude = snapshot.topic("vehicle_attitude", source="flight")
    table = attitude.read(fields=["roll", "pitch"])

    for batch in attitude.iter_batches(start_ns=0, end_ns=5_000_000_000):
        print(batch.num_rows)

client.close()
```

A snapshot pins one immutable catalog, fetched once when the snapshot is
created. Lookups never guess: a name that matches nothing raises
`NotFoundError`, and a name that matches several topics raises `AmbiguousError`
whose `candidates` list holds sorted `source/topic` paths. Multi-instance topics
keep their bracketed names, such as `sensor_accel[1]`; look them up by that
name, or by the base name together with `instance=1`. A name that exactly
matches a topic's full name, such as a bare `HEARTBEAT` next to `HEARTBEAT[1]`,
selects that topic; the base name is only used when nothing matches exactly or
`instance` is given. This precedence applies within one source only: without
`source=`, matches from different sources are always reported as ambiguous.

Every data stream starts with two signed nanosecond columns:
`__delog_time_ns`, the timeline shown in DeLOG after source offsets, and
`__source_time_ns`, the original source timeline. Omitting `fields` returns
every field of the topic; an explicit list returns only those fields, in the
given order.

`iter_batches()` streams record batches with bounded buffering and releases the
HTTP response as soon as the iterator finishes or is closed. `read()` collects
the whole selection into one table.

## Publishing and controlling DeLOG

```python
import pyarrow as pa
import pyarrow.compute as pc

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
```

Published topics and created UI resources belong to the connection's owner
name and stay visible after the script disconnects. Reconnecting with the same
name and calling `client.remove_owned()` removes them again. In DeLOG's default
safe mode a script may only change what it created; removing manual plots,
traces, markers, or vehicles raises `ForbiddenError` until full control is
confirmed in DeLOG.

## Errors

Every failure raises a subclass of `DeLOGError` carrying `code`, `message`,
`request_id`, `retryable`, `completion`, `details`, and `status_code`:

| Wire code          | Exception              |
| ------------------ | ---------------------- |
| `invalid_input`    | `InvalidInputError`    |
| `not_found`        | `NotFoundError`        |
| `ambiguous`        | `AmbiguousError`       |
| `stale_handle`     | `StaleHandleError`     |
| `forbidden`        | `ForbiddenError`       |
| `snapshot_expired` | `SnapshotExpiredError` |
| `conflict`         | `ConflictError`        |
| `unavailable`      | `UnavailableError`     |
| `internal`         | `InternalError`        |

`IncompatibleVersionError` is raised when connecting to an instance whose API
version this client does not speak.

## Development

```sh
uv run --project python/delog-client --extra test pytest python/delog-client/tests -q
uv run --project python/delog-client --extra test --with mypy --with pyarrow-stubs \
    mypy --config-file python/delog-client/pyproject.toml \
    python/delog-client/src python/delog-client/tests
```
