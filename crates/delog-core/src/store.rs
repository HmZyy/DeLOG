//! Immutable topic store spine.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::chunk::Chunk;
use crate::schema::TopicSchema;
use crate::time::TimeRange;

/// Append-only topic table: schema plus an immutable spine of sealed chunks.
#[derive(Debug, Clone)]
pub struct TopicStore {
    pub schema: Arc<TopicSchema>,
    pub chunks: Arc<[Arc<Chunk>]>,
    pub rows: u64,
    /// Union of every chunk's `[t_min, t_max]`, maintained as a cached aggregate
    /// like `rows`: O(chunks) once in `from_chunks`, O(1) per `append_chunk`.
    /// `time_range()` reads it in O(1) — it used to rescan all chunks on every
    /// call, and the UI calls it (via `global_time_range`/the browser tree)
    /// several times per frame, so the scan grew with the live chunk count.
    time_range: Option<TimeRange>,
    /// Whether the chunk spine is sorted and non-overlapping in time
    /// (`chunks[i].t_max <= chunks[i+1].t_min`). True for the common in-order
    /// (live/file) case; an out-of-order source clears it (overlap is
    /// tolerated). `FieldView::sample_at` binary-searches the spine when this
    /// holds and falls back to a linear scan otherwise. Maintained O(1) per
    /// `append_chunk` like `rows`/`time_range`.
    monotonic: bool,
}

/// Topic store construction/append failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicStoreError {
    ChunkSchemaMismatch { expected: usize, actual: usize },
    RowCountOverflow,
}

impl TopicStore {
    pub fn new(schema: Arc<TopicSchema>) -> Self {
        Self {
            schema,
            chunks: Arc::from([]),
            rows: 0,
            time_range: None,
            monotonic: true,
        }
    }

    pub fn from_chunks(
        schema: Arc<TopicSchema>,
        chunks: impl IntoIterator<Item = Arc<Chunk>>,
    ) -> Result<Self, TopicStoreError> {
        let mut rows = 0_u64;
        let mut time_range: Option<TimeRange> = None;
        let mut monotonic = true;
        let mut prev_t_max: Option<i64> = None;
        let chunks: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                validate_chunk(&schema, &chunk)?;
                rows = rows
                    .checked_add(chunk.len() as u64)
                    .ok_or(TopicStoreError::RowCountOverflow)?;
                if prev_t_max.is_some_and(|pmax| chunk.t_min < pmax) {
                    monotonic = false;
                }
                prev_t_max = Some(chunk.t_max);
                time_range = Some(union_with_chunk(time_range, &chunk));
                Ok(chunk)
            })
            .collect::<Result<_, _>>()?;

        Ok(Self {
            schema,
            chunks: Arc::from(chunks),
            rows,
            time_range,
            monotonic,
        })
    }

    /// Return a new store with a rebuilt spine that structurally shares every
    /// existing sealed chunk.
    pub fn append_chunk(&self, chunk: Arc<Chunk>) -> Result<Self, TopicStoreError> {
        validate_chunk(&self.schema, &chunk)?;

        let rows = self
            .rows
            .checked_add(chunk.len() as u64)
            .ok_or(TopicStoreError::RowCountOverflow)?;
        let time_range = Some(union_with_chunk(self.time_range, &chunk));
        let monotonic = self.monotonic
            && self
                .chunks
                .last()
                .is_none_or(|last| last.t_max <= chunk.t_min);
        let mut chunks = Vec::with_capacity(self.chunks.len() + 1);
        chunks.extend(self.chunks.iter().cloned());
        chunks.push(chunk);

        Ok(Self {
            schema: Arc::clone(&self.schema),
            chunks: Arc::from(chunks),
            rows,
            time_range,
            monotonic,
        })
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// Union of every chunk's time range. O(1): returns the cached aggregate
    /// maintained by `from_chunks`/`append_chunk`.
    pub fn time_range(&self) -> Option<TimeRange> {
        self.time_range
    }

    /// Whether the chunk spine is sorted and non-overlapping in time, so a
    /// time query can binary-search the chunks instead of scanning them all.
    pub fn is_monotonic(&self) -> bool {
        self.monotonic
    }
}

/// Fold one chunk's `[t_min, t_max]` into a running union. Mirrors the
/// `expect` the previous per-call scan used — a sealed chunk always has a
/// valid range.
fn union_with_chunk(acc: Option<TimeRange>, chunk: &Chunk) -> TimeRange {
    let chunk_range = TimeRange::new(chunk.t_min, chunk.t_max).expect("chunk range is valid");
    match acc {
        Some(range) => range.union(chunk_range),
        None => chunk_range,
    }
}

