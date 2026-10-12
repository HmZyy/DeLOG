import csv
import io

import numpy as np

TIMESTAMP_COLUMN = None
TIMESTAMP_UNITS = "s"

TOPIC = "csv"
UNIT_SCALE = {"s": 1.0, "ms": 1e-3, "us": 1e-6, "ns": 1e-9}
RESERVED = {"rtc", "index"}


def _decode(raw_data):
    text = raw_data.view(np.uint8).tobytes().decode("utf-8-sig", errors="replace")
    if text and not text.endswith(("\n", "\r")):
        text = text[: max(text.rfind("\n"), 0)]
    return text


def _dialect(text):
    try:
        return csv.Sniffer().sniff(text[:65536], delimiters=",;\t|")
    except csv.Error:
        return csv.excel


def _to_float(values):
    try:
        return np.array(values, dtype=np.float64)
    except ValueError:
        out = np.full(len(values), np.nan)
        for i, v in enumerate(values):
            try:
                out[i] = float(v)
            except ValueError:
                pass
        return out


def _resolve_timestamp(header):
    if TIMESTAMP_COLUMN is None:
        return None
    if isinstance(TIMESTAMP_COLUMN, int):
        if not -len(header) <= TIMESTAMP_COLUMN < len(header):
            raise ValueError(f"TIMESTAMP_COLUMN {TIMESTAMP_COLUMN} is out of range for {len(header)} columns")
        return TIMESTAMP_COLUMN % len(header)
    if TIMESTAMP_COLUMN not in header:
        raise ValueError(f"TIMESTAMP_COLUMN {TIMESTAMP_COLUMN!r} not found; columns are {header}")
    return header.index(TIMESTAMP_COLUMN)


def _field_names(header):
    names, seen = [], set()
    for i, name in enumerate(header):
        name = name.strip() or f"col{i}"
        if name in RESERVED:
            name += "_"
        base, n = name, 1
        while name in seen:
            n += 1
            name = f"{base}_{n}"
        seen.add(name)
        names.append(name)
    return names


def Parse(raw_data):
    if TIMESTAMP_UNITS not in UNIT_SCALE:
        raise ValueError(f"TIMESTAMP_UNITS must be one of {sorted(UNIT_SCALE)}, got {TIMESTAMP_UNITS!r}")

    text = _decode(raw_data)
    rows = [r for r in csv.reader(io.StringIO(text), _dialect(text)) if any(c.strip() for c in r)]
    if not rows:
        raise ValueError("CSV file is empty")

    header, body = rows[0], rows[1:]
    width = len(header)
    body = [(r + [""] * width)[:width] for r in body]
    ts_idx = _resolve_timestamp(header)
    names = _field_names(header)

    columns = [_to_float([r[i].strip() for r in body]) for i in range(width)]

    if ts_idx is None:
        keep = np.ones(len(body), dtype=bool)
        rtc = np.arange(len(body), dtype=np.float64)
        rtc_tip = "row number (1 row = 1 second)"
    else:
        ts = columns[ts_idx] * UNIT_SCALE[TIMESTAMP_UNITS]
        keep = np.isfinite(ts)
        rtc = ts[keep]
        rtc_tip = f"{header[ts_idx]} converted from {TIMESTAMP_UNITS} to seconds"

    out = [(f"{TOPIC}.rtc", rtc, rtc_tip)]
    for i, (name, col) in enumerate(zip(names, columns)):
        col = col[keep]
        if i != ts_idx and col.size and np.isnan(col).all():
            continue
        out.append((f"{TOPIC}.{name}", col, f"column {header[i]!r}"))
    return out
