# External Python API

DéLOG can expose the open session to Python programs running outside the
application. An external script discovers a running DéLOG window, reads
immutable snapshots of its data as PyArrow tables, publishes derived topics
back into the session, and drives windows, plots, traces, annotations,
markers, and vehicles. The client is the `delog-client` package in
[`python/delog-client`](../python/delog-client).

This is separate from [embedded scripting](scripting.md) and
[in-app control](app_control.md): external scripts run in their own Python
interpreter, with their own packages, and talk to DéLOG over a local
connection. Both paths end at the same native control boundary, so ownership
and validation rules are identical.

Version 1 scope:

- loopback only: DéLOG listens on `127.0.0.1` and never on a network
  interface;
- snapshot reads only: there is no WebSocket, live subscription, or streaming
  of new telemetry to external scripts;
- no in-process code execution: DéLOG never runs code supplied by an external
  client.

## Installing

The client needs Python 3.11 or newer. Install the wheel attached to a DéLOG
release, or build it from a checkout:

```sh
uv build python/delog-client
uv pip install python/delog-client/dist/delog_client-0.1.0-py3-none-any.whl
```

The wheel depends on `httpx` and `pyarrow` and ships a `py.typed` marker for
type checkers.

The scripts in [`scripts/external`](../scripts/external) run with any Python
that has `delog-client` installed. From a checkout, without installing
anything, run them through the client's project environment:

```sh
uv run --project python/delog-client python scripts/external/catalog.py --pretty
```

## Enabling access in DéLOG

External access is off by default and stays off until you enable it in each
session. Open **Settings**, choose the **External API** tab, and press
**Enable**; the command palette's **External API** command opens the same
tab. The tab shows the instance ID to pass to `DeLOG.connect()`, the access mode,
connected clients, active snapshot leases, and the limits. **Disable** stops the
server, removes its discovery file, and revokes every client; everything
clients created stays visible.

Enabling writes a descriptor file readable only by your user:

- Linux: `$XDG_RUNTIME_DIR/delog/instances`, or
  `$TMPDIR/delog-runtime-<euid>/instances` when `XDG_RUNTIME_DIR` is unset;
- Windows: `%LOCALAPPDATA%\DeLOG\runtime\instances`.

Set `DELOG_DISCOVERY_DIR` in the script's environment to read another
directory. The descriptor holds a bootstrap token; the client exchanges it
for a per-connection token and never prints either.

## Choosing an instance and connecting

<!-- test-example -->
```python
from delog_client import DeLOG

instances = DeLOG.list_instances()
for instance in instances:
    print(instance.id, instance.label, instance.loaded_file)

instance_id = instances[0].id
client = DeLOG.connect(instance_id, name="flight-diagnosis")
print(client.name, client.owner_id)
client.close()
```

`list_instances()` returns every live instance whose API version this client
speaks, sorted by label, and never chooses one for you; scripts should pick by ID when several windows are
running. The bundled [`scripts/external/catalog.py`](../scripts/external/catalog.py)
uses the only instance when exactly one runs and exits with status 2
otherwise.

`name` is the owner name. Everything the connection publishes or creates is
recorded under it. Only one connection may use a name at a time;
`DeLOG.connect(instance_id, name=..., takeover=True)` revokes the previous
connection and cancels its queued work. Reconnecting with the same name after
a disconnect reclaims the same owner, so a rerun can update or remove what an
earlier run left behind.

## Snapshots and the catalog

A snapshot pins one immutable view of the session: its catalog of sources,
topics, and fields, and the data behind it. Loading a new file does not change
an open snapshot. Close it with a `with` block; DéLOG also releases a snapshot
after five minutes without requests.

<!-- test-example -->
```python
from delog_client import DeLOG

[instance] = DeLOG.list_instances()
client = DeLOG.connect(instance.id, name="catalog-inspector")
with client.snapshot() as snapshot:
    for source in snapshot.sources():
        print(source.label, source.kind)
        for topic in source.topics:
            print("  ", topic.name, topic.row_count, topic.time_range_ns)
            for field in topic.fields:
                print("    ", field.name, field.arrow_type, field.unit)
client.close()
```

`snapshot.topic(name, source=...)` looks a topic up by name. A name that
matches nothing raises `NotFoundError`; a name matching several topics raises
`AmbiguousError`, whose `candidates` list the `source/topic` paths to choose
from.

For tools that plan work from the catalog, including a local language model,
[`scripts/external/catalog.py`](../scripts/external/catalog.py) prints the
whole catalog as JSON without reading any data:

```sh
python scripts/external/catalog.py --pretty
python scripts/external/catalog.py --instance INSTANCE_ID
```

## Reading data

