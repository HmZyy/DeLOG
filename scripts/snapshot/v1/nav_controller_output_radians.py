TOPIC = "NAV_CONTROLLER_OUTPUT"
FIELDS = ("nav_roll", "nav_pitch", "nav_bearing")
DEG_TO_RAD = 0.017453292519943295


def find_nav_controller_output():
    candidates = {}
    for path in delog.sources():
        try:
            prefix, field = path.rsplit("/", 1)
            _source, topic = prefix.rsplit("/", 1)
        except ValueError:
            continue

        if topic == TOPIC or topic.startswith(f"{TOPIC}["):
            candidates.setdefault(prefix, set()).add(field)

    for prefix, fields in sorted(candidates.items()):
        if all(field in fields for field in FIELDS):
            return prefix

    raise RuntimeError(
        f"{TOPIC} with fields {', '.join(FIELDS)} was not found in the loaded data"
    )


base = find_nav_controller_output()

nav_roll = delog.field(f"{base}/nav_roll")
nav_pitch = delog.field(f"{base}/nav_pitch")
nav_bearing = delog.field(f"{base}/nav_bearing")

out = delog.output(nav_roll.t, "NAV_CONTROLLER_OUTPUT_RAD")
out.add_field("nav_roll_rad", nav_roll.v * DEG_TO_RAD, unit="rad")
out.add_field("nav_pitch_rad", nav_pitch.v * DEG_TO_RAD, unit="rad")
out.add_field("nav_bearing_rad", nav_bearing.v * DEG_TO_RAD, unit="rad")

print(f"converted {len(nav_roll.t)} {TOPIC} samples from degrees to radians")
