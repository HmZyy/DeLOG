DEG_TO_RAD = 0.017453292519943295


@delog.live_transform(
    topic="NAV_CONTROLLER_OUTPUT",
    fields=["nav_roll", "nav_pitch", "nav_bearing"],
    output_topic="NAV_CONTROLLER_OUTPUT_RAD",
)
def nav_controller_rad(batch):
    return {
        "nav_roll_rad": (batch.nav_roll * DEG_TO_RAD, "rad"),
        "nav_pitch_rad": (batch.nav_pitch * DEG_TO_RAD, "rad"),
        "nav_bearing_rad": (batch.nav_bearing * DEG_TO_RAD, "rad"),
    }
