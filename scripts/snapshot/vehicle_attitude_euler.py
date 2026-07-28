import numpy as np

TOPIC = "vehicle_attitude"

att_topic = delog.topic(TOPIC, instance=0)
att = att_topic.read("q[0]", "q[1]", "q[2]", "q[3]")

w = att["q[0]"]
x = att["q[1]"]
y = att["q[2]"]
z = att["q[3]"]

norm = np.sqrt(w * w + x * x + y * y + z * z)
norm[norm == 0.0] = 1.0
w, x, y, z = w / norm, x / norm, y / norm, z / norm

roll = np.arctan2(2.0 * (w * x + y * z), 1.0 - 2.0 * (x * x + y * y))
pitch = np.arcsin(np.clip(2.0 * (w * y - z * x), -1.0, 1.0))
yaw = np.arctan2(2.0 * (w * z + x * y), 1.0 - 2.0 * (y * y + z * z))

delog.emit("vehicle_attitude_euler", att.t, {
    "roll": (np.degrees(roll), "deg"),
    "pitch": (np.degrees(pitch), "deg"),
    "yaw": (np.degrees(yaw), "deg"),
})

print(f"vehicle_attitude_euler: {len(att.t)} samples from {att_topic.source}/{att_topic.name}")
