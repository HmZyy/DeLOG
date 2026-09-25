from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from delog_client import DeLOG, DeLOGError, Instance


def select_instance(instance_id: str | None) -> Instance:
    instances = DeLOG.list_instances()
    if instance_id is not None:
        for instance in instances:
            if instance.id == instance_id:
                return instance
        raise SystemExit(
            _selection_error(f"no running DeLOG instance has id {instance_id!r}")
        )
    if not instances:
        raise SystemExit(
            _selection_error("no running DeLOG instance has external access enabled")
        )
    if len(instances) > 1:
        listing = ", ".join(f"{i.id} ({i.label})" for i in instances)
        raise SystemExit(
            _selection_error(f"several DeLOG instances are running: {listing}")
        )
    return instances[0]


def _selection_error(message: str) -> int:
    print(f"{Path(sys.argv[0]).stem}: {message}; pass --instance ID", file=sys.stderr)
    return 2


def catalog(instance: Instance) -> dict[str, Any]:
    client = DeLOG.connect(instance.id, name="catalog-inspector")
    try:
        with client.snapshot() as snapshot:
            return {
                "instance": {
                    "id": instance.id,
                    "label": instance.label,
                    "loaded_file": instance.loaded_file,
                },
                "epoch": snapshot.epoch,
                "sources": [
                    {
                        "label": source.label,
                        "kind": source.kind,
                        "offset_ns": source.offset_ns,
                        "topics": [
                            {
                                "name": topic.name,
                                "base_name": topic.base_name,
                                "instance": topic.instance,
                                "row_count": topic.row_count,
                                "time_range_ns": None
                                if topic.time_range_ns is None
                                else [
                                    topic.time_range_ns.start_ns,
                                    topic.time_range_ns.end_ns,
                                ],
                                "fields": [
                                    {
                                        "name": field.name,
                                        "arrow_type": field.arrow_type,
                                        "unit": field.unit,
                                        "description": field.description,
                                        "multiplier": field.multiplier,
                                    }
                                    for field in topic.fields
                                ],
                            }
                            for topic in source.topics
                        ],
                    }
                    for source in snapshot.sources()
                ],
            }
    finally:
        client.close()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Print the sources, topics, and fields of a running DeLOG session as JSON."
    )
    parser.add_argument(
        "--instance", help="DeLOG instance ID; optional when exactly one runs"
    )
    parser.add_argument("--pretty", action="store_true", help="indent the JSON output")
    args = parser.parse_args(argv)
    instance = select_instance(args.instance)
    try:
        document = catalog(instance)
    except DeLOGError as error:
        print(f"catalog: {error}", file=sys.stderr)
        return 1
    json.dump(document, sys.stdout, indent=2 if args.pretty else None)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
