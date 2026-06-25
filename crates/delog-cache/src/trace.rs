//! Per-trace f32 render cache: one interleaved `[x0,y0,…]` buffer per field.
//! NaN stays NaN so the shader breaks the segment (gaps render as gaps).

use arrow::array::{
    Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::DataType;

use delog_core::identity::FieldId;
use delog_core::metrics::MetricsRegistry;
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;

use crate::pyramid::{BRANCH, MinMax, MinMaxPyramid};

#[derive(Debug)]
pub struct TraceCache {
    /// Interleaved `[x0,y0,x1,y1,…]`.
    pub xy: Vec<f32>,
    /// Time rebase origin: `x = (t_eff - origin_us) * 1e-6` as f32.
    pub origin_us: i64,
    pub built_rows: u64,
    pub pyramid: MinMaxPyramid,
    pub last_used_frame: u64,
    /// Source offset baked into the x values; a different offset makes this
    /// cache stale (rebuild, not append).
    pub offset_us: i64,
}

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
    pub fn build(
        snapshot: &StoreSnapshot,
        field: FieldId,
        origin_us: i64,
        frame: u64,
        metrics: &MetricsRegistry,
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
        let pyramid = {
            let _t = metrics.scope("minmax_build");
            MinMaxPyramid::build_strided(&xy, 2, 1)
        };
        Some(Self {
            xy,
            origin_us,
            built_rows: r.store.rows,
            pyramid,
            last_used_frame: frame,
            offset_us: r.offset_us,
        })
    }

    pub fn offset_changed(&self, snapshot: &StoreSnapshot, field: FieldId) -> bool {
        resolve(snapshot, field).is_some_and(|r| r.offset_us != self.offset_us)
    }

    /// Append rows that arrived since the last build/append; returns whether
    /// any were added. A changed global origin is a rebuild, not an append.
    pub fn append(
        &mut self,
        snapshot: &StoreSnapshot,
        field: FieldId,
        metrics: &MetricsRegistry,
    ) -> bool {
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

        {
            let _t = metrics.scope("minmax_build");
            self.pyramid.extend(&self.xy);
        }
        self.built_rows = r.store.rows;
        true
    }

    pub fn samples(&self) -> usize {
        self.xy.len() / 2
    }

    pub fn is_empty(&self) -> bool {
        self.xy.is_empty()
    }

    fn x_at(&self, i: usize) -> f32 {
        self.xy[2 * i]
    }

    /// Sample index range `[a, b)` whose x falls in `[x0, x1]` (seconds).
    pub fn index_range(&self, x0: f32, x1: f32) -> (usize, usize) {
        let n = self.samples();
        let a = self.partition_point(0, n, |x| x < x0);
        let b = self.partition_point(a, n, |x| x <= x1);
        (a, b)
    }

    /// First index in `[lo, hi)` whose x fails `pred`; relies on the x channel
    /// being sorted so `pred` is monotone-true-then-false.
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

    /// Min/max y over the visible x window `[x0, x1]` (seconds). One sample of
    /// context is included each side so a line entering/leaving is bounded.
    pub fn y_range(&self, x0: f32, x1: f32) -> MinMax {
        let (a, b) = self.index_range(x0, x1);
        let a = a.saturating_sub(1);
        let b = (b + 1).min(self.samples());
        self.pyramid.query(&self.xy, a, b)
    }

    /// Per-column `[x, min, max]` triples over `[x0, x1)` split into `width`
    /// equal half-open time columns; an empty column reports `NaN` (skipped by
    /// the shader). Splitting by time, not index, keeps columns aligned to
    /// screen pixels even for irregularly-sampled data.
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

    /// Distribute each L0 node overlapping `[a, b)` to the column(s) its x range
    /// covers. Conservative at boundaries: never hides a transient, may smear it
    /// one column.
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

    pub fn bytes(&self) -> u64 {
        (self.xy.capacity() * std::mem::size_of::<f32>()) as u64 + self.pyramid.bytes()
    }

    pub fn touch(&mut self, frame: u64) {
        self.last_used_frame = frame;
    }
}

/// Stretch each finite column's span to meet its right neighbour's so the
/// shader's disjoint per-column bars touch (else a sloped signal reads dashed).
/// Only ever grows a span (no transient hidden); `NaN` columns stay gaps.
fn bridge_columns(mins: &mut [f32], maxs: &mut [f32]) {
    for c in 0..mins.len().saturating_sub(1) {
        let (cur_min, cur_max) = (mins[c], maxs[c]);
        let (nxt_min, nxt_max) = (mins[c + 1], maxs[c + 1]);
        if cur_min.is_nan() || nxt_min.is_nan() {
            continue;
        }
        if cur_max < nxt_min {
            maxs[c] = nxt_min;
        } else if nxt_max < cur_min {
            mins[c] = nxt_max;
        }
    }
}