<!-- test-example -->
```python
from delog_client import DeLOG

[instance] = DeLOG.list_instances()
client = DeLOG.connect(instance.id, name="flight-diagnosis")
with client.snapshot() as snapshot:
    attitude = snapshot.topic("vehicle_attitude", source="flight")
    table = attitude.read(fields=["roll", "pitch"])
    print(table.column_names)

    rows = 0
    for batch in attitude.iter_batches(fields=["roll"], start_ns=0):
        rows += batch.num_rows
    print(rows)
client.close()
```

`read()` collects the selection into one `pyarrow.Table`. `iter_batches()`
streams record batches with bounded buffering; prefer it for long logs, and
select only the fields you need. `start_ns` and `end_ns` bound the time range.

Every result starts with two signed nanosecond columns:

- `__delog_time_ns`: the timeline shown in DéLOG, after source offsets;
- `__source_time_ns`: the original timestamps of the source.

## Publishing derived topics

A publication is an Arrow table with a `__delog_time_ns` column and one or
more numeric fields. It appears in DéLOG as a derived source owned by the
connection and can be plotted like parsed data.

<!-- test-example -->
```python
import pyarrow as pa
import pyarrow.compute as pc

from delog_client import DeLOG

[instance] = DeLOG.list_instances()
instance_id = instance.id

client = DeLOG.connect(instance_id, name="flight-diagnosis")
with client.snapshot() as snapshot:
    attitude = snapshot.topic("vehicle_attitude", source="flight").read()
error = pc.subtract(attitude.column("roll_setpoint"), attitude.column("roll"))
result_table = pa.table({"__delog_time_ns": attitude.column("__delog_time_ns"), "error": error})
derived = client.publish_topic("attitude_error", result_table, replace=True)
plot = client.windows.open("Flight diagnosis").workspace.add_plot()
plot.traces.add(derived.field("error"))
client.close()
```

`publish_topic()` accepts `units`, `descriptions`, and `multipliers`
mappings keyed by field name:

```python
derived = client.publish_topic(
    "attitude_error",
    result_table,
    units={"error": "rad"},
    descriptions={"error": "roll setpoint minus roll"},
    replace=True,
)
print(derived.name, derived.generation, derived.row_count)
```

Publishing a name the owner already published fails with `ConflictError`
unless `replace=True`; a replacement swaps the whole topic at once, and a
failed upload leaves the previous publication untouched.
`derived.remove()` withdraws one publication. Passing a
`pyarrow.RecordBatchReader` instead of a table streams the upload without
holding it in memory; a reader cannot be replayed, so an interrupted upload
must be restarted with a fresh reader.

## Controlling the workspace

Control calls return handles to the created resources:

<!-- test-example -->
```python
import pyarrow as pa

from delog_client import DeLOG

[instance] = DeLOG.list_instances()
client = DeLOG.connect(instance.id, name="flight-diagnosis")
derived = client.publish_topic(
    "attitude_error",
    pa.table({"__delog_time_ns": [1_000_000, 2_000_000], "error": [0.1, 0.4]}),
    replace=True,
)
window = client.windows.open("Flight diagnosis")
plot = window.workspace.add_plot("horizontal")
trace = plot.traces.add(derived.field("error"), mode="step", color="#ff8800")
plot.annotations.add_text(time_ns=2_000_000, value=0.5, text="divergence")
client.markers.add(2_000_000, "divergence", note="roll error above limit")

state = client.state()
print([w.title for w in state.windows], len(state.traces), len(state.markers))
print([t.field.path for t in state.traces])
client.close()
```

`client.state()` lists windows, plots, traces, annotations, markers,
vehicles, layouts, and playback, each with its owner. A trace reports its
field as `source/topic/field`; for a published topic the source is the owner
name.

Commands inside `client.batch()` are sent together and applied atomically when
the block exits; an exception inside the block discards them:

```python
with client.batch():
    client.playback.set(speed=2.0)
    client.workspace.equalize()
```

## Safe and full control

Every external connection starts in **safe** mode. A safe client may create
resources, and change or remove only the resources its owner created.
Changing or removing anything created by hand in DéLOG, or by another owner,
raises `ForbiddenError` and leaves the state unchanged:

```python
from delog_client import ForbiddenError

manual = [trace for trace in client.state().traces if trace.owner is None]
for trace in manual:
    try:
        trace.remove()
    except ForbiddenError as error:
        print("DeLOG kept the manual trace:", error.message)
```

**Full** control lets clients modify or remove manual state as well. It must
be confirmed in the External API settings tab, applies to every connected client,
shows a warning while active, and resets to safe whenever the server is
disabled and enabled again.

## Idempotency and retries

Every mutation carries an idempotency key. The client generates one per call
and, if the connection drops after the request was sent, asks DéLOG what
happened before deciding whether to send it again, so a retried request is
applied at most once. Pass your own key to make a whole script step safe to
repeat:

```python
from delog_client import UnavailableError

try:
    client.markers.add(5_000_000, "checked", idempotency_key="diagnosis-checked-1")
except UnavailableError as error:
    if error.completion == "unknown":
        status = client.request_status(idempotency_key="diagnosis-checked-1")
        print(status.state)
```

