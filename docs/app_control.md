# DéLOG Python app control

Python can inspect and change the open DéLOG workspace as well as compute
derived data. Two rules matter before using this API:

1. App control is available only at the top level of a named script and in the
   Scripting Console. It is unavailable in live-transform callbacks, custom
   parsers, and dataflow Python nodes. `delog.add_marker(...)` and
   `delog.markers.add(...)` are the live-callback exception because their
   writes are deferred until the callback succeeds.
2. A named script owns the windows, plot panes, traces, annotations, vehicles,
   and markers it creates. A successful rerun commits the new generation and
   removes the script's older generation, including its windows and plot
   panes. A failed run rolls back the new generation, including the windows
   and plot panes it opened, and preserves the last successful one. Removing
   a script's resources by owner follows the same rules. An owned window or
   plot pane that holds any manually added trace or annotation is kept; only
   its owned content is removed. Console-created objects persist until
   explicitly removed.

Calls are synchronous unless a section says they are deferred. Invalid Python
arguments raise `ValueError`; a missing/stale UI object, filesystem failure, or
unavailable control context raises `RuntimeError`.

## Native control boundary

`delog-api` defines the transport-independent control requests and validates
every request, including requests constructed directly by a Rust caller. The
app invokes that validation at its control boundary before changing state.
Native errors retain typed categories such as `InvalidInput`, `StaleHandle`,
`Forbidden`, `Conflict`, and `Unavailable` as they cross the app service;
the Python exceptions above are the embedded adapter's presentation of them.

Resource ownership and the bounded UI control queue are always compiled,
including builds without embedded Python. The app drains control requests in
its UI update loop, and embedded Python is one adapter to that shared native
boundary. Script-owned objects carry an owner name and generation; manual
objects remain unowned.

The [external Python API](external_python_api.md) is a second adapter to the
same boundary. External programs connect over loopback with `delog-client`,
their resources are owned by the connection's name (`external/<name>`), and
safe mode stops them from changing manual objects unless full control is
confirmed in DéLOG. Embedded scripts are not subject to that switch.

## Plots and workspace

```python
plots = delog.plots()              # every plot in every window
main = delog.plots(window=0)       # main-window plots only
plot = delog.focused_plot()        # Plot or None

new_plot = delog.workspace.add_plot(split="horizontal")
other = delog.workspace.split(new_plot, "vertical")
delog.workspace.equalize()
delog.workspace.show_scene(True)
delog.workspace.close(other)

window = delog.windows.open(title="Attitude")
print(window.id)
```

`Plot` exposes `index`, `window`, `label`, `traces`, and `annotations`.
Workspace split directions are `"horizontal"` and `"vertical"`. Window `0` is
the main window; additional windows have stable numeric IDs for the current
session.

The workspace methods are:

| API | Result |
| --- | --- |
| `delog.plots(window=None)` | `list[Plot]` |
| `delog.focused_plot()` | `Plot` or `None` |
| `delog.workspace.add_plot(split="horizontal")` | new `Plot` |
| `delog.workspace.split(plot, direction)` | new `Plot` |
| `delog.workspace.close(plot)` | `None` |
| `delog.workspace.equalize()` | `None` |
| `delog.workspace.show_scene(visible)` | `None` |
| `delog.windows.open(title=None)` | new `Window` |

## Traces

Fields may be a `FieldRef` or a string such as `"IMU.AccX"`. String lookup
must resolve to exactly one live source; use `delog.find(...)` first when the
name is ambiguous.

```python
plot = delog.focused_plot()
plot.traces.add("IMU.AccX", color="#E74C3C", width_px=2.0, mode="line")
plot.traces.extend(["IMU.AccY", "IMU.AccZ"])

for trace in plot.traces:
    print(trace.index, trace.field, trace.color, trace.mode, trace.visible)

trace = plot.traces[0]
trace.color = "#2ECC71"
trace.mode = "scatter"             # line, scatter, or step
trace.visible = False

plot.traces.remove(0)               # by index
plot.traces.remove(field="IMU.AccZ")
plot.traces.clear()
```

Trace handles verify both their index and field identity when edited. If the
trace was removed or the index now addresses another field, the edit fails
instead of retargeting the replacement.

## Annotations