fn col_index(x: f32, x0: f32, inv: f32, width: usize) -> usize {
    let c = ((x - x0) * inv * width as f32) as i64;
    c.clamp(0, width as i64 - 1) as usize
}

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
        let (snap, field) = snapshot_with(
            vec![1_000_000, 2_000_000, 3_000_000],
            vec![Some(100), Some(200), Some(300)],
            5_000_000,
        );
        let origin = 6_000_000;
        let cache = TraceCache::build(&snap, field, origin, 0, &MetricsRegistry::new()).unwrap();

        assert_eq!(cache.samples(), 3);
        assert_eq!(cache.built_rows, 3);
        assert_eq!(cache.xy[0], 0.0);
        assert_eq!(cache.xy[2], 1.0);
        assert_eq!(cache.xy[4], 2.0);
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
        let cache = TraceCache::build(&snap, field, 0, 0, &MetricsRegistry::new()).unwrap();
        assert_eq!(cache.xy[1], 1.0);
        assert!(cache.xy[3].is_nan());
        assert_eq!(cache.xy[5], 3.0);
        let q = cache.pyramid.query(&cache.xy, 0, 3);
        assert_eq!(q.min, 1.0);
        assert_eq!(q.max, 3.0);
    }

    #[test]
    fn index_range_and_y_range_target_the_visible_window() {
        let (snap, field) = snapshot_with(
            vec![0, 1_000_000, 2_000_000, 3_000_000, 4_000_000],
            vec![Some(0), Some(100), Some(200), Some(300), Some(400)],
            0,
        );
        let cache = TraceCache::build(&snap, field, 0, 0, &MetricsRegistry::new()).unwrap();

        let (a, b) = cache.index_range(1.0, 3.0);
        assert_eq!((a, b), (1, 4));

        let mm = cache.y_range(1.0, 3.0);
        assert_eq!(mm.min, 0.0);
        assert_eq!(mm.max, 4.0);

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
        let cache = TraceCache::build(&snap, field, 0, 0, &MetricsRegistry::new()).unwrap();

        let cols = cache.minmax_columns(0.0, 4.0, 4, true);
        assert_eq!(cols.len(), 4 * 3);
        assert_eq!(cols[0], 0.5);
        assert_eq!(cols[1], 0.0);
        assert_eq!(cols[2], 1.0);
        assert_eq!(cols[4], 1.0);
        assert_eq!(cols[11], 4.0);
        assert!(cols[5] > cols[2]);
        for c in 0..3 {
            assert_eq!(cols[c * 3 + 2], cols[(c + 1) * 3 + 1], "column {c} bridges");
        }
    }

    #[test]
    fn l0_walk_path_preserves_min_max_and_transients() {
        // 1000 samples / width 4 = ~250 per column (≥ BRANCH), so the L0 path runs.
        let times: Vec<i64> = (0..1000).collect();
        let mut alts: Vec<Option<i32>> = (0..1000).map(|i| Some(i * 100)).collect();
        alts[300] = Some(999_900);
        let (snap, field) = snapshot_with(times, alts, 0);
        let cache = TraceCache::build(&snap, field, 0, 0, &MetricsRegistry::new()).unwrap();

        let x1 = 999.0 * 1e-6;
        let cols = cache.minmax_columns(0.0, x1, 4, true);
        assert_eq!(cols.len(), 12);
        assert_eq!(cols[1], 0.0);
        assert!(cols[11] >= 990.0);
        assert!(
            (cols[5] - 9999.0).abs() < 1.0,
            "spike preserved, got {}",
            cols[5]
        );
    }

    #[test]
    fn append_adds_only_new_rows_and_extends_the_pyramid() {
        let (snap1, field) = snapshot_with(vec![0, 1_000_000], vec![Some(100), Some(200)], 0);
        let mut cache = TraceCache::build(&snap1, field, 0, 0, &MetricsRegistry::new()).unwrap();
        assert_eq!(cache.samples(), 2);

        let (snap2, field2) = snapshot_with(
            vec![0, 1_000_000, 2_000_000, 3_000_000],
            vec![Some(100), Some(200), Some(50), Some(400)],
            0,
        );
        assert_eq!(field, field2);

        assert!(cache.append(&snap2, field, &MetricsRegistry::new()));
        assert_eq!(cache.samples(), 4);
        assert_eq!(cache.built_rows, 4);
        assert_eq!(cache.xy[4], 2.0);
        assert_eq!(cache.xy[5], 0.5);

        assert!(!cache.append(&snap2, field, &MetricsRegistry::new()));

        let q = cache.pyramid.query(&cache.xy, 0, 4);
        assert_eq!(q.min, 0.5);
        assert_eq!(q.max, 4.0);
    }
}
