//! Per-trace f32 render cache — the One Copy (PLAN.md §8.1-§8.2, CCH-01/02/04).
//!
//! Exactly one transform copy per plotted field (ZC-3): the builder iterates the
//! snapshot's canonical Arrow chunks in place (ZC-2), and per sample applies the
//! schema multiplier in `f64`, casts to `f32`, and rebases effective time to
//! seconds against a shared `origin_us`. The result is one interleaved
//! `[x0,y0,x1,y1,…]` buffer (8 B/sample) mirrored 1:1 into a GPU storage buffer.
//! Non-finite values stay `NaN` so the line shader breaks the segment (§9.4) —
//! gaps render as gaps. On a later epoch the cache *appends* new rows; it never
//! rebuilds (CCH-04).

use arrow::array::{
    Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::DataType;

use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

use crate::pyramid::{BRANCH, MinMax, MinMaxPyramid};

/// The f32 render cache for one field.
#[derive(Debug)]
pub struct TraceCache {
    /// Interleaved `[x0,y0,x1,y1,…]`; one buffer, one GPU upload span.
    pub xy: Vec<f32>,
    /// Time rebase origin: `x = (t_eff - origin_us) * 1e-6` as f32 (§8.3).
    pub origin_us: i64,
    /// Rows already transformed — the high-water mark vs the canonical store.
    pub built_rows: u64,
    /// Min/max index over the y-channel of `xy` (stride 2, offset 1).
    pub pyramid: MinMaxPyramid,
    /// Frame of last use, for LRU eviction (CCH-09).
    pub last_used_frame: u64,
    /// Source offset baked into the x values; a snapshot with a different
    /// offset makes this cache stale (BRW-07 → rebuild, not append).
    pub offset_us: i64,
}

/// What a field resolves to in a snapshot.
struct Resolved<'a> {
    store: &'a TopicStore,
    col_index: usize,
    offset_us: i64,
    multiplier: f64,
}

fn resolve(snapshot: &StoreSnapshot, field: FieldId) -> Option<Resolved<'_>> {
    let entry = snapshot
        .fields
        .get(field.index())
        .filter(|f| f.id == field && !f.removed)?;
    let topic = snapshot.topic(entry.topic).filter(|t| !t.entry.removed)?;
    let store = topic.store.as_deref()?;
    let source = snapshot.source(topic.entry.source)?;
    let col_index = store.schema.field_index(&entry.name)?;
    let multiplier = store.schema.field(col_index)?.multiplier;
    Some(Resolved {
        store,
        col_index,
        offset_us: source.entry.offset_us,
        multiplier,
    })
}

impl TraceCache {
    /// Build the cache for `field` against `origin_us`, off the UI thread
    /// (CCH-02/03). Returns `None` if the field has no data in this snapshot.
    pub fn build(
        snapshot: &StoreSnapshot,
        field: FieldId,
        origin_us: i64,
        frame: u64,
    ) -> Option<Self> {
        let r = resolve(snapshot, field)?;
        let mut xy = Vec::with_capacity(r.store.rows as usize * 2);
        for chunk in r.store.chunks.iter() {
            append_chunk(
                &mut xy,
                &chunk.t,
                chunk.cols[r.col_index].as_ref(),
                r.offset_us,
                origin_us,
                r.multiplier,
                0,
            );
        }
        let pyramid = MinMaxPyramid::build_strided(&xy, 2, 1);
        Some(Self {
            xy,
            origin_us,
            built_rows: r.store.rows,
            pyramid,
            last_used_frame: frame,
            offset_us: r.offset_us,
        })
    }

    /// Whether `snapshot` carries a different source offset than this cache
    /// was built with — its x values are stale and need a rebuild (BRW-07).
    pub fn offset_changed(&self, snapshot: &StoreSnapshot, field: FieldId) -> bool {
        resolve(snapshot, field).is_some_and(|r| r.offset_us != self.offset_us)
    }

