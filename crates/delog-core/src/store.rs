//! Immutable topic store spine (PLAN.md §4.3-§4.4).

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
        }
    }

    pub fn from_chunks(
        schema: Arc<TopicSchema>,
        chunks: impl IntoIterator<Item = Arc<Chunk>>,
    ) -> Result<Self, TopicStoreError> {
        let mut rows = 0_u64;
        let chunks: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                validate_chunk(&schema, &chunk)?;
                rows = rows
                    .checked_add(chunk.len() as u64)
                    .ok_or(TopicStoreError::RowCountOverflow)?;
                Ok(chunk)
            })
            .collect::<Result<_, _>>()?;

        Ok(Self {
            schema,
            chunks: Arc::from(chunks),
            rows,
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
        let mut chunks = Vec::with_capacity(self.chunks.len() + 1);
        chunks.extend(self.chunks.iter().cloned());
        chunks.push(chunk);

        Ok(Self {
            schema: Arc::clone(&self.schema),
            chunks: Arc::from(chunks),
            rows,
        })
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn time_range(&self) -> Option<TimeRange> {
        self.chunks
            .iter()
            .map(|chunk| TimeRange::new(chunk.t_min, chunk.t_max).expect("chunk range is valid"))
            .reduce(TimeRange::union)
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
