# ANA-02 - Visible-window field statistics (design)

Date: 2026-06-19 | PLAN.md item **ANA-02** | spec section 17.1 | milestone M10

## Summary

Extend the existing Field Stats popup with **Visible window** and **Global**
tabs. The Visible window tab is selected by default and updates automatically
as the shared plot window pans, zooms, or advances with live data. It reports
exact min, max, mean, standard deviation, sample count, missing count, and
sample rate for samples whose effective timestamps are in the inclusive range
`[t0_us, t1_us]`. Mean is the average, so the UI does not duplicate it under a
second label.

All exact calculations run outside the UI thread. Rapid view changes are
coalesced so work cannot queue without bound, and results for stale windows or
snapshot epochs never replace the current display.

## Decisions

- Extend the existing popup opened from browser and plot-trace context menus.
  A legend readout was rejected because seven metrics per trace would obscure
  the plot; a separate plot overlay would duplicate the existing popup flow.
- Use two tabs rather than stacked sections. **Visible window** opens by
  default; **Global** retains the existing ANA-01 results.
- Recompute automatically while panning and zooming, with request launches
  capped near 10 Hz. Min/max may appear immediately from an existing trace
  pyramid; an exact canonical result replaces it when the background job
  completes.
- Include samples at both window endpoints. Do not synthesize interpolated
  boundary values.
- Reuse seal-time `ColStats` for chunks fully contained in the window and scan
  only overlapping boundary slices. The job may use Rayon across chunks.
- Keep a single running calculation and one replaceable pending request. This
  preserves responsiveness and prevents a pan gesture from creating a job per
  frame.
- Memoize a small LRU keyed by `(field, snapshot_epoch, t0_us, t1_us)`.
- Report values in plotted units. Apply the schema multiplier in `f64`; for a
  negative multiplier, swap transformed min/max and scale standard deviation
  by its absolute value. This keeps both tabs consistent with the plot without
  relying on the `f32` render cache for final values.
- Popup selection, selected tab, job state, and memoized results are transient.
  No layout or settings schema changes are required.

## Core API

Add a window-scoped statistics type and helper in `delog-core::analysis`:

```rust
pub struct FieldStats {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub stddev: f64,
    pub count: u64,
    pub missing_count: u64,
    pub rate_hz: Option<f64>,
}

pub fn visible_field_stats(
    snapshot: &StoreSnapshot,
    field: FieldId,
    t0_us: i64,
    t1_us: i64,
) -> Result<Option<FieldStats>, FieldViewError>;
```

`Ok(None)` means the field is non-numeric, matching `global_field_stats`.
Invalid fields, topics, or schemas use the existing `FieldViewError` variants.
The function reads Arrow arrays in place and creates no sample-value copy,
upholding ZC-2.

For every chunk, source offset is applied when testing effective timestamps.
Chunks wholly outside the inclusive window are skipped. For wholly contained
chunks, fold the field's sealed `ColStats`. For partial overlaps, binary-search
the sorted timestamp array to obtain an Arrow range and scan its numeric values
in place. If the topic spine is non-monotonic or overlapping, process each
chunk independently; correctness does not depend on spine order.

The partial results contain min, max, sum, sum of squares, valid count, missing
count, and first/last included effective timestamps. Reduction produces the
same population standard deviation convention as ANA-01. Rate is
`count / ((last_time - first_time) / 1e6)` when at least two distinct included
times exist; otherwise it is absent. When no valid numeric samples exist,
count is zero, numeric values are NaN, and rate is absent.

The existing global and new visible helpers should share internal reduction and
unit-transformation helpers where that removes duplication, without changing
the public ANA-01 behavior beyond making multiplier handling consistent.

## App Job Manager

Add a focused visible-stats controller in `delog-app`. It owns:

- the selected field and selected popup tab;
- the current request key;
- at most one running job and one newest pending request;
- a result channel;
- a small recent-result LRU;
- the last accepted result and its status (`Idle`, `Updating`, or `Error`).

On each UI update while the popup is open on Visible window, compare the
current `(field, epoch, t0, t1)` with the controller key. A changed key updates
the pending request. Launch no more often than the interaction refresh cap. A
worker owns an `Arc<StoreSnapshot>`, so its epoch remains coherent for the
whole calculation and old chunks remain valid until it exits.

Poll completed work before drawing the popup. Accept a result only when its
full key equals the controller's current key. Otherwise discard it. After a job
finishes, launch the newest pending request when the rate cap permits. Closing
the popup clears pending work and stops new launches; an already running job
may finish and its result is discarded.

The LRU returns exact results immediately when revisiting a recent window. A
new live snapshot epoch is a different key, so appended data cannot reuse a
stale result.

## Popup UI

The window title and existing entry points remain unchanged. Add two tabs:

1. **Visible window** (default) shows the current window bounds, calculation
   status, and the seven statistic rows.
2. **Global** shows the existing ANA-01 whole-field statistics.

While a new visible result is running, retain the last accepted values but dim
them and show **Updating...** next to the current requested bounds. If the
trace cache is ready, its indexed min/max can be shown immediately as a
provisional display; the canonical `f64` result replaces them on completion.
If no prior result exists, rows show `-` until the result arrives.

An empty numeric window shows `-` for min, max, mean, standard deviation, and
rate, with `0` samples. Missing count reflects null and NaN samples whose
timestamps are inside the window, matching the existing `ColStats` convention.
Non-numeric fields show the existing
unsupported message on both tabs. Worker errors appear inline and do not close
the popup. If the selected field disappears from the snapshot, close the popup
as it does today.

## Performance

The UI thread only builds keys, polls a channel, performs LRU lookups, and
formats accepted results. It never scans samples or waits for a worker.

The common case folds `ColStats` for interior chunks and scans at most the two
window-edge portions per ordered spine. Fragmented or overlapping data can
touch more boundary chunks, so the implementation still runs as a background
job. Coalescing bounds queued work regardless of input rate. The refresh cap
prevents a long pan from continuously saturating all worker threads while
remaining visually responsive.

Add or update a Criterion benchmark for small windows, fragmented windows, and
multi-million-sample windows. Record the measured behavior in the ANA-02
checklist summary; do not declare a latency budget without benchmark evidence.

## Testing

Core unit and property tests cover:

- inclusion of samples exactly at `t0_us` and `t1_us`;
- empty and single-timestamp windows;
- null and NaN handling;
- source time offsets;
- wholly contained chunk folding and partial boundary scans;
- overlapping/non-monotonic chunk spines;
- positive, negative, and fractional schema multipliers;
- equality with a naive full scan for random samples and windows.

App tests cover request coalescing, launch throttling, stale window and epoch
rejection, LRU hits, close-with-work-in-flight behavior, default tab selection,
and non-numeric/error display states. The tab layout and dimmed updating state
also receive a manual in-app check.

Completion requires `cargo fmt --all`, warning-free workspace Clippy, workspace
tests, and benchmark compilation. Because this changes a statistics/query hot
path, run the relevant Criterion benchmark as required by PLAN.md section 0.

## Out of Scope

Median, percentiles, histograms, selection-range statistics, multiple
simultaneous Field Stats windows, persistent popup/tab state, exporting popup
results, and a permanent per-field prefix/segment statistics index.

## Checklist

Implement under PLAN.md item ANA-02. Mark it `[~]` when implementation starts
and `[x]` only after the Definition of Done is met, updating the checklist in
the same implementation commit.