    /// Append rows that arrived since the last build/append (CCH-04). Returns
    /// `true` if any rows were added. Uses the cache's fixed `origin_us`; a
    /// changed global origin is a rebuild, not an append (handled by the
    /// manager, §8.3).
    pub fn append(&mut self, snapshot: &StoreSnapshot, field: FieldId) -> bool {
        let Some(r) = resolve(snapshot, field) else {
            return false;
        };
        if r.store.rows <= self.built_rows {
            return false;
        }

        let mut consumed = 0u64;
        for chunk in r.store.chunks.iter() {
            let len = chunk.len() as u64;
            if consumed + len > self.built_rows {
                // Start row within this chunk (0 once we are past built_rows).
                let start = self.built_rows.saturating_sub(consumed) as usize;
                append_chunk(
                    &mut self.xy,
                    &chunk.t,
                    chunk.cols[r.col_index].as_ref(),
                    r.offset_us,
                    self.origin_us,
                    r.multiplier,
                    start,
                );
            }
            consumed += len;
        }

        self.pyramid.extend(&self.xy);
        self.built_rows = r.store.rows;
        true
    }

    /// Number of samples cached.
    pub fn samples(&self) -> usize {
        self.xy.len() / 2
    }

    pub fn is_empty(&self) -> bool {
        self.xy.is_empty()
    }

    /// x (seconds, rebased to `origin_us`) of sample `i`.
    fn x_at(&self, i: usize) -> f32 {
        self.xy[2 * i]
    }

    /// Sample index range `[a, b)` whose x falls in `[x0, x1]` (seconds). The x
    /// channel is sorted, so this is two binary searches.
    pub fn index_range(&self, x0: f32, x1: f32) -> (usize, usize) {
        let n = self.samples();
        let a = self.partition_point(0, n, |x| x < x0); // first x >= x0
        let b = self.partition_point(a, n, |x| x <= x1); // first x > x1
        (a, b)
    }

