//! Memory accounting per field/topic/source.
//!
//! [`MemBreakdown`] is the shared vocabulary type for the four memory pools the
//! tool tracks. `delog-core` can only measure the `canonical` pool — the Arrow
//! buffers held by the store — because the others live in crates it must not
//! depend on: `cache_cpu` is `delog-cache`'s f32 render caches,
//! `gpu` is `delog-render`'s buffer-manager ledger, and `mmap` is the
//! memory-mapped IPC sidecars. Upper layers fill those pools in and
//! merge with the canonical report built here.

use std::iter::Sum;
use std::ops::{Add, AddAssign};

use arrow::array::Array;

use crate::identity::{FieldId, SourceId, TopicId};
use crate::snapshot::StoreSnapshot;

/// Bytes attributed to one entity (field, topic, source, or the whole store),
/// split across the four pools.
///
/// All arithmetic saturates: byte totals never realistically overflow `u64`,
/// but saturating keeps accounting panic-free even on absurd inputs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemBreakdown {
    /// Canonical Arrow buffers in the store (timestamps + columns).
    pub canonical: u64,
    /// Lazily-built f32 render caches + min/max pyramids (`delog-cache`).
    pub cache_cpu: u64,
    /// GPU storage buffers tracked by the renderer ledger (`delog-render`).
    pub gpu: u64,
    /// Memory-mapped Arrow IPC sidecar pages (`delog-cache` reload).
    pub mmap: u64,
}

impl MemBreakdown {
    /// All pools zero.
    pub const ZERO: Self = Self {
        canonical: 0,
        cache_cpu: 0,
        gpu: 0,
        mmap: 0,
    };

    /// A breakdown carrying only canonical bytes — the only pool `delog-core`
    /// can measure.
    pub const fn canonical(bytes: u64) -> Self {
        Self {
            canonical: bytes,
            cache_cpu: 0,
            gpu: 0,
            mmap: 0,
        }
    }

    /// Sum across all four pools.
    pub const fn total(&self) -> u64 {
        self.canonical
            .saturating_add(self.cache_cpu)
            .saturating_add(self.gpu)
            .saturating_add(self.mmap)
    }
}

impl Add for MemBreakdown {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            canonical: self.canonical.saturating_add(rhs.canonical),
            cache_cpu: self.cache_cpu.saturating_add(rhs.cache_cpu),
            gpu: self.gpu.saturating_add(rhs.gpu),
            mmap: self.mmap.saturating_add(rhs.mmap),
        }
    }
}

impl AddAssign for MemBreakdown {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sum for MemBreakdown {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Add::add)
    }
}

/// Canonical memory of a snapshot, broken down per field/topic/source plus a
/// grand total. Built by [`MemReport::canonical`].
///
/// The per-entity vectors are indexed by the dense runtime ID (`id.index()`),
/// aligned to the snapshot's own `fields`/`topics`/`sources` vectors. A topic's
/// canonical bytes include its shared timestamp buffers; those are *not*
/// attributed to any single field, so for a topic whose every column has a
/// registered field, `topic.canonical == timestamp_bytes + Σ field.canonical`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemReport {
    fields: Vec<MemBreakdown>,
    topics: Vec<MemBreakdown>,
    sources: Vec<MemBreakdown>,
    total: MemBreakdown,
}