`completion` on an error tells you whether the request was `not_started`,
`committed`, or has an `unknown` outcome. A control that DéLOG has not started
when its connection is revoked, taken over, or the server is disabled is
cancelled and never applied.

## Cleanup

Disconnecting does not remove anything: publications and UI resources stay
visible until the session is closed or their owner removes them. A rerun can
clear its previous results first:

<!-- test-example -->
```python
from delog_client import DeLOG

[instance] = DeLOG.list_instances()
client = DeLOG.connect(instance.id, name="flight-diagnosis")
report = client.remove_owned()
print(report.ui_resources, report.publications, report.complete)
client.close()
```

## A complete diagnosis workflow

[`scripts/external/flight_diagnosis.py`](../scripts/external/flight_diagnosis.py)
streams one field, computes the gap between consecutive samples, publishes
`diagnostic_sample_gaps` with a `gap_ms` field, opens a `Flight diagnosis`
window with the gap trace, and adds a marker and annotation at each gap above
the threshold. When no gap exceeds it, the script still publishes the series
and adds an annotation saying so. It disconnects without cleanup so the
results stay on screen.

```sh
python scripts/external/flight_diagnosis.py \
    --source flight --topic vehicle_attitude --field roll --gap-ms 100
python scripts/external/flight_diagnosis.py \
    --topic vehicle_attitude --field roll --replace
```

## Errors

Every failure raises a subclass of `DeLOGError` with `code`, `message`,
`request_id`, `retryable`, `completion`, `details`, and `status_code`:

```python
from delog_client import DeLOGError, NotFoundError, StaleHandleError

try:
    with client.snapshot() as snapshot:
        snapshot.topic("not_a_topic")
except NotFoundError as error:
    print(error.code, error.message)
except StaleHandleError:
    print("a resource was closed in DeLOG; look it up again")
except DeLOGError as error:
    print(error.code, error.retryable, error.request_id)
```

| Code               | Exception              | Meaning                                         |
| ------------------ | ---------------------- | ----------------------------------------------- |
| `invalid_input`    | `InvalidInputError`    | the request is malformed or out of range        |
| `not_found`        | `NotFoundError`        | no topic, field, or resource has that name      |
| `ambiguous`        | `AmbiguousError`       | several matches; see `candidates`               |
| `stale_handle`     | `StaleHandleError`     | the resource was closed or replaced             |
| `forbidden`        | `ForbiddenError`       | safe mode, or a revoked connection              |
| `snapshot_expired` | `SnapshotExpiredError` | the snapshot was closed, released, or revoked   |
| `conflict`         | `ConflictError`        | reused idempotency key or existing publication  |
| `unavailable`      | `UnavailableError`     | DéLOG is busy, stopping, or did not respond     |
| `internal`         | `InternalError`        | a DéLOG defect; please report it                |

`IncompatibleVersionError` means the instance speaks an API version this
client does not support.

## Limits

The External API settings tab lists the limits and lets you change them for
the next enable; a value that differs from the running server shows the one
in use next to it. Defaults:

| Limit                          | Default   |
| ------------------------------ | --------- |
| Snapshot idle release          | 300 s     |
| Request timeout                | 5 s       |
| Concurrent data downloads      | 4         |
| Upload size                    | 512 MiB   |
| Upload rows                    | 10000000  |
| Upload fields                  | 1024      |
| Concurrent uploads             | 2         |
| Queued controls                | 8         |
| Queued controls per client     | 4         |
| Control timeout                | 5 s       |

Exceeding a limit raises `InvalidInputError` for oversized uploads and
`UnavailableError` when a queue is full; both leave DéLOG unchanged.

## Local language models

A local language model can use this API the same way a person does: read the
JSON catalog from `catalog.py`, then write an ordinary Python script against
`delog_client` and run it. DéLOG does not run model output itself and has no
model integration. Treat generated scripts like any other untrusted program:
run them in an operating-system sandbox or a separate user account, keep
DéLOG in safe mode so they cannot touch manual work, and give each one its own
owner name so `remove_owned()` can undo its results.

## Troubleshooting

- **`list_instances()` is empty**: external access is not enabled in that
  DéLOG window, or the script reads a different discovery directory. Check
  `DELOG_DISCOVERY_DIR` and `XDG_RUNTIME_DIR` in the script's environment.
- **A descriptor is rejected**: the directory or file is readable by other
  users. DéLOG creates them private; do not copy them elsewhere.
- **`ConflictError` on connect**: another connection uses that owner name.
  Close it, or connect with `takeover=True`.
- **`SnapshotExpiredError`**: the snapshot was idle longer than the release
  time or its connection was revoked; open a new snapshot.
- **`UnavailableError` with completion `unknown`**: DéLOG started the request
  but did not answer in time. Check `client.request_status()` before retrying.
- **`ForbiddenError`**: the script tried to change something it does not own
  while DéLOG is in safe mode.
