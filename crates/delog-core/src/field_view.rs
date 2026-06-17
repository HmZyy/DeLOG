//! Zero-copy field accessors over immutable snapshots (PLAN.md §4.5, CORE-07).

use std::error::Error;
use std::fmt;

use arrow::array::{
    Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    LargeStringArray, StringArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::DataType;

use crate::chunk::Chunk;
use crate::identity::{FieldId, TopicId};
use crate::snapshot::StoreSnapshot;
use crate::store::TopicStore;
use crate::time::{TimeRange, TimestampUs, effective_time_us, raw_time_us};

/// How [`FieldView::sample_at`] chooses a value around the query time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleMode {
    Prev,
    Next,
    Linear,
}

/// Borrowed sample value. Strings are borrowed directly from Arrow buffers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SampleValue<'a> {
    Int(i64),
    UInt(u64),
    Float(f64),
    Bool(bool),
    Utf8(&'a str),
    Null,
}

/// One sampled field value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample<'a> {
    pub raw_time_us: TimestampUs,
    pub effective_time_us: TimestampUs,
    pub value: SampleValue<'a>,
}

/// Borrowed view of one field in one snapshot.
pub struct FieldView<'a> {
    snapshot: &'a StoreSnapshot,
    field: FieldId,
    topic: TopicId,
    store: &'a TopicStore,
    col_index: usize,
    source_offset_us: TimestampUs,
}

/// Field view construction failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldViewError {
    InvalidFieldId(FieldId),
    MissingTopic(TopicId),
    MissingSource,
    MissingTopicStore(TopicId),
    FieldMissingFromSchema { topic: TopicId, field: String },
}

impl<'a> FieldView<'a> {
    pub fn new(snapshot: &'a StoreSnapshot, field: FieldId) -> Result<Self, FieldViewError> {
        let field_entry = snapshot
            .fields
            .get(field.index())
            .filter(|entry| entry.id == field)
            .ok_or(FieldViewError::InvalidFieldId(field))?;
        let topic = snapshot
            .topic(field_entry.topic)
            .ok_or(FieldViewError::MissingTopic(field_entry.topic))?;
        let source = snapshot
            .source(topic.entry.source)
            .ok_or(FieldViewError::MissingSource)?;
        let store = topic
            .store
            .as_deref()
            .ok_or(FieldViewError::MissingTopicStore(topic.entry.id))?;
        let col_index = store.schema.field_index(&field_entry.name).ok_or_else(|| {
            FieldViewError::FieldMissingFromSchema {
                topic: topic.entry.id,
                field: field_entry.name.clone(),
            }
        })?;

        Ok(Self {
            snapshot,
            field,
            topic: topic.entry.id,
            store,
            col_index,
            source_offset_us: source.entry.offset_us,
        })
    }

    pub fn field(&self) -> FieldId {
        self.field
    }

    /// Column index of this field inside its topic schema.
    pub fn col_index(&self) -> usize {
        self.col_index
    }

    pub fn topic(&self) -> TopicId {
        self.topic
    }

    pub fn dtype(&self) -> &DataType {
        &self.store.schema.fields()[self.col_index].dtype
    }

    /// Chunks whose raw time range overlaps the requested effective range.
    pub fn chunks_overlapping(
        &'a self,
        effective_range: TimeRange,
    ) -> impl Iterator<Item = &'a Chunk> + 'a {
        let raw_range =
            raw_time_us(effective_range.min_us, self.source_offset_us).and_then(|min_us| {
                raw_time_us(effective_range.max_us, self.source_offset_us)
                    .map(|max_us| TimeRange { min_us, max_us })
            });

        self.store.chunks.iter().filter_map(move |chunk| {
            let range = raw_range?;
            ranges_overlap(range, TimeRange::new(chunk.t_min, chunk.t_max)?)
                .then_some(chunk.as_ref())
        })
    }