impl MemReport {
    /// Measure the canonical (Arrow buffer) memory of every field, topic and
    /// source in `snapshot`. The other three pools are left zero — only
    /// `delog-core`'s store is in scope here.
    pub fn canonical(snapshot: &StoreSnapshot) -> Self {
        let mut fields = vec![MemBreakdown::ZERO; snapshot.fields.len()];
        let mut topics = vec![MemBreakdown::ZERO; snapshot.topics.len()];
        let mut sources = vec![MemBreakdown::ZERO; snapshot.sources.len()];

        // Per-topic: shared timestamp buffers + every column buffer, summed
        // across chunks. Roll the same total up into the owning source.
        for topic in snapshot.topics.iter() {
            let Some(store) = topic.store.as_ref() else {
                continue;
            };

            let mut topic_bytes = 0_u64;
            for chunk in store.chunks.iter() {
                topic_bytes = topic_bytes.saturating_add(buffer_bytes(&chunk.t));
                for col in &chunk.cols {
                    topic_bytes = topic_bytes.saturating_add(buffer_bytes(col.as_ref()));
                }
            }

            let breakdown = MemBreakdown::canonical(topic_bytes);
            if let Some(slot) = topics.get_mut(topic.entry.id.index()) {
                *slot = breakdown;
            }
            if let Some(slot) = sources.get_mut(topic.entry.source.index()) {
                *slot += breakdown;
            }
        }

        // Per-field: only that field's own column buffer (timestamps excluded).
        for field in snapshot.fields.iter() {
            let Some(topic) = snapshot.topic(field.topic) else {
                continue;
            };
            let Some(store) = topic.store.as_ref() else {
                continue;
            };
            let Some(col_index) = store.schema.field_index(&field.name) else {
                continue;
            };

            let mut field_bytes = 0_u64;
            for chunk in store.chunks.iter() {
                if let Some(col) = chunk.cols.get(col_index) {
                    field_bytes = field_bytes.saturating_add(buffer_bytes(col.as_ref()));
                }
            }
            if let Some(slot) = fields.get_mut(field.id.index()) {
                *slot = MemBreakdown::canonical(field_bytes);
            }
        }

        let total = sources.iter().copied().sum();

        Self {
            fields,
            topics,
            sources,
            total,
        }
    }

    pub fn field(&self, id: FieldId) -> MemBreakdown {
        self.fields.get(id.index()).copied().unwrap_or_default()
    }

    pub fn topic(&self, id: TopicId) -> MemBreakdown {
        self.topics.get(id.index()).copied().unwrap_or_default()
    }

    pub fn source(&self, id: SourceId) -> MemBreakdown {
        self.sources.get(id.index()).copied().unwrap_or_default()
    }

    pub fn total(&self) -> MemBreakdown {
        self.total
    }

    pub fn fields(&self) -> &[MemBreakdown] {
        &self.fields
    }

    pub fn topics(&self) -> &[MemBreakdown] {
        &self.topics
    }

    pub fn sources(&self) -> &[MemBreakdown] {
        &self.sources
    }
}