Points are `(time_us, y)` pairs. A plot-local collection supports typed
creation, listing, editing, removal, and bulk creation:

```python
anns = plot.annotations
label = anns.add_text((1_000_000, 12.5), "armed", color="#FFFFFFFF")
limit = anns.add_hline(20.0, "limit", color="#E74C3C")
anns.add_segment((1_000_000, 10.0), (2_000_000, 15.0), "rise")
anns.add_rect((2_000_000, 5.0), (3_000_000, 12.0), "window")
anns.add_ellipse((3_000_000, 5.0), (4_000_000, 12.0), "region")

label.move_to((1_500_000, 13.0))
label.label = "motor armed"
label.color = "#2ECC71FF"
limit.y = 25.0
```

All creation calls accept optional `color`, `stroke_px`, `fill_opacity`,
`font_px`, and `arrow` style keywords. `add(kind, ...)` and `extend([...])`
provide dynamic and mapping-based forms. Handles expose `id`, `index`, `kind`,
`label`, `color`, and `owner`.

Plot-local removal can target an index/handle or exactly one of `kind=`,
`label=`, and `owner=`. The global collection searches every window:

```python
plot.annotations.remove(label)
plot.annotations.clear()

all_annotations = delog.annotations.list()
delog.annotations.remove(owner="analysis.py")
delog.annotations.clear()
```

## Playback

Playback exposes write-only properties:

```python
delog.playback.speed = 2.0
delog.playback.follow_live = True
```

Speed must be finite. These setters change transport behavior but do not alter
the recorded timestamps.

## Vehicles and profiles

Build position and orientation mappings first, then add the vehicle:

```python
pos = delog.gps(
    "GPS.lat", "GPS.lon", "GPS.alt",
    dege7=True, alt_mm=True, alt_offset_m=0.0,
)
ori = delog.euler("ATT.roll", "ATT.pitch", "ATT.yaw", degrees=True)

vehicle = delog.vehicles.add(
    source="flight",
    pos=pos,
    ori=ori,
    label="UAV",
    model="quad",
    color="#5AAAFFFF",
    path_color="#FFAA3CFF",
    scale=1.0,
    show=True,
    show_path=True,
)
```

Other mapping constructors are:

```python
delog.ned(north, east, down, reference=None)
delog.geo(lat_deg, lon_deg, alt_m)
delog.geo_fields(lat, lon, alt)
delog.quat(w, x, y, z)
delog.static_ori()
```

`Vehicle` handles expose stable `id`, `index`, `source`, `owner`, `label`,
`show`, `show_path`, `pos`, `ori`, `model`, `color`, `path_color`, and `scale`.
Every property after `owner` is editable where meaningful.

```python
vehicle.show_path = False
vehicle.scale = 0.5
delog.vehicles.remove(vehicle)
delog.vehicles.remove(label="UAV")
delog.vehicles.remove(source="flight")
delog.vehicles.clear()
```

Vehicle profiles form a reusable filesystem library:

```python
delog.vehicle_profiles.save("inspection", vehicle)
print(delog.vehicle_profiles.list())
profile = delog.vehicle_profiles.load("inspection")
restored = delog.vehicle_profiles.apply("inspection", source="flight")
delog.vehicle_profiles.delete("inspection")
```

Loaded profiles expose their label, visibility, mapping fields, model, colors,
and scale without creating a vehicle. Profile operations are immediate and are
not allowed inside a batch.

## Markers

`delog.add_marker(...)` remains the compact deferred API. `delog.markers`
adds bulk creation, inspection, stable handles, filtering, and editing:

```python
delog.add_marker(1_000_000, "armed")
delog.markers.add(2_000_000, "takeoff", color="#2ECC71", note="detected")
delog.markers.extend([(3_000_000, "cruise"), (4_000_000, "landing")])

for marker in delog.markers:
    print(marker.id, marker.t_us, marker.label, marker.origin, marker.owner)

marker = delog.markers[0]
marker.t_us = 1_100_000
marker.label = "armed confirmed"
marker.color = "#FFFFFFFF"
marker.note = "reviewed"
```

Adds are deferred until the enclosing named run, console evaluation, or live
callback succeeds. Reads and edits are immediate and therefore need the normal
top-level control context.

Removal accepts one axis at a time:

