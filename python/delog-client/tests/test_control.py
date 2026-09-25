from __future__ import annotations

import json
from collections.abc import Callable
from typing import Any

import httpx
import pyarrow as pa
import pytest

from delog_client import (
    Annotation,
    Client,
    ControlBatch,
    ControlState,
    ForbiddenError,
    InvalidInputError,
    LayoutLoadReport,
    Marker,
    Plot,
    RemovalReport,
    StaleHandleError,
    Trace,
    UnavailableError,
    Vehicle,
    VehicleProfile,
    Window,
)

from conftest import FakeControl, FakeInstance, make_id


def control_requests(server: FakeInstance) -> list[httpx.Request]:
    return [r for r in server.requests if r.url.path.startswith("/v1/control")]


def last_command(server: FakeInstance) -> dict[str, Any]:
    body: dict[str, Any] = json.loads(control_requests(server)[-1].content)
    return body


def assert_no_owner(value: Any) -> None:
    if isinstance(value, dict):
        assert "owner" not in value
        for item in value.values():
            assert_no_owner(item)
    elif isinstance(value, list):
        for item in value:
            assert_no_owner(item)


def test_windows_open_returns_a_typed_window(client: Client, server: FakeInstance) -> None:
    window = client.windows.open("Flight diagnosis")
    assert isinstance(window, Window)
    assert window.title == "Flight diagnosis"
    assert last_command(server) == {"op": "window_open", "title": "Flight diagnosis"}
    untitled = client.windows.open()
    assert untitled.title is None
    assert last_command(server) == {"op": "window_open"}
    assert window.handle != untitled.handle
    assert not hasattr(window, "id")