    /// First index in `[lo, hi)` whose x fails `pred` (the x channel is sorted,
    /// so `pred` is monotone-true then monotone-false).
    fn partition_point(&self, mut lo: usize, mut hi: usize, pred: impl Fn(f32) -> bool) -> usize {
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if pred(self.x_at(mid)) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Min/max y over the visible x window `[x0, x1]` (seconds) — the
    /// AutoVisible y range (PLT-06). One sample of context is included on each
    /// side so a line entering/leaving the window is bounded.
    pub fn y_range(&self, x0: f32, x1: f32) -> MinMax {
        let (a, b) = self.index_range(x0, x1);
        let a = a.saturating_sub(1);
        let b = (b + 1).min(self.samples());
        self.pyramid.query(&self.xy, a, b)
    }

    /// Per-pixel-column `[x, min, max]` triples over `[x0, x1)` split into
    /// `width` equal **time** columns — the decimated draw input (§9.5, GPU-09).
    /// Columns are half-open `[x0 + c·s, x0 + (c+1)·s)`; an empty column reports
    /// `NaN` (the shader skips it). Splitting by time, not index, keeps columns
    /// aligned to screen pixels even for irregularly-sampled data.
    ///
    /// Wide columns (≥ one pyramid node each) walk the compact L0 array —
    /// O(visible_nodes), 64× fewer items than the samples — binning each node by
    /// its x range; narrow columns sweep the (necessarily small) visible sample
    /// range exactly. Both are sequential and cache-friendly.
    pub fn minmax_columns(&self, x0: f32, x1: f32, width: usize, bridge: bool) -> Vec<f32> {
        if width == 0 || x1 <= x0 {
            return Vec::new();
        }
        let (a, b) = self.index_range(x0, x1);
        let mut mins = vec![f32::NAN; width];
        let mut maxs = vec![f32::NAN; width];

        if b.saturating_sub(a) >= width * BRANCH {
            self.l0_columns(x0, x1, a, b, &mut mins, &mut maxs);
        } else {
            self.sweep_columns(x0, x1, a, b, &mut mins, &mut maxs);
        }

        if bridge {
            bridge_columns(&mut mins, &mut maxs);
        }

        let span = (x1 - x0) / width as f32;
        let mut out = Vec::with_capacity(width * 3);
        for c in 0..width {
            out.push(x0 + span * (c as f32 + 0.5));
            out.push(mins[c]);
            out.push(maxs[c]);
        }
        out
    }

    /// Exact: sweep samples `[a, b)`, bucketing each by half-open column.
    fn sweep_columns(
        &self,
        x0: f32,
        x1: f32,
        a: usize,
        b: usize,
        mins: &mut [f32],
        maxs: &mut [f32],
    ) {
        let width = mins.len();
        let inv = 1.0 / (x1 - x0);
        for i in a..b {
            let y = self.xy[2 * i + 1];
            if y.is_nan() {
                continue;
            }
            let col = col_index(self.x_at(i), x0, inv, width);
            if y < mins[col] || mins[col].is_nan() {
                mins[col] = y;
            }
            if y > maxs[col] || maxs[col].is_nan() {
                maxs[col] = y;
            }
        }
    }

    /// Fast: distribute each L0 node overlapping `[a, b)` to the column(s) its x
    /// range covers. Conservative at column/edge boundaries (never hides a
    /// transient, may smear it one column — fine for decimation, §9.5).
    fn l0_columns(&self, x0: f32, x1: f32, a: usize, b: usize, mins: &mut [f32], maxs: &mut [f32]) {
        let width = mins.len();
        let inv = 1.0 / (x1 - x0);
        let n = self.samples();
        let l0 = self.pyramid.l0();
        let first = a / BRANCH;
        let last = (b.saturating_sub(1) / BRANCH).min(l0.len().saturating_sub(1));
        if first > last {
            return;
        }
        for (offset, node) in l0[first..=last].iter().enumerate() {
            if !node.is_finite() {
                continue;
            }
            let node_idx = first + offset;
            let s0 = node_idx * BRANCH;
            let s1 = ((node_idx + 1) * BRANCH).min(n) - 1;
            let nx0 = self.x_at(s0);
            let nx1 = self.x_at(s1);
            if nx1 < x0 || nx0 > x1 {
                continue;
            }
            let cl = col_index(nx0.max(x0), x0, inv, width);
            let cr = col_index(nx1.min(x1), x0, inv, width);
            for col in cl..=cr {
                if node.min < mins[col] || mins[col].is_nan() {
                    mins[col] = node.min;
                }
                if node.max > maxs[col] || maxs[col].is_nan() {
                    maxs[col] = node.max;
                }
            }
        }
    }

    /// CPU bytes held (xy buffer + pyramid), for `MemBreakdown` (CCH-10).
    pub fn bytes(&self) -> u64 {
        (self.xy.capacity() * std::mem::size_of::<f32>()) as u64 + self.pyramid.bytes()
    }

    pub fn touch(&mut self, frame: u64) {
        self.last_used_frame = frame;
    }
}

/// Bridge gaps between adjacent decimated columns (§9.5). Per-column min/max
/// bars are drawn disjoint by the shader, so a smooth, moderately-sloped
/// signal — where each column's own span is smaller than the value change to
/// its neighbour — reads as a broken/dashed line. Stretch each finite column's
/// span just enough to meet its right neighbour's, so consecutive bars always
/// touch. This only ever *grows* a span (a transient is never hidden, §9.5),
/// and `NaN` (empty) columns stay gaps so real data gaps are preserved.
fn bridge_columns(mins: &mut [f32], maxs: &mut [f32]) {
    for c in 0..mins.len().saturating_sub(1) {
        let (cur_min, cur_max) = (mins[c], maxs[c]);
        let (nxt_min, nxt_max) = (mins[c + 1], maxs[c + 1]);
        if cur_min.is_nan() || nxt_min.is_nan() {
            continue;
        }
        if cur_max < nxt_min {
            // The next column sits entirely above: raise this column to meet it.
            maxs[c] = nxt_min;
        } else if nxt_max < cur_min {
            // The next column sits entirely below: lower this column to meet it.
            mins[c] = nxt_max;
        }
    }
}

/// Column index for `x` in a `width`-column window starting at `x0` with
/// inverse span `inv`, clamped to `[0, width)`.
fn col_index(x: f32, x0: f32, inv: f32, width: usize) -> usize {
    let c = ((x - x0) * inv * width as f32) as i64;
    c.clamp(0, width as i64 - 1) as usize
}

/// Transform `chunk[start..]` of one column into interleaved x,y f32 pairs.
fn append_chunk(
    xy: &mut Vec<f32>,
    t: &Int64Array,
    col: &dyn Array,
    offset_us: i64,
    origin_us: i64,
    multiplier: f64,
    start: usize,
) {
    let reader = ColReader::new(col);
    for i in start..t.len() {
        let eff = t.value(i).saturating_add(offset_us);
        let x = ((eff.saturating_sub(origin_us)) as f64 * 1e-6) as f32;
        let y = (reader.value(i) * multiplier) as f32;
        xy.push(x);
        xy.push(y);
    }
}

/// A column downcast once, yielding `f64` per row (NaN for null / non-numeric).
enum ColReader<'a> {
    I8(&'a Int8Array),
    I16(&'a Int16Array),
    I32(&'a Int32Array),
    I64(&'a Int64Array),
    U8(&'a UInt8Array),
    U16(&'a UInt16Array),
    U32(&'a UInt32Array),
    U64(&'a UInt64Array),
    F32(&'a Float32Array),
    F64(&'a Float64Array),
    Bool(&'a BooleanArray),
    /// Strings/blobs are not plottable (ZC-6) → all NaN.
    NonNumeric,
}

impl<'a> ColReader<'a> {
    fn new(col: &'a dyn Array) -> Self {
        let any = col.as_any();
        match col.data_type() {
            DataType::Int8 => Self::I8(any.downcast_ref().unwrap()),
            DataType::Int16 => Self::I16(any.downcast_ref().unwrap()),
            DataType::Int32 => Self::I32(any.downcast_ref().unwrap()),
            DataType::Int64 => Self::I64(any.downcast_ref().unwrap()),
            DataType::UInt8 => Self::U8(any.downcast_ref().unwrap()),
            DataType::UInt16 => Self::U16(any.downcast_ref().unwrap()),
            DataType::UInt32 => Self::U32(any.downcast_ref().unwrap()),
            DataType::UInt64 => Self::U64(any.downcast_ref().unwrap()),
            DataType::Float32 => Self::F32(any.downcast_ref().unwrap()),
            DataType::Float64 => Self::F64(any.downcast_ref().unwrap()),
            DataType::Boolean => Self::Bool(any.downcast_ref().unwrap()),
            _ => Self::NonNumeric,
        }
    }

    #[inline]
    fn value(&self, i: usize) -> f64 {
        macro_rules! num {
            ($a:expr) => {
                if $a.is_null(i) {
                    f64::NAN
                } else {
                    $a.value(i) as f64
                }
            };
        }
        match self {
            Self::I8(a) => num!(a),
            Self::I16(a) => num!(a),
            Self::I32(a) => num!(a),
            Self::I64(a) => num!(a),
            Self::U8(a) => num!(a),
            Self::U16(a) => num!(a),
            Self::U32(a) => num!(a),
            Self::U64(a) => num!(a),
            Self::F32(a) => num!(a),
            Self::F64(a) => num!(a),
            Self::Bool(a) => {
                if a.is_null(i) {
                    f64::NAN
                } else if a.value(i) {
                    1.0
                } else {
                    0.0
                }
            }
            Self::NonNumeric => f64::NAN,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Int32Array};
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};

    use super::*;

    /// One source/topic with an Int32 field `Alt` (cm → ×0.01) over `times`.
    fn snapshot_with(
        times: Vec<i64>,
        alts: Vec<Option<i32>>,
        offset_us: i64,
    ) -> (StoreSnapshot, FieldId) {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        identity.set_source_offset_us(source, offset_us);
        let topic = identity.add_topic(source, "BARO").unwrap();
        let field = identity.add_field(topic, "Alt").unwrap();

        let schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Int32, Some("cm"), 0.01).unwrap()],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![Arc::new(Int32Array::from(alts))];
        let chunk = Arc::new(Chunk::try_new(Int64Array::from(times), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());
        let snap = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();
        (snap, field)
    }

    #[test]
    fn build_applies_multiplier_offset_and_rebase() {
        // raw cm: 100, 200, 300 → metres 1.0, 2.0, 3.0. offset +5_000_000 µs.
        let (snap, field) = snapshot_with(
            vec![1_000_000, 2_000_000, 3_000_000],
            vec![Some(100), Some(200), Some(300)],
            5_000_000,
        );
        // origin = first effective time = 1_000_000 + 5_000_000.
        let origin = 6_000_000;
        let cache = TraceCache::build(&snap, field, origin, 0).unwrap();

        assert_eq!(cache.samples(), 3);
        assert_eq!(cache.built_rows, 3);
        // x = (t + offset - origin) * 1e-6 seconds.
        assert_eq!(cache.xy[0], 0.0); // first sample at origin
        assert_eq!(cache.xy[2], 1.0); // +1 s
        assert_eq!(cache.xy[4], 2.0);
        // y = raw * 0.01.
        assert_eq!(cache.xy[1], 1.0);
        assert_eq!(cache.xy[3], 2.0);
        assert_eq!(cache.xy[5], 3.0);

        let q = cache.pyramid.query(&cache.xy, 0, 3);
        assert_eq!(q.min, 1.0);
        assert_eq!(q.max, 3.0);
    }

    #[test]
    fn null_cells_become_nan_gaps() {
        let (snap, field) = snapshot_with(vec![0, 1, 2], vec![Some(100), None, Some(300)], 0);
        let cache = TraceCache::build(&snap, field, 0, 0).unwrap();
        assert_eq!(cache.xy[1], 1.0);
        assert!(cache.xy[3].is_nan()); // the gap
        assert_eq!(cache.xy[5], 3.0);
        // Pyramid ignores the NaN.
        let q = cache.pyramid.query(&cache.xy, 0, 3);
        assert_eq!(q.min, 1.0);
        assert_eq!(q.max, 3.0);
    }

    #[test]
    fn index_range_and_y_range_target_the_visible_window() {
        // t = 0,1,2,3,4 s (µs), raw alt = 0,100,200,300,400 cm → 0..4 m.
        let (snap, field) = snapshot_with(
            vec![0, 1_000_000, 2_000_000, 3_000_000, 4_000_000],
            vec![Some(0), Some(100), Some(200), Some(300), Some(400)],
            0,
        );
        let cache = TraceCache::build(&snap, field, 0, 0).unwrap();

        // Window [1.0, 3.0] s covers samples at indices 1,2,3.
        let (a, b) = cache.index_range(1.0, 3.0);
        assert_eq!((a, b), (1, 4));

        // y over that window, with one sample of context each side, spans the
        // full set here → 0..4.
        let mm = cache.y_range(1.0, 3.0);
        assert_eq!(mm.min, 0.0);
        assert_eq!(mm.max, 4.0);

        // A tight inner window still bounds correctly.
        let mm = cache.y_range(2.0, 2.0);
        assert!(mm.is_finite());
    }

    #[test]
    fn minmax_columns_split_by_time_into_triples() {
        let (snap, field) = snapshot_with(
            vec![0, 1_000_000, 2_000_000, 3_000_000, 4_000_000],
            vec![Some(0), Some(100), Some(200), Some(300), Some(400)],
            0,
        );
        let cache = TraceCache::build(&snap, field, 0, 0).unwrap();

        // 4 columns over [0,4]s → 1-second half-open columns [c, c+1).
        let cols = cache.minmax_columns(0.0, 4.0, 4, true);
        assert_eq!(cols.len(), 4 * 3);
        // Column 0 = [0,1)s holds only the sample at t=0 (y=0); centre x=0.5.
        assert_eq!(cols[0], 0.5);
        assert_eq!(cols[1], 0.0); // col0 min
        // col0's max is bridged up to col1's value so the bars connect (§9.5).
        assert_eq!(cols[2], 1.0); // col0 max, bridged to col1 min
        // Column 1 = [1,2)s holds t=1 → y=1.
        assert_eq!(cols[4], 1.0); // col1 min
        // Last column [3,4) holds t=3 and the boundary t=4 → max 4 (unchanged).
        assert_eq!(cols[11], 4.0);
        // Monotone ramp: each column's max rises.
        assert!(cols[5] > cols[2]);
        // Adjacent columns touch after bridging: col c's max == col c+1's min.
        for c in 0..3 {
            assert_eq!(cols[c * 3 + 2], cols[(c + 1) * 3 + 1], "column {c} bridges");
        }
    }

    #[test]
    fn l0_walk_path_preserves_min_max_and_transients() {
        // 1000 samples → with width 4 each column is ~250 samples (≥64), so the
        // fast L0-walk path runs. y = i, plus a single spike in column 1.
        let times: Vec<i64> = (0..1000).collect(); // µs
        let mut alts: Vec<Option<i32>> = (0..1000).map(|i| Some(i * 100)).collect(); // y = i
        alts[300] = Some(999_900); // y = 9999 spike, in column 1
        let (snap, field) = snapshot_with(times, alts, 0);
        let cache = TraceCache::build(&snap, field, 0, 0).unwrap();

        let x1 = 999.0 * 1e-6;
        let cols = cache.minmax_columns(0.0, x1, 4, true);
        assert_eq!(cols.len(), 12);
        // Column 0 starts at y=0; the last column reaches the max y≈999.
        assert_eq!(cols[1], 0.0); // col0 min
        assert!(cols[11] >= 990.0); // col3 max near 999
        // The single-sample spike is NOT decimated away (§9.5).
        assert!(
            (cols[5] - 9999.0).abs() < 1.0,
            "spike preserved, got {}",
            cols[5]
        );
    }

    #[test]
    fn append_adds_only_new_rows_and_extends_the_pyramid() {
        let (snap1, field) = snapshot_with(vec![0, 1_000_000], vec![Some(100), Some(200)], 0);
        let mut cache = TraceCache::build(&snap1, field, 0, 0).unwrap();
        assert_eq!(cache.samples(), 2);

        // A later snapshot of the same field with two more rows.
        let (snap2, field2) = snapshot_with(
            vec![0, 1_000_000, 2_000_000, 3_000_000],
            vec![Some(100), Some(200), Some(50), Some(400)],
            0,
        );
        assert_eq!(field, field2);

        assert!(cache.append(&snap2, field));
        assert_eq!(cache.samples(), 4);
        assert_eq!(cache.built_rows, 4);
        assert_eq!(cache.xy[4], 2.0); // x of 3rd sample = 2 s
        assert_eq!(cache.xy[5], 0.5); // y = 50 * 0.01

        // No-op when nothing new.
        assert!(!cache.append(&snap2, field));

        let q = cache.pyramid.query(&cache.xy, 0, 4);
        assert_eq!(q.min, 0.5);
        assert_eq!(q.max, 4.0);
    }
}
