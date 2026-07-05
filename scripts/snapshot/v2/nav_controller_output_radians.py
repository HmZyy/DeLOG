TOPIC = "NAV_CONTROLLER_OUTPUT"
DEG_TO_RAD = 0.017453292519943295

nav = delog.topic(TOPIC).read("nav_roll", "nav_pitch", "nav_bearing")

delog.emit("NAV_CONTROLLER_OUTPUT_RAD", nav.t, {
    "nav_roll_rad": (nav.nav_roll * DEG_TO_RAD, "rad"),
    "nav_pitch_rad": (nav.nav_pitch * DEG_TO_RAD, "rad"),
    "nav_bearing_rad": (nav.nav_bearing * DEG_TO_RAD, "rad"),
})

print(f"converted {len(nav.t)} {TOPIC} samples from degrees to radians")
