import numpy as np


@delog.live_transform(topic="PARAM_VALUE", fields=["param_id", "param_value"])
def split_param_values(batch):
    out = {}
    for param_id in np.unique(batch.param_id):
        if not param_id:
            continue
        mask = batch.param_id == param_id
        out[f"PARAM_VALUE/{param_id}"] = {"value": (batch.t[mask], batch.param_value[mask], None)}
    return out
