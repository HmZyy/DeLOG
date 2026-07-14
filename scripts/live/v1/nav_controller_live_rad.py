DEG_TO_RAD = 0.017453292519943295

delog.transform(
    "NAV_CONTROLLER_OUTPUT",
    multiplier=DEG_TO_RAD,
    fields=["nav_roll", "nav_pitch", "nav_bearing"],
    unit="rad",
    output_topic="NAV_CONTROLLER_OUTPUT_RAD",
)