impl fmt::Display for TopicStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChunkSchemaMismatch { expected, actual } => write!(
                f,
                "chunk schema mismatch: expected {expected} columns, got {actual}"
            ),
            Self::RowCountOverflow => write!(f, "topic row count overflow"),
        }
    }
}

impl Error for TopicStoreError {}

fn validate_chunk(schema: &TopicSchema, chunk: &Chunk) -> Result<(), TopicStoreError> {
    if chunk.cols.len() != schema.len() {
        return Err(TopicStoreError::ChunkSchemaMismatch {
            expected: schema.len(),
            actual: chunk.cols.len(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;

    use super::*;
    use crate::chunk::Chunk;
    use crate::schema::FieldSchema;

    fn schema() -> Arc<TopicSchema> {
        Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        )
    }

    fn chunk(times: Vec<i64>, values: Vec<f64>) -> Arc<Chunk> {
        let cols: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(values))];
        Arc::new(Chunk::try_new(Int64Array::from(times), cols, &schema()).unwrap())
    }

    #[test]
    fn new_store_is_empty() {
        let store = TopicStore::new(schema());
        assert_eq!(store.rows, 0);
        assert_eq!(store.chunk_count(), 0);
        assert!(store.is_empty());
        assert_eq!(store.time_range(), None);
        assert!(store.is_monotonic());
    }

    #[test]
    fn append_returns_new_store_and_keeps_old_spine_unchanged() {
        let first = chunk(vec![100, 200], vec![1.0, 2.0]);
        let second = chunk(vec![50, 300], vec![3.0, 4.0]);
        let store = TopicStore::new(schema());

        let one = store.append_chunk(Arc::clone(&first)).unwrap();
        let two = one.append_chunk(Arc::clone(&second)).unwrap();

        assert_eq!(store.rows, 0);
        assert_eq!(one.rows, 2);
        assert_eq!(two.rows, 4);
        assert_eq!(one.chunk_count(), 1);
        assert_eq!(two.chunk_count(), 2);
        assert!(Arc::ptr_eq(&one.chunks[0], &two.chunks[0]));
        assert!(Arc::ptr_eq(&first, &two.chunks[0]));
        assert!(Arc::ptr_eq(&second, &two.chunks[1]));
        assert_eq!(two.time_range(), TimeRange::new(50, 300));
        // `second` starts (50) before `first` ends (200): the spine overlaps,
        // so the appended store is no longer monotonic.
        assert!(one.is_monotonic());
        assert!(!two.is_monotonic());
    }

    #[test]
    fn monotonic_flag_tracks_chunk_ordering() {
        // In-order, non-overlapping chunks stay monotonic.
        let store = TopicStore::from_chunks(
            schema(),
            [
                chunk(vec![0, 1, 2], vec![1.0, 2.0, 3.0]),
                chunk(vec![10], vec![4.0]),
            ],
        )
        .unwrap();
        assert!(store.is_monotonic());
        // Appending a still-later chunk keeps it monotonic; an out-of-order one clears it.
        assert!(
            store
                .append_chunk(chunk(vec![20], vec![5.0]))
                .unwrap()
                .is_monotonic()
        );
        assert!(
            !store
                .append_chunk(chunk(vec![5], vec![6.0]))
                .unwrap()
                .is_monotonic()
        );

        // Overlapping inputs to from_chunks are detected too.
        let overlapping = TopicStore::from_chunks(
            schema(),
            [
                chunk(vec![0, 100], vec![1.0, 2.0]),
                chunk(vec![50], vec![3.0]),
            ],
        )
        .unwrap();
        assert!(!overlapping.is_monotonic());
    }

    #[test]
    fn from_chunks_computes_rows_and_shares_inputs() {
        let first = chunk(vec![0, 1, 2], vec![1.0, 2.0, 3.0]);
        let second = chunk(vec![10], vec![4.0]);

        let store =
            TopicStore::from_chunks(schema(), [Arc::clone(&first), Arc::clone(&second)]).unwrap();

        assert_eq!(store.rows, 4);
        assert!(Arc::ptr_eq(&store.chunks[0], &first));
        assert!(Arc::ptr_eq(&store.chunks[1], &second));
    }

    #[test]
    fn rejects_chunks_with_wrong_column_count() {
        let bad_schema = Arc::new(TopicSchema::new("EMPTY", []).unwrap());
        let good_chunk = chunk(vec![0], vec![1.0]);

        let err = TopicStore::new(bad_schema)
            .append_chunk(good_chunk)
            .unwrap_err();
        assert_eq!(
            err,
            TopicStoreError::ChunkSchemaMismatch {
                expected: 0,
                actual: 1,
            }
        );
    }
}