def test_workspace_plots_split_and_close(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    window = client.windows.open("w")
    plot = window.workspace.add_plot(direction="vertical")
    assert isinstance(plot, Plot)
    assert plot.window is window
    assert last_command(server) == {
        "op": "workspace_add_plot",
        "window": window.handle,
        "direction": "vertical",
    }
    main_plot = client.workspace.add_plot(direction="horizontal")
    assert last_command(server) == {"op": "workspace_add_plot", "direction": "horizontal"}
    assert main_plot.window is not None
    assert main_plot.window.handle == control.main_window
    assert main_plot.window.handle != window.handle
    below = plot.split(direction="horizontal")
    assert isinstance(below, Plot)
    assert below.window is window
    assert last_command(server) == {
        "op": "workspace_split",
        "plot": plot.handle,
        "direction": "horizontal",
    }
    below.close()
    assert last_command(server) == {"op": "workspace_close", "plot": below.handle}
    assert below.stale
    before = len(server.requests)
    with pytest.raises(StaleHandleError):
        below.split(direction="vertical")
    assert len(server.requests) == before
    window.workspace.equalize()
    assert last_command(server) == {"op": "workspace_equalize", "window": window.handle}
    window.workspace.set_scene_visible(False)
    assert last_command(server) == {"op": "scene_set_visible", "visible": False}
    client.workspace.equalize()
    assert last_command(server) == {"op": "workspace_equalize"}


def test_traces_add_set_remove_and_clear(
    client: Client, server: FakeInstance, arrow_table: pa.Table
) -> None:
    publication = client.publish_topic("attitude_error", arrow_table)
    plot = client.windows.open("w").workspace.add_plot(direction="vertical")
    error = publication.field("error")
    trace = plot.traces.add(error, color="#ff8800", width_px=2.0, mode="line")
    assert isinstance(trace, Trace)
    assert trace.plot is plot
    assert trace.field is error
    assert last_command(server) == {
        "op": "trace_add",
        "plot": plot.handle,
        "field": error.handle,
        "color": "#ff8800",
        "width_px": 2.0,
        "mode": "line",
    }
    plain = plot.traces.add(publication.field("limit"))
    assert last_command(server) == {
        "op": "trace_add",
        "plot": plot.handle,
        "field": publication.field("limit").handle,
        "mode": "line",
    }
    trace.set(visible=False, mode="step")
    assert last_command(server) == {
        "op": "trace_set",
        "trace": trace.handle,
        "mode": "step",
        "visible": False,
    }
    trace.remove()
    assert last_command(server) == {"op": "trace_remove", "trace": trace.handle}
    assert trace.stale
    assert plain.stale
    before = len(server.requests)
    with pytest.raises(StaleHandleError):
        plain.set(color="#00ff00")
    assert len(server.requests) == before
    again = plot.traces.add(error)
    plot.traces.clear()
    assert last_command(server) == {"op": "trace_clear", "plot": plot.handle}
    assert again.stale


def test_traces_accept_snapshot_fields_and_reject_closed_snapshots(
    client: Client, server: FakeInstance
) -> None:
    plot = client.windows.open("w").workspace.add_plot(direction="vertical")
    with client.snapshot() as snapshot:
        roll = snapshot.topic("vehicle_attitude", source="flight").field("roll")
        trace = plot.traces.add(roll)
        assert last_command(server)["field"] == roll.handle
        assert trace.field.name == "roll"
    before = len(server.requests)
    with pytest.raises(StaleHandleError):
        plot.traces.add(roll)
    with pytest.raises(InvalidInputError):
        plot.traces.add("roll")  # type: ignore[arg-type]
    assert len(server.requests) == before


def test_server_stale_handles_raise_stale_handle_error(
    client: Client, control: FakeControl
) -> None:
    plot = client.windows.open("w").workspace.add_plot(direction="vertical")
    control.stale.add(plot.handle)
    with pytest.raises(StaleHandleError):
        plot.traces.clear()


def test_annotations_cover_every_geometry(client: Client, server: FakeInstance) -> None:
    plot = client.windows.open("w").workspace.add_plot(direction="vertical")
    text = plot.annotations.add_text(time_ns=5_000, value=1.5, text="divergence", color="#ff0000")
    assert isinstance(text, Annotation)
    assert text.plot is plot
    assert last_command(server) == {
        "op": "annotation_add",
        "plot": plot.handle,
        "geometry": {"kind": "text", "at": {"time_ns": 5_000, "y": 1.5}},
        "label": "divergence",
        "style": {"color": "#ff0000"},
    }
    plot.annotations.add_segment((1_000, 0.0), (2_000, 1.0), label="ramp", arrow=True)
    assert last_command(server)["geometry"] == {
        "kind": "segment",
        "from": {"time_ns": 1_000, "y": 0.0},
        "to": {"time_ns": 2_000, "y": 1.0},
    }
    assert last_command(server)["style"] == {"arrow": True}
    plot.annotations.add_rect((1_000, 0.0), (2_000, 1.0), fill_opacity=0.25)
    assert last_command(server)["geometry"]["kind"] == "rect"
    assert last_command(server)["label"] == ""
    plot.annotations.add_ellipse((1_000, 0.0), (2_000, 1.0), label="cluster")
    assert last_command(server)["geometry"] == {
        "kind": "ellipse",
        "a": {"time_ns": 1_000, "y": 0.0},
        "b": {"time_ns": 2_000, "y": 1.0},
    }
    line = plot.annotations.add_hline(0.3, label="limit", stroke_px=2.0)
    assert last_command(server)["geometry"] == {"kind": "h_line", "y": 0.3}
    assert last_command(server)["style"] == {"stroke_px": 2.0}
    line.set(label="new limit", geometry=(0.4,), color="#00ff00")
    assert last_command(server) == {
        "op": "annotation_set",
        "annotation": line.handle,
        "label": "new limit",
        "geometry": {"kind": "h_line", "y": 0.4},
        "style": {"color": "#00ff00"},
    }
    text.set(geometry=[(6_000, 2.0)])
    assert last_command(server)["geometry"] == {
        "kind": "text",
        "at": {"time_ns": 6_000, "y": 2.0},
    }
    text.remove()
    assert last_command(server) == {"op": "annotation_remove", "annotation": text.handle}
    assert text.stale
    with pytest.raises(InvalidInputError):
        plot.annotations.add_text(time_ns=1.5, value=1.0, text="x")  # type: ignore[arg-type]


def test_markers_add_set_remove(client: Client, server: FakeInstance) -> None:
    marker = client.markers.add(9_000, label="Probable failure", color="#ff0000", note="check")
    assert isinstance(marker, Marker)
    assert marker.time_ns == 9_000
    assert marker.label == "Probable failure"
    assert last_command(server) == {
        "op": "marker_add",
        "time_ns": 9_000,
        "label": "Probable failure",
        "color": "#ff0000",
        "note": "check",
    }
    client.markers.add(1_000)
    assert last_command(server) == {"op": "marker_add", "time_ns": 1_000, "label": ""}
    marker.set(label="Confirmed failure")
    assert last_command(server) == {
        "op": "marker_set",
        "marker": marker.handle,
        "label": "Confirmed failure",
    }
    marker.remove()
    assert last_command(server) == {"op": "marker_remove", "marker": marker.handle}
    assert marker.stale


def test_vehicles_with_gps_and_quaternion_infer_the_publication_source(
    client: Client, server: FakeInstance
) -> None:
    table = pa.table(
        {
            "__delog_time_ns": pa.array([0, 1000], pa.int64()),
            **{
                name: pa.array([0.0, 1.0]) for name in ("lat", "lon", "alt", "qw", "qx", "qy", "qz")
            },
        }
    )
    estimate = client.publish_topic("estimate", table)
    field = estimate.field
    vehicle = client.vehicles.add(
        label="Estimated vehicle",
        position={"lat": field("lat"), "lon": field("lon"), "alt": field("alt")},
        orientation={"quaternion": [field("qw"), field("qx"), field("qy"), field("qz")]},
        model="quad",
    )
    assert isinstance(vehicle, Vehicle)
    assert vehicle.label == "Estimated vehicle"
    assert last_command(server) == {
        "op": "vehicle_add",
        "source": estimate.handle,
        "label": "Estimated vehicle",
        "show": True,
        "show_path": True,
        "position": {
            "kind": "gps",
            "lat": field("lat").handle,
            "lon": field("lon").handle,
            "alt": field("alt").handle,
            "lat_lon_dege7": False,
            "alt_mm": False,
            "alt_offset_m": 0.0,
        },
        "orientation": {
            "kind": "quat",
            "w": field("qw").handle,
            "x": field("qx").handle,
            "y": field("qy").handle,
            "z": field("qz").handle,
        },
        "model": "quad",
        "color": "#5AAAFFFF",
        "path_color": "#FFAA3CFF",
        "scale": 1.0,
    }
    previous = vehicle.handle
    vehicle.set(label="Renamed", show_path=False, orientation=None)
    assert last_command(server) == {
        "op": "vehicle_set",
        "vehicle": previous,
        "patch": {"label": "Renamed", "show_path": False, "orientation": {"kind": "static"}},
    }
    assert vehicle.label == "Renamed"
    vehicle.save_profile("estimate")
    assert last_command(server) == {
        "op": "vehicle_profile_save",
        "name": "estimate",
        "vehicle": vehicle.handle,
    }
    vehicle.remove()
    assert last_command(server) == {"op": "vehicle_remove", "vehicle": vehicle.handle}
    assert vehicle.stale


def test_vehicles_with_ned_and_euler_from_a_snapshot(client: Client, server: FakeInstance) -> None:
    with client.snapshot() as snapshot:
        topic = snapshot.topic("vehicle_attitude", source="flight")
        roll, pitch, yaw = (topic.field(n) for n in ("roll", "pitch", "yaw"))
        client.vehicles.add(
            position={
                "north": roll,
                "east": pitch,
                "down": yaw,
                "reference": {"lat_deg": 47.0, "lon_deg": 8.0, "alt_m": 400.0},
            },
            orientation={"euler": [roll, pitch, yaw], "degrees": True},
        )
        command = last_command(server)
        assert command["source"] == topic.source.handle
        assert command["label"] == "Vehicle"
        assert command["position"] == {
            "kind": "ned",
            "north": roll.handle,
            "east": pitch.handle,
            "down": yaw.handle,
            "reference": {"kind": "manual", "lat_deg": 47.0, "lon_deg": 8.0, "alt_m": 400.0},
        }
        assert command["orientation"] == {
            "kind": "euler",
            "roll": roll.handle,
            "pitch": pitch.handle,
            "yaw": yaw.handle,
            "degrees": True,
        }
        with pytest.raises(InvalidInputError):
            client.vehicles.add(position={"north": roll})
        with pytest.raises(InvalidInputError):
            client.vehicles.add(
                position={"lat": roll, "lon": pitch, "alt": yaw}, orientation={"spin": 1}
            )


def test_vehicle_profiles(client: Client, server: FakeInstance, arrow_table: pa.Table) -> None:
    assert client.vehicle_profiles.list() == ["default", "quad"]
    listed = control_requests(server)[-1]
    assert "idempotency-key" not in listed.headers
    profile = client.vehicle_profiles.load("quad")
    assert isinstance(profile, VehicleProfile)
    assert profile.name == "quad"
    assert profile.scale == 1.5
    assert "idempotency-key" not in control_requests(server)[-1].headers
    publication = client.publish_topic("estimate", arrow_table)
    vehicle = client.vehicle_profiles.apply("quad", publication)
    assert isinstance(vehicle, Vehicle)
    assert last_command(server) == {
        "op": "vehicle_profile_apply",
        "name": "quad",
        "source": publication.handle,
    }
    client.vehicle_profiles.save("mine", vehicle)
    client.vehicle_profiles.delete("mine")
    assert last_command(server) == {"op": "vehicle_profile_delete", "name": "mine"}


def test_layouts(client: Client, control: FakeControl, server: FakeInstance) -> None:
    assert client.layouts.list() == ["default", "quad"]
    assert "idempotency-key" not in control_requests(server)[-1].headers
    assert client.layouts.current() == '{"plots":[]}'
    assert "idempotency-key" not in control_requests(server)[-1].headers
    client.layouts.save("diagnosis")
    assert last_command(server) == {"op": "layout_save", "name": "diagnosis"}
    assert control_requests(server)[-1].headers["idempotency-key"]
    client.layouts.rename("diagnosis", "diag")
    assert last_command(server) == {"op": "layout_rename", "from": "diagnosis", "to": "diag"}
    client.layouts.duplicate("diag", "diag2")
    assert last_command(server) == {"op": "layout_duplicate", "from": "diag", "to": "diag2"}
    client.layouts.export_file("diag", "/tmp/diag.json")
    assert last_command(server) == {"op": "layout_export", "name": "diag", "path": "/tmp/diag.json"}
    client.layouts.delete("diag2")
    assert last_command(server) == {"op": "layout_delete", "name": "diag2"}

    plot = client.windows.open("w").workspace.add_plot(direction="vertical")
    control.results["layout_load"] = {
        "kind": "load_report",
        "ambiguous": [{"field": "roll", "candidates": ["a/roll", "b/roll"]}],
        "unresolved": ["gone"],
        "warnings": ["w"],
    }
    report = client.layouts.load("diag")
    assert isinstance(report, LayoutLoadReport)
    assert report.ambiguous[0].field == "roll"
    assert report.ambiguous[0].candidates == ("a/roll", "b/roll")
    assert report.unresolved == ("gone",)
    assert report.warnings == ("w",)
    assert plot.stale
    assert client.layouts.clear() is None
    assert last_command(server) == {"op": "layout_clear"}
    client.layouts.import_file("/tmp/diag.json")
    assert last_command(server) == {"op": "layout_import", "path": "/tmp/diag.json"}
    client.layouts.apply('{"plots":[]}')
    assert last_command(server) == {"op": "layout_apply", "json": '{"plots":[]}'}


def test_playback_set_and_state(client: Client, control: FakeControl, server: FakeInstance) -> None:
    client.playback.set(speed=2.0, follow_live=True)
    assert last_command(server) == {"op": "playback_set", "speed": 2.0, "follow_live": True}
    client.playback.set(speed=0.5)
    assert last_command(server) == {"op": "playback_set", "speed": 0.5}
    control.state["playback"] = {"speed": 3.0, "follow_live": True}
    state = client.playback.state()
    assert (state.speed, state.follow_live) == (3.0, True)


def test_state_returns_typed_handles(client: Client, control: FakeControl) -> None:
    window, plot, trace, annotation, marker, vehicle = (make_id() for _ in range(6))
    control.state.update(
        {
            "windows": [{"handle": window, "title": "Main", "owner": None}],
            "plots": [{"handle": plot, "window": window, "label": "p", "owner": "external/diag"}],
            "traces": [
                {
                    "handle": trace,
                    "plot": plot,
                    "field": "flight/vehicle_attitude/roll",
                    "color": "#ff8800ff",
                    "width_px": 1.5,
                    "mode": "line",
                    "visible": True,
                    "owner": "external/diag",
                }
            ],
            "annotations": [
                {
                    "handle": annotation,
                    "plot": plot,
                    "geometry": {"kind": "h_line", "y": 1.0},
                    "label": "limit",
                    "color": "#ffffffff",
                    "owner": None,
                }
            ],
            "markers": [
                {
                    "handle": marker,
                    "time_ns": 5000,
                    "label": "m",
                    "color": "#ffffffff",
                    "note": "",
                    "origin": "script",
                    "owner": "external/diag",
                }
            ],
            "vehicles": [
                {
                    "handle": vehicle,
                    "source": "flight",
                    "label": "v",
                    "show": True,
                    "show_path": True,
                    "position": {"kind": "gps"},
                    "orientation": {"kind": "static"},
                    "model": "quad",
                    "color": "#5AAAFFFF",
                    "path_color": "#FFAA3CFF",
                    "scale": 1.0,
                    "owner": None,
                }
            ],
            "future_field": 1,
        }
    )
    state = client.state()
    assert isinstance(state, ControlState)
    assert state.windows[0].title == "Main"
    assert state.plots[0].window is state.windows[0]
    assert state.plots[0].owner == "external/diag"
    assert state.traces[0].plot is state.plots[0]
    assert state.traces[0].field.name == "roll"
    assert state.traces[0].mode == "line"
    assert state.annotations[0].label == "limit"
    assert state.markers[0].time_ns == 5000
    assert state.vehicles[0].label == "v"
    assert state.layout_names == ("default",)
    assert state.scene_visible is True
    assert [w.handle for w in client.windows.list()] == [window]
    assert [p.handle for p in state.windows[0].workspace.plots()] == [plot]
    assert [t.handle for t in state.plots[0].traces.list()] == [trace]
    assert [a.handle for a in state.plots[0].annotations.list()] == [annotation]
    assert [m.handle for m in client.markers.list()] == [marker]
    assert [v.handle for v in client.vehicles.list()] == [vehicle]
    state.markers[0].set(label="relabel")


def test_safe_mode_forbids_mutations_but_allows_queries(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.access = "safe"
    with pytest.raises(ForbiddenError) as caught:
        client.windows.open("w")
    assert caught.value.completion == "not_started"
    assert "safe mode" in caught.value.message
    assert client.layouts.list() == ["default", "quad"]
    assert client.state().scene_visible is True


def test_no_request_carries_an_owner(
    client: Client, server: FakeInstance, arrow_table: pa.Table
) -> None:
    publication = client.publish_topic("attitude_error", arrow_table)
    plot = client.windows.open("w").workspace.add_plot(direction="vertical")
    plot.traces.add(publication.field("error"))
    client.markers.add(1000, label="m")
    client.remove_owned()
    for request in server.requests:
        assert "owner" not in request.url.params
        if request.url.path.startswith("/v1/control"):
            assert_no_owner(json.loads(request.content))


def test_remove_owned_reports_counts_and_stales_handles(
    client: Client, server: FakeInstance, arrow_table: pa.Table
) -> None:
    publication = client.publish_topic("attitude_error", arrow_table)
    window = client.windows.open("w")
    marker = client.markers.add(1000, label="m")
    report = client.remove_owned()
    assert isinstance(report, RemovalReport)
    assert (report.ui_resources, report.publications) == (4, 2)
    assert report.complete
    assert report.failed == ()
    assert last_command(server) == {"op": "remove_owned"}
    assert control_requests(server)[-1].headers["idempotency-key"]
    assert window.stale and marker.stale and publication.stale


def test_remove_owned_partial_safe_mode_failure_is_terminal(
    client: Client, control: FakeControl, server: FakeInstance, arrow_table: pa.Table
) -> None:
    publication = client.publish_topic("attitude_error", arrow_table)
    window = client.windows.open("w")
    control.errors["remove_owned"] = httpx.Response(
        403,
        json={
            "request_id": "req-partial",
            "code": "forbidden",
            "message": "DeLOG is in safe mode",
            "retryable": False,
            "completion": "committed",
            "details": {"ui_resources": None, "publications": 1, "failed": ["ui"]},
        },
    )
    before = len(control_requests(server))
    with pytest.raises(ForbiddenError) as caught:
        client.remove_owned()
    assert len(control_requests(server)) == before + 1
    error = caught.value
    assert error.completion == "committed"
    report = error.report
    assert isinstance(report, RemovalReport)
    assert report.failed == ("ui",)
    assert report.ui_resources is None
    assert report.publications == 1
    assert not report.complete
    assert report.requires_user_action
    assert publication.stale
    assert not window.stale


def test_remove_owned_unknown_outcome_is_not_retried(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    control.errors["remove_owned"] = httpx.Response(
        503,
        json={
            "request_id": "req-unknown",
            "code": "unavailable",
            "message": "the UI did not answer",
            "retryable": True,
            "completion": "unknown",
            "details": {"ui_resources": None, "publications": 0, "failed": ["ui"]},
        },
    )
    before = len(control_requests(server))
    with pytest.raises(UnavailableError) as caught:
        client.remove_owned()
    assert caught.value.completion == "unknown"
    assert caught.value.report is not None
    assert caught.value.report.requires_user_action is False
    assert len(control_requests(server)) == before + 1


def test_batch_sends_one_request_for_batchable_commands(
    client: Client, control: FakeControl, server: FakeInstance
) -> None:
    marker = client.markers.add(1000, label="m")
    other = client.markers.add(2000, label="n")
    before = len(control_requests(server))
    with client.batch() as batch:
        assert isinstance(batch, ControlBatch)
        client.workspace.equalize()
        client.workspace.set_scene_visible(True)
        client.playback.set(speed=2.0)
        marker.set(label="moved", time_ns=3000)
        other.remove()
        assert len(control_requests(server)) == before
        assert not other.stale
        assert len(batch) == 5
    requests = control_requests(server)[before:]
    assert len(requests) == 1
    assert requests[0].url.path == "/v1/control/batch"
    assert requests[0].headers["idempotency-key"]
    assert json.loads(requests[0].content) == {
        "commands": [
            {"op": "workspace_equalize"},
            {"op": "scene_set_visible", "visible": True},
            {"op": "playback_set", "speed": 2.0},
            {"op": "marker_set", "marker": marker.handle, "time_ns": 3000, "label": "moved"},
            {"op": "marker_remove", "marker": other.handle},
        ]
    }
    assert other.stale
    assert not marker.stale


def test_empty_batch_sends_nothing(client: Client, server: FakeInstance) -> None:
    before = len(server.requests)
    with client.batch():
        pass
    assert len(server.requests) == before


def test_exception_inside_batch_discards_queued_commands(
    client: Client, server: FakeInstance
) -> None:
    marker = client.markers.add(1000, label="m")
    before = len(server.requests)
    with pytest.raises(RuntimeError):
        with client.batch():
            client.playback.set(speed=2.0)
            marker.remove()
            raise RuntimeError("analysis failed")
    assert len(server.requests) == before
    assert not marker.stale
    client.playback.set(speed=1.0)
    assert last_command(server) == {"op": "playback_set", "speed": 1.0}


def test_batch_rejects_non_batchable_operations_before_http(
    client: Client, server: FakeInstance, arrow_table: pa.Table
) -> None:
    window = client.windows.open("w")
    plot = window.workspace.add_plot(direction="vertical")
    error = client.publish_topic("e", arrow_table).field("error")
    trace = plot.traces.add(error)
    annotation = plot.annotations.add_hline(1.0)
    vehicle = client.vehicles.add(
        source=client.publish_topic("f", arrow_table),
        position={"north": error, "east": error, "down": error},
    )
    rejected: list[Callable[[], object]] = [
        lambda: client.windows.open("x"),
        lambda: window.workspace.add_plot(direction="vertical"),
        lambda: plot.split(direction="vertical"),
        lambda: plot.close(),
        lambda: plot.traces.add(error),
        lambda: plot.traces.clear(),
        lambda: trace.set(visible=False),
        lambda: trace.remove(),
        lambda: plot.annotations.add_hline(2.0),
        lambda: annotation.set(label="x"),
        lambda: annotation.remove(),
        lambda: client.markers.add(1000),
        lambda: client.markers.list(),
        lambda: vehicle.set(label="x"),
        lambda: vehicle.save_profile("x"),
        lambda: client.vehicles.list(),
        lambda: client.vehicle_profiles.list(),
        lambda: client.layouts.list(),
        lambda: client.layouts.save("x"),
        lambda: client.state(),
        lambda: client.playback.state(),
        lambda: client.remove_owned(),
        lambda: client.snapshot(),
        lambda: client.publish_topic("g", arrow_table),
        lambda: client.batch(),
        lambda: client.request_status(idempotency_key="k-1"),
    ]
    before = len(server.requests)
    for operation in rejected:
        with pytest.raises(InvalidInputError):
            with client.batch():
                client.playback.set(speed=2.0)
                operation()
    assert len(server.requests) == before


def test_batch_rejects_per_command_keys(client: Client, server: FakeInstance) -> None:
    before = len(server.requests)
    with pytest.raises(InvalidInputError):
        with client.batch():
            client.playback.set(speed=2.0, idempotency_key="own-key")
    assert len(server.requests) == before


def test_closed_client_refuses_control(client: Client) -> None:
    window = client.windows.open("w")
    client.close()
    with pytest.raises(ForbiddenError):
        window.workspace.add_plot(direction="vertical")
    with pytest.raises(ForbiddenError):
        client.markers.add(1000)