```python
delog.markers.remove(marker)                    # stable handle
delog.markers.remove(0)                         # sorted display index
delog.markers.remove(owner="analysis.py")
delog.markers.remove(origin="script")           # or "manual"
delog.markers.remove(label="landing")           # script markers only
delog.markers.remove(after=1_000_000, before=2_000_000)
delog.markers.clear()                            # script markers only
delog.markers.clear(manual=True)                 # all markers
```

Manual-marker protection is deliberate: label/time filters and the default
`clear()` affect only script markers. Use an explicit handle/index,
`origin="manual"`, or `manual=True` when manual markers should be removed.

## Layouts

The layout library uses names containing only ASCII letters, digits, `-`, and
`_`:

```python
delog.layouts.save("analysis")
print(delog.layouts.list())
report = delog.layouts.load("analysis")
delog.layouts.duplicate("analysis", "analysis-copy")
delog.layouts.rename("analysis-copy", "cruise")
delog.layouts.export_file("analysis", "/tmp/analysis.json")
report = delog.layouts.import_file("/tmp/analysis.json")
delog.layouts.delete("cruise")
```

`import_file` applies a document but does not add it to the named library.
`export_file` writes a named library document. `delog.layouts.clear()` clears
the current workspace, windows, playback state, markers, and vehicles; it does
not delete saved layouts.

`delog.layouts.current()` returns an ordinary JSON-compatible dictionary.
`delog.layouts.apply()` accepts such a dictionary and rejects recursive values,
NaN, and infinity before contacting the app:

```python
doc = delog.layouts.current()
doc["playback"]["speed"] = 0.5
report = delog.layouts.apply(doc)
```

`load`, `import_file`, and `apply` resolve every field they can and return a
`LoadReport`:

- `report.ambiguous`: dictionaries with `field` and sorted `candidates`;
- `report.unresolved`: missing `"topic.field"` names;
- `report.warnings`: non-field issues, such as a malformed annotation skipped
  during restore.

Ambiguous and unresolved traces remain as ghosts where possible. Vehicles that
cannot resolve all required fields are skipped. The report lists each issue
once and no interactive source-mapping dialog is opened for Python loads.

## Atomic batches

Use `with delog.batch():` when several mutations must become visible together:

```python
with delog.batch():
    delog.playback.speed = 2.0
    plot.traces.add("IMU.AccX", color="#E74C3C")
    delog.markers.add(1_000_000, "analysis start")
```

The block validates and stages requests on the Python worker. It does not call
the app until the block exits successfully. A Python exception discards the
block. At the app, all requests apply to shadow state and commit together; if
one request refers to a stale object, none of the block's changes commit. This
is an all-or-nothing transaction. Nested batches are rejected.

Batch-compatible operations are those whose normal result is `None`:

- trace add/extend/remove/clear and handle property edits;
- annotation remove/clear and handle edits (annotation creation returns a
  handle and is therefore excluded);
- workspace close/equalize/show-scene and playback setters;
- vehicle handle edits and remove/clear;
- marker add/extend/remove/clear and handle edits.

Reads, list/length/index operations, handle-returning creates, window opening,
workspace add/split, layouts, vehicle profiles, and nested batches raise
`ValueError` before anything is staged.

## Ownership, timing, and stale handles

A successful named run publishes its explicit batch blocks, deferred markers,
and generation commit as one transaction. The commit removes only older
objects owned by that same script. If execution or layout/output preparation
fails, the new generation is rolled back and the previous one remains.
Asynchronous bridge failures are shown in the app log with the
`python-control` target.

Immediate calls outside `delog.batch()` can be visible while a long named
script continues to run. If the run later fails, its generation rollback
removes those owned objects. Use a batch when intermediate visibility is not
desirable.

Marker, annotation, and vehicle IDs are monotonic. Handles never silently
retarget a newly created object after deletion or layout clear. A stale handle
raises a readable error instead.

## Context restrictions

The app-control bridge intentionally does not exist in:

- `@delog.live_transform` callback bodies (deferred marker adds are allowed);
- custom `Parse(raw_data)` file parsers;
- dataflow Python Script nodes;
- flow scripts.

Those contexts run away from the synchronous UI request loop or under a
narrower data-only contract. Move control calls to the named script's top level
or the Scripting Console.