fn buffer_bytes(array: &dyn Array) -> u64 {
    array.get_buffer_memory_size() as u64
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int32Array, Int64Array};
    use arrow::datatypes::DataType;

    use super::*;
    use crate::chunk::Chunk;
    use crate::identity::{FieldId, IdentityRegistry, SourceId, TopicId};
    use crate::schema::{FieldSchema, TopicSchema};
    use crate::store::TopicStore;

    #[test]
    fn breakdown_arithmetic_saturates_and_totals() {
        let a = MemBreakdown {
            canonical: 10,
            cache_cpu: 20,
            gpu: 30,
            mmap: 40,
        };
        assert_eq!(a.total(), 100);
        assert_eq!(
            MemBreakdown::canonical(7),
            MemBreakdown {
                canonical: 7,
                ..MemBreakdown::ZERO
            }
        );

        let sum: MemBreakdown = [a, MemBreakdown::canonical(5)].into_iter().sum();
        assert_eq!(sum.canonical, 15);
        assert_eq!(sum.total(), 105);

        let big = MemBreakdown::canonical(u64::MAX);
        assert_eq!((big + MemBreakdown::canonical(1)).canonical, u64::MAX);
    }

    // A two-source snapshot: one source with a BARO topic (two numeric fields),
    // another with a GPS topic (one field). Lets us check per-field/topic/source
    // attribution and the timestamp-folding invariant.
    fn snapshot() -> (
        StoreSnapshot,
        SourceId,
        SourceId,
        TopicId,
        TopicId,
        [FieldId; 3],
    ) {
        let mut identity = IdentityRegistry::new();
        let flight = identity.add_source("flight");
        let live = identity.add_source("live");

        let baro = identity.add_topic(flight, "BARO").unwrap();
        let alt = identity.add_field(baro, "Alt").unwrap();
        let temp = identity.add_field(baro, "Temp").unwrap();

        let gps = identity.add_topic(live, "GPS").unwrap();
        let lat = identity.add_field(gps, "Lat").unwrap();

        let baro_store = baro_store();
        let gps_store = gps_store();

        let snapshot = StoreSnapshot::from_registry(
            &identity,
            [
                (baro, Arc::clone(&baro_store)),
                (gps, Arc::clone(&gps_store)),
            ],
            0,
        )
        .unwrap();

        (snapshot, flight, live, baro, gps, [alt, temp, lat])
    }

    fn baro_store() -> Arc<TopicStore> {
        let schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [
                    FieldSchema::new("Alt", DataType::Int32, Some("cm"), 0.01).unwrap(),
                    FieldSchema::new("Temp", DataType::Float64, Some("C"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Int32Array::from(vec![1, 2, 3, 4])),
            Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0, 4.0])),
        ];
        let chunk =
            Arc::new(Chunk::try_new(Int64Array::from(vec![0, 1, 2, 3]), cols, &schema).unwrap());
        Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap())
    }

    fn gps_store() -> Arc<TopicStore> {
        let schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [FieldSchema::new("Lat", DataType::Float64, Some("deg"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(vec![10.0, 11.0]))];
        let chunk = Arc::new(Chunk::try_new(Int64Array::from(vec![0, 1]), cols, &schema).unwrap());
        Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap())
    }

    #[test]
    fn report_attributes_canonical_bytes_per_entity() {
        let (snapshot, flight, live, baro, gps, [alt, temp, lat]) = snapshot();
        let report = MemReport::canonical(&snapshot);

        // Every measured pool is canonical-only here.
        for slot in report
            .fields()
            .iter()
            .chain(report.topics())
            .chain(report.sources())
        {
            assert_eq!(slot.cache_cpu, 0);
            assert_eq!(slot.gpu, 0);
            assert_eq!(slot.mmap, 0);
        }

        // Fields carry their own column buffers, no timestamps.
        assert!(report.field(alt).canonical > 0);
        assert!(report.field(temp).canonical > 0);
        assert!(report.field(lat).canonical > 0);

        // BARO timestamps are shared, so the topic exceeds the field columns.
        let baro_ts = report.topic(baro).canonical
            - report.field(alt).canonical
            - report.field(temp).canonical;
        assert!(baro_ts > 0, "topic must include shared timestamp buffers");

        // Source folds its topic; total folds both sources.
        assert_eq!(report.source(flight), report.topic(baro));
        assert_eq!(report.source(live), report.topic(gps));
        assert_eq!(report.total(), report.source(flight) + report.source(live));
        assert_eq!(report.total().total(), report.total().canonical);
    }

    #[test]
    fn topic_equals_timestamp_plus_field_columns() {
        let (snapshot, _flight, _live, baro, _gps, [alt, temp, _lat]) = snapshot();
        let report = MemReport::canonical(&snapshot);

        // Recompute the lone timestamp buffer to pin the folding invariant.
        let baro_store = snapshot.topic_store(baro).unwrap();
        let ts_bytes: u64 = baro_store
            .chunks
            .iter()
            .map(|c| c.t.get_buffer_memory_size() as u64)
            .sum();

        assert_eq!(
            report.topic(baro).canonical,
            ts_bytes + report.field(alt).canonical + report.field(temp).canonical
        );
    }

    #[test]
    fn empty_and_unknown_ids_report_zero() {
        let report = MemReport::canonical(&StoreSnapshot::empty());
        assert_eq!(report.total(), MemBreakdown::ZERO);
        assert_eq!(report.field(FieldId(99)), MemBreakdown::ZERO);
        assert_eq!(report.topic(TopicId(99)), MemBreakdown::ZERO);
        assert_eq!(report.source(SourceId(99)), MemBreakdown::ZERO);
    }
}
