import numpy as np

# A slider-controlled exponential low-pass smoothing factor. Because the
# callback reads delog.param("alpha") each batch, moving the slider in
# Tools > Scripts > Variables changes the filter live, with no re-run.
delog.slider("alpha", 0.2, min=0.01, max=1.0, step=0.01,
             label="LPF alpha (higher = less smoothing)")


@delog.live_transform(topic="IMU", fields=["AccX"], output_topic="IMU_LPF")
def lowpass(batch):
    alpha = delog.param("alpha")
    x = batch.AccX
    y = np.empty_like(x)
    acc = x[0] if len(x) else 0.0
    for i in range(len(x)):
        acc = alpha * x[i] + (1.0 - alpha) * acc
        y[i] = acc
    return {"AccX_lpf": (y, "m/s^2")}
