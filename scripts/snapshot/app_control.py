layouts = delog.layouts.list()
print(f"layouts ({len(layouts)})")
for name in layouts:
    print(name)

plots = delog.plots()
print(f"plots ({len(plots)})")
for plot in plots:
    print(plot.window, plot.index, plot.label)

plots[0].traces.add("ATT.Pitch")

plot = delog.workspace.add_plot(split="horizontal")
plot.annotations.add_text(
    (1_000_000, 0.0),
    "Python control annotation",
    color="#E74C3C",
)
delog.markers.add(
    1_000_000,
    "Python control marker",
    color="#2ECC71",
    note="Created by app_control.py",
)
delog.workspace.equalize()

print("added plot", plot.window, plot.index)
