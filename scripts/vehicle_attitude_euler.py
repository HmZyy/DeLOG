import numpy as np

TOPIC = "vehicle_attitude[0]"

suffix = f"/{TOPIC}/q[0]"
prefix = next((p[: -len(suffix)] for p in delog.sources() if p.endswith(suffix)), None)
if prefix is None:
    raise RuntimeError(f"{TOPIC}/q[0] not found in this log")

qw = delog.field(f"{prefix}/{TOPIC}/q[0]")
t = qw.t
w = qw.v
x = delog.field(f"{prefix}/{TOPIC}/q[1]").v
y = delog.field(f"{prefix}/{TOPIC}/q[2]").v
z = delog.field(f"{prefix}/{TOPIC}/q[3]").v

if not (len(w) == len(x) == len(y) == len(z) == len(t)):
    raise RuntimeError("quaternion components have mismatched lengths")

norm = np.sqrt(w * w + x * x + y * y + z * z)
norm[norm == 0.0] = 1.0
w, x, y, z = w / norm, x / norm, y / norm, z / norm

roll = np.arctan2(2.0 * (w * x + y * z), 1.0 - 2.0 * (x * x + y * y))
pitch = np.arcsin(np.clip(2.0 * (w * y - z * x), -1.0, 1.0))
yaw = np.arctan2(2.0 * (w * z + x * y), 1.0 - 2.0 * (y * y + z * z))

out = delog.output(t, "vehicle_attitude_euler")
out.add_field("roll", np.degrees(roll), unit="deg")
out.add_field("pitch", np.degrees(pitch), unit="deg")
out.add_field("yaw", np.degrees(yaw), unit="deg")

print(f"vehicle_attitude_euler: {len(t)} samples from {prefix}/{TOPIC}")