    /// Owned `(effective_time, string)` for every Utf8 sample whose effective
    /// time falls within `range`, up to `max` entries (PLT-15 text annotations).
    /// Non-string samples are skipped. When `filter` is set, only samples whose
    /// text contains it (case-insensitive) are kept — the `max` cap counts
    /// matches, so filtering reaches matches deep in a large field. Returns owned
    /// strings so callers need no Arrow access and hold no borrow of the snapshot.
    pub fn string_samples_in_range(
        &self,
        range: TimeRange,
        max: usize,
        filter: Option<&str>,
    ) -> Vec<(TimestampUs, String)> {
        let needle = filter
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_lowercase);
        let mut out = Vec::new();
        for chunk in self.store.chunks.iter() {
            // Skip chunks whose effective span can't overlap the range.
            let lo = effective_time_us(chunk.t_min, self.source_offset_us);
            let hi = effective_time_us(chunk.t_max, self.source_offset_us);
            if let (Some(lo), Some(hi)) = (lo, hi)
                && (hi < range.min_us || lo > range.max_us)
            {
                continue;
            }
            let col = chunk.cols[self.col_index].as_ref();
            for row in 0..chunk.len() {
                let Some(eff) = effective_time_us(chunk.t.value(row), self.source_offset_us) else {
                    continue;
                };
                if eff < range.min_us || eff > range.max_us {
                    continue;
                }
                if let SampleValue::Utf8(s) = value_at(col, row) {
                    if let Some(needle) = &needle
                        && !s.to_lowercase().contains(needle.as_str())
                    {
                        continue;
                    }
                    out.push((eff, s.to_string()));
                    if out.len() >= max {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Sample this field at an effective/global timestamp.
    pub fn sample_at(
        &'a self,
        effective_time_us: TimestampUs,
        mode: SampleMode,
    ) -> Option<Sample<'a>> {
        let raw_time = raw_time_us(effective_time_us, self.source_offset_us)?;
        match mode {
            SampleMode::Prev => self.prev_sample(raw_time),
            SampleMode::Next => self.next_sample(raw_time),
            SampleMode::Linear => self.linear_sample(raw_time),
        }
    }

    fn prev_sample(&'a self, raw_time: TimestampUs) -> Option<Sample<'a>> {
        // Fast path: a sorted, non-overlapping spine lets us binary-search to
        // the one chunk that can hold the predecessor — O(log chunks) instead
        // of scanning every chunk (which dominated the 3D pose reads, PRF-11).
        if self.store.is_monotonic() {
            let chunks = &self.store.chunks;
            // Rightmost chunk whose t_min <= raw_time; later chunks start after
            // raw_time, earlier ones end before this chunk's first sample.
            let idx = chunks
                .partition_point(|c| c.t_min <= raw_time)
                .checked_sub(1)?;
            let chunk = &chunks[idx];
            let row = upper_bound(&chunk.t, raw_time).checked_sub(1)?;
            return self.sample_from_chunk(chunk, row);
        }
        let mut best = None;
        for chunk in self.store.chunks.iter() {
            if chunk.t_min > raw_time {
                continue;
            }
            let row = upper_bound(&chunk.t, raw_time).checked_sub(1)?;
            let Some(sample) = self.sample_from_chunk(chunk, row) else {
                continue;
            };
            if best
                .map(|current: Sample<'_>| sample.raw_time_us > current.raw_time_us)
                .unwrap_or(true)
            {
                best = Some(sample);
            }
        }
        best
    }

    fn next_sample(&'a self, raw_time: TimestampUs) -> Option<Sample<'a>> {
        if self.store.is_monotonic() {
            let chunks = &self.store.chunks;
            // Leftmost chunk whose t_max >= raw_time; it holds the successor
            // (earlier chunks end before raw_time).
            let idx = chunks.partition_point(|c| c.t_max < raw_time);
            let chunk = chunks.get(idx)?;
            let row = lower_bound(&chunk.t, raw_time);
            return self.sample_from_chunk(chunk, row);
        }
        let mut best = None;
        for chunk in self.store.chunks.iter() {
            if chunk.t_max < raw_time {
                continue;
            }
            let row = lower_bound(&chunk.t, raw_time);
            if row == chunk.len() {
                continue;
            }
            let Some(sample) = self.sample_from_chunk(chunk, row) else {
                continue;
            };
            if best
                .map(|current: Sample<'_>| sample.raw_time_us < current.raw_time_us)
                .unwrap_or(true)
            {
                best = Some(sample);
            }
        }
        best
    }

    fn linear_sample(&'a self, raw_time: TimestampUs) -> Option<Sample<'a>> {
        let prev = self.prev_sample(raw_time)?;
        if prev.raw_time_us == raw_time {
            return Some(prev);
        }

        let next = self.next_sample(raw_time)?;
        if next.raw_time_us == raw_time {
            return Some(next);
        }
        if prev.raw_time_us >= next.raw_time_us {
            return None;
        }

        let prev_y = prev.value.as_f64()?;
        let next_y = next.value.as_f64()?;
        let alpha =
            (raw_time - prev.raw_time_us) as f64 / (next.raw_time_us - prev.raw_time_us) as f64;
        Some(Sample {
            raw_time_us: raw_time,
            effective_time_us: raw_time.checked_add(self.source_offset_us)?,
            value: SampleValue::Float(prev_y + (next_y - prev_y) * alpha),
        })
    }

    fn sample_from_chunk(&'a self, chunk: &'a Chunk, row: usize) -> Option<Sample<'a>> {
        let raw_time = chunk.t.value(row);
        Some(Sample {
            raw_time_us: raw_time,
            effective_time_us: raw_time.checked_add(self.source_offset_us)?,
            value: value_at(chunk.cols[self.col_index].as_ref(), row),
        })
    }
}

impl SampleValue<'_> {
    pub fn as_f64(self) -> Option<f64> {
        match self {
            Self::Int(v) => Some(v as f64),
            Self::UInt(v) => Some(v as f64),
            Self::Float(v) if !v.is_nan() => Some(v),
            Self::Float(_) | Self::Bool(_) | Self::Utf8(_) | Self::Null => None,
        }
    }
}

impl fmt::Debug for FieldView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FieldView")
            .field("epoch", &self.snapshot.epoch)
            .field("field", &self.field)
            .field("topic", &self.topic)
            .field("col_index", &self.col_index)
            .field("source_offset_us", &self.source_offset_us)
            .finish()
    }
}

impl fmt::Display for FieldViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFieldId(id) => write!(f, "invalid field id {id:?}"),
            Self::MissingTopic(id) => write!(f, "missing topic {id:?}"),
            Self::MissingSource => write!(f, "missing source for field topic"),
            Self::MissingTopicStore(id) => write!(f, "missing topic store for {id:?}"),
            Self::FieldMissingFromSchema { topic, field } => {
                write!(f, "field `{field}` is missing from schema for {topic:?}")
            }
        }
    }
}

impl Error for FieldViewError {}

fn ranges_overlap(a: TimeRange, b: TimeRange) -> bool {
    a.min_us <= b.max_us && b.min_us <= a.max_us
}

fn lower_bound(t: &Int64Array, query: TimestampUs) -> usize {
    let mut left = 0;
    let mut right = t.len();
    while left < right {
        let mid = left + (right - left) / 2;
        if t.value(mid) < query {
            left = mid + 1;
        } else {
            right = mid;
        }
    }
    left
}

fn upper_bound(t: &Int64Array, query: TimestampUs) -> usize {
    let mut left = 0;
    let mut right = t.len();
    while left < right {
        let mid = left + (right - left) / 2;
        if t.value(mid) <= query {
            left = mid + 1;
        } else {
            right = mid;
        }
    }
    left
}

/// Read row `row` of `array` as `f64` (NaN for nulls/non-numeric), for script
/// materialization (SCR-03). NaN gap markers in float columns are preserved.
pub fn array_row_as_f64(array: &dyn Array, row: usize) -> f64 {
    match value_at(array, row) {
        SampleValue::Int(v) => v as f64,
        SampleValue::UInt(v) => v as f64,
        SampleValue::Float(v) => v,
        SampleValue::Bool(b) => b as i64 as f64,
        SampleValue::Utf8(_) | SampleValue::Null => f64::NAN,
    }
}

pub(crate) fn value_at(array: &dyn Array, row: usize) -> SampleValue<'_> {
    if array.is_null(row) {
        return SampleValue::Null;
    }

