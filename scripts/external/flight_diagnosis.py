from __future__ import annotations

import argparse
import sys
from typing import cast

import pyarrow as pa
from catalog import select_instance
from delog_client import Client, DeLOG, DeLOGError

OWNER = "flight-diagnosis"
TOPIC = "diagnostic_sample_gaps"
MAX_MARKS = 50


def sample_gaps(
    client: Client, source: str | None, topic: str, field: str
) -> tuple[list[int], list[float]]:
    times: list[int] = []
    gaps: list[float] = []
    previous: int | None = None
    with client.snapshot() as snapshot:
        selected = snapshot.topic(topic, source=source)
        for batch in selected.iter_batches(fields=[field]):
            for time_ns in cast(list[int], batch.column("__delog_time_ns").to_pylist()):
                if previous is not None:
                    times.append(time_ns)
                    gaps.append((time_ns - previous) / 1_000_000)
                previous = time_ns
    return times, gaps


def diagnose(client: Client, args: argparse.Namespace) -> str:
    times, gaps = sample_gaps(client, args.source, args.topic, args.field)
    table = pa.table(
        {
            "__delog_time_ns": pa.array(times, pa.int64()),
            "gap_ms": pa.array(gaps, pa.float64()),
        }
    )
    derived = client.publish_topic(
        TOPIC,
        table,
        units={"gap_ms": "ms"},
        descriptions={
            "gap_ms": f"time since the previous {args.topic}.{args.field} sample"
        },
        replace=args.replace,
    )
    plot = client.windows.open("Flight diagnosis").workspace.add_plot()
    plot.traces.add(derived.field("gap_ms"))

    over = [(t, gap) for t, gap in zip(times, gaps, strict=True) if gap > args.gap_ms]
    for time_ns, gap in over[:MAX_MARKS]:
        label = f"gap {gap:.1f} ms"
        client.markers.add(time_ns, label)
        plot.annotations.add_text(time_ns=time_ns, value=gap, text=label)
    if not over:
        message = f"no gap above {args.gap_ms} ms"
        plot.annotations.add_text(
            time_ns=times[-1] if times else 0, value=args.gap_ms, text=message
        )
        return message
    shown = "" if len(over) <= MAX_MARKS else f", first {MAX_MARKS} marked"
    return f"{len(over)} gaps above {args.gap_ms} ms{shown}"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Publish the sample gaps of one field and mark gaps above a threshold."
    )
    parser.add_argument(
        "--instance", help="DeLOG instance ID; optional when exactly one runs"
    )
    parser.add_argument(
        "--source", help="source label, when the topic name is ambiguous"
    )
    parser.add_argument("--topic", required=True, help="topic to inspect")
    parser.add_argument("--field", required=True, help="field whose samples are timed")
    parser.add_argument(
        "--gap-ms",
        type=float,
        default=100.0,
        help="threshold in milliseconds (default 100.0)",
    )
    parser.add_argument(
        "--replace", action="store_true", help=f"replace an earlier {TOPIC} publication"
    )
    args = parser.parse_args(argv)
    instance = select_instance(args.instance)
    client = DeLOG.connect(instance.id, name=OWNER)
    try:
        print(diagnose(client, args))
    except DeLOGError as error:
        print(f"flight_diagnosis: {error}", file=sys.stderr)
        return 1
    finally:
        client.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
