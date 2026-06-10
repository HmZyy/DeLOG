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

use crate::pyramid::MinMaxPyramid;

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
        })
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

    /// CPU bytes held (xy buffer + pyramid), for `MemBreakdown` (CCH-10).
    pub fn bytes(&self) -> u64 {
        (self.xy.capacity() * std::mem::size_of::<f32>()) as u64 + self.pyramid.bytes()
    }

    pub fn touch(&mut self, frame: u64) {
        self.last_used_frame = frame;
    }
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