    match array.data_type() {
        DataType::Int8 => SampleValue::Int(i64::from(
            array
                .as_any()
                .downcast_ref::<Int8Array>()
                .unwrap()
                .value(row),
        )),
        DataType::Int16 => SampleValue::Int(i64::from(
            array
                .as_any()
                .downcast_ref::<Int16Array>()
                .unwrap()
                .value(row),
        )),
        DataType::Int32 => SampleValue::Int(i64::from(
            array
                .as_any()
                .downcast_ref::<Int32Array>()
                .unwrap()
                .value(row),
        )),
        DataType::Int64 => SampleValue::Int(
            array
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .value(row),
        ),
        DataType::UInt8 => SampleValue::UInt(u64::from(
            array
                .as_any()
                .downcast_ref::<UInt8Array>()
                .unwrap()
                .value(row),
        )),
        DataType::UInt16 => SampleValue::UInt(u64::from(
            array
                .as_any()
                .downcast_ref::<UInt16Array>()
                .unwrap()
                .value(row),
        )),
        DataType::UInt32 => SampleValue::UInt(u64::from(
            array
                .as_any()
                .downcast_ref::<UInt32Array>()
                .unwrap()
                .value(row),
        )),
        DataType::UInt64 => SampleValue::UInt(
            array
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .value(row),
        ),
        DataType::Float32 => SampleValue::Float(f64::from(
            array
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap()
                .value(row),
        )),
        DataType::Float64 => SampleValue::Float(
            array
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .value(row),
        ),
        DataType::Boolean => SampleValue::Bool(
            array
                .as_any()
                .downcast_ref::<BooleanArray>()
                .unwrap()
                .value(row),
        ),
        DataType::Utf8 => SampleValue::Utf8(
            array
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(row),
        ),
        DataType::LargeUtf8 => SampleValue::Utf8(
            array
                .as_any()
                .downcast_ref::<LargeStringArray>()
                .unwrap()
                .value(row),
        ),
        dtype => unreachable!("schema validation rejects unsupported dtype {dtype:?}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};

    use super::*;
    use crate::chunk::Chunk;
    use crate::identity::{FieldId, IdentityRegistry};
    use crate::schema::{FieldSchema, TopicSchema};
    use crate::snapshot::StoreSnapshot;
    use crate::store::TopicStore;

    struct Fixture {
        snapshot: StoreSnapshot,
        alt: FieldId,
        mode: FieldId,
    }

    fn fixture(offset_us: i64) -> Fixture {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        assert_eq!(identity.set_source_offset_us(source, offset_us), Some(0));
        let topic = identity.add_topic(source, "BARO").unwrap();
        let alt = identity.add_field(topic, "Alt").unwrap();
        let mode = identity.add_field(topic, "Mode").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [
                    FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap(),
                    FieldSchema::new("Mode", DataType::Utf8, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let first_cols: Vec<ArrayRef> = vec![
            Arc::new(Float64Array::from(vec![0.0, 10.0, 20.0])),
            Arc::new(StringArray::from(vec!["idle", "climb", "cruise"])),
        ];
        let second_cols: Vec<ArrayRef> = vec![
            Arc::new(Float64Array::from(vec![30.0, 40.0])),
            Arc::new(StringArray::from(vec!["descend", "land"])),
        ];
        let first = Arc::new(
            Chunk::try_new(Int64Array::from(vec![0, 100, 200]), first_cols, &schema).unwrap(),
        );
        let second = Arc::new(
            Chunk::try_new(Int64Array::from(vec![300, 400]), second_cols, &schema).unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [first, second]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();

        Fixture {
            snapshot,
            alt,
            mode,
        }
    }

    #[test]
    fn string_samples_filter_is_case_insensitive_contains() {
        let fx = fixture(0);
        let mode = FieldView::new(&fx.snapshot, fx.mode).unwrap();
        let range = TimeRange::new(0, 1_000).unwrap();

        let all = mode.string_samples_in_range(range, 100, None);
        assert_eq!(all.len(), 5); // idle, climb, cruise, descend, land

        // Case-insensitive substring; "cruise" is the only match for "CR".
        let matched = mode.string_samples_in_range(range, 100, Some("CR"));
        let labels: Vec<&str> = matched.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(labels, ["cruise"]);

        // Blank/whitespace filter disables filtering.
        let blank = mode.string_samples_in_range(range, 100, Some("   "));
        assert_eq!(blank.len(), 5);
    }

    #[test]
    fn field_view_resolves_schema_column_and_dtype() {
        let fixture = fixture(0);
        let alt = FieldView::new(&fixture.snapshot, fixture.alt).unwrap();
        let mode = FieldView::new(&fixture.snapshot, fixture.mode).unwrap();

        assert_eq!(alt.field(), fixture.alt);
        assert_eq!(alt.dtype(), &DataType::Float64);
        assert_eq!(mode.dtype(), &DataType::Utf8);
    }

    #[test]
    fn chunks_overlapping_prunes_by_effective_time_range() {
        let fixture = fixture(1_000);
        let alt = FieldView::new(&fixture.snapshot, fixture.alt).unwrap();

        let ranges = alt
            .chunks_overlapping(TimeRange::new(1_250, 1_350).unwrap())
            .map(|chunk| (chunk.t_min, chunk.t_max))
            .collect::<Vec<_>>();

        assert_eq!(ranges, vec![(300, 400)]);
    }

    #[test]
    fn sample_at_prev_next_searches_across_chunks() {
        let fixture = fixture(1_000);
        let alt = FieldView::new(&fixture.snapshot, fixture.alt).unwrap();

        assert_eq!(
            alt.sample_at(1_250, SampleMode::Prev),
            Some(Sample {
                raw_time_us: 200,
                effective_time_us: 1_200,
                value: SampleValue::Float(20.0),
            })
        );
        assert_eq!(
            alt.sample_at(1_250, SampleMode::Next),
            Some(Sample {
                raw_time_us: 300,
                effective_time_us: 1_300,
                value: SampleValue::Float(30.0),
            })
        );
    }

    #[test]
    fn sample_at_falls_back_to_linear_scan_for_overlapping_chunks() {
        // Out-of-order source (§4.3): chunk A spans [0, 200], chunk B spans
        // [100, 300] — the spine overlaps, so it is not monotonic and the
        // binary-search fast path must not engage. The predecessor of 150 lives
        // in the *later* chunk B (t=100) and the successor in the *earlier*
        // chunk A (t=200), which a naive bsearch would miss.
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "BARO").unwrap();
        let alt = identity.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "BARO",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk_a = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![0, 200]),
                vec![Arc::new(Float64Array::from(vec![0.0, 20.0])) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let chunk_b = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![100, 300]),
                vec![Arc::new(Float64Array::from(vec![10.0, 30.0])) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk_a, chunk_b]).unwrap());
        assert!(!store.is_monotonic());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();
        let alt = FieldView::new(&snapshot, alt).unwrap();

        assert_eq!(
            alt.sample_at(150, SampleMode::Prev).map(|s| s.raw_time_us),
            Some(100)
        );
        assert_eq!(
            alt.sample_at(150, SampleMode::Next).map(|s| s.raw_time_us),
            Some(200)
        );
        assert_eq!(
            alt.sample_at(300, SampleMode::Prev).map(|s| s.raw_time_us),
            Some(300)
        );
        assert_eq!(
            alt.sample_at(0, SampleMode::Next).map(|s| s.raw_time_us),
            Some(0)
        );
    }

    #[test]
    fn sample_at_linear_interpolates_numeric_values() {
        let fixture = fixture(0);
        let alt = FieldView::new(&fixture.snapshot, fixture.alt).unwrap();

        assert_eq!(
            alt.sample_at(250, SampleMode::Linear),
            Some(Sample {
                raw_time_us: 250,
                effective_time_us: 250,
                value: SampleValue::Float(25.0),
            })
        );
    }

    #[test]
    fn sample_at_returns_borrowed_string_for_prev_next_but_not_linear() {
        let fixture = fixture(0);
        let mode = FieldView::new(&fixture.snapshot, fixture.mode).unwrap();

        assert_eq!(
            mode.sample_at(150, SampleMode::Prev).unwrap().value,
            SampleValue::Utf8("climb")
        );
        assert_eq!(mode.sample_at(150, SampleMode::Linear), None);
    }

    #[test]
    fn missing_topic_store_is_reported() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "BARO").unwrap();
        let field = identity.add_field(topic, "Alt").unwrap();
        let snapshot = StoreSnapshot::from_registry(&identity, [], 0).unwrap();

        assert_eq!(
            FieldView::new(&snapshot, field).unwrap_err(),
            FieldViewError::MissingTopicStore(topic)
        );
    }
}
