import numpy as np


@delog.live_transform(topic="NAMED_VALUE_FLOAT", fields=["name", "value"])
def split_named_floats(batch):
    out = {}
    for name in np.unique(batch.name):
        mask = batch.name == name
        out[f"NAMED_VALUE_FLOAT/{name}"] = {"value": (batch.t[mask], batch.value[mask], None)}
    return out


@delog.live_transform(topic="NAMED_VALUE_INT", fields=["name", "value"])
def split_named_ints(batch):
    out = {}
    for name in np.unique(batch.name):
        mask = batch.name == name
        out[f"NAMED_VALUE_INT/{name}"] = {"value": (batch.t[mask], batch.value[mask], None)}
    return out
