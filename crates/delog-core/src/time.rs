//! Canonical microsecond time model.
//!
//! The core layer stores timestamps as integer microseconds. Source offsets are
//! also integer microseconds; floating point belongs only in render caches.

/// Canonical timestamp or offset in microseconds.
pub type TimestampUs = i64;

/// Inclusive timestamp range in canonical microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub min_us: TimestampUs,
    pub max_us: TimestampUs,
}

impl TimeRange {
    /// Build a range when `min_us <= max_us`.
    pub const fn new(min_us: TimestampUs, max_us: TimestampUs) -> Option<Self> {
        if min_us <= max_us {
            Some(Self { min_us, max_us })
        } else {
            None
        }
    }

    /// Build a one-sample range.
    pub const fn point(us: TimestampUs) -> Self {
        Self {
            min_us: us,
            max_us: us,
        }
    }

    /// Apply a source offset, returning `None` on `i64` overflow.
    pub fn offset(self, offset_us: TimestampUs) -> Option<Self> {
        Some(Self {
            min_us: effective_time_us(self.min_us, offset_us)?,
            max_us: effective_time_us(self.max_us, offset_us)?,
        })
    }

    /// Return the smallest range containing both inputs.
    pub const fn union(self, other: Self) -> Self {
        Self {
            min_us: if self.min_us < other.min_us {
                self.min_us
            } else {
                other.min_us
            },
            max_us: if self.max_us > other.max_us {
                self.max_us
            } else {
                other.max_us
            },
        }
    }

    /// Return the smallest range containing this range and `us`.
    pub const fn include(self, us: TimestampUs) -> Self {
        Self {
            min_us: if self.min_us < us { self.min_us } else { us },
            max_us: if self.max_us > us { self.max_us } else { us },
        }
    }

    pub const fn contains(self, us: TimestampUs) -> bool {
        self.min_us <= us && us <= self.max_us
    }
}

/// Effective source time = raw source timestamp + user/source offset.
pub fn effective_time_us(raw_us: TimestampUs, offset_us: TimestampUs) -> Option<TimestampUs> {
    raw_us.checked_add(offset_us)
}

/// Inverse of [`effective_time_us`].
pub fn raw_time_us(effective_us: TimestampUs, offset_us: TimestampUs) -> Option<TimestampUs> {
    effective_us.checked_sub(offset_us)
}

/// Union all ranges into one global range. Empty input has no range.
pub fn global_range(ranges: impl IntoIterator<Item = TimeRange>) -> Option<TimeRange> {
    ranges.into_iter().reduce(TimeRange::union)
}

/// Apply each source offset and union the resulting effective ranges.
///
/// Returns `None` for empty input or if any offset operation overflows.
pub fn global_effective_range(
    ranges: impl IntoIterator<Item = (TimeRange, TimestampUs)>,
) -> Option<TimeRange> {
    let mut out: Option<TimeRange> = None;
    for (raw_range, offset_us) in ranges {
        let effective = raw_range.offset(offset_us)?;
        out = Some(match out {
            Some(current) => current.union(effective),
            None => effective,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_time_is_raw_plus_offset_in_microseconds() {
        assert_eq!(effective_time_us(1_000_000, -250_000), Some(750_000));
        assert_eq!(raw_time_us(750_000, -250_000), Some(1_000_000));
    }

    #[test]
    fn effective_time_uses_checked_arithmetic() {
        assert_eq!(effective_time_us(i64::MAX, 1), None);
        assert_eq!(raw_time_us(i64::MIN, 1), None);
    }

    #[test]
    fn range_rejects_inverted_bounds_and_supports_queries() {
        assert_eq!(TimeRange::new(10, 5), None);
        let range = TimeRange::new(10, 20).unwrap();
        assert!(range.contains(10));
        assert!(range.contains(15));
        assert!(range.contains(20));
        assert!(!range.contains(21));
    }

    #[test]
    fn range_offset_applies_to_both_bounds() {
        let range = TimeRange::new(100, 200).unwrap();
        assert_eq!(range.offset(-50), TimeRange::new(50, 150));
        assert_eq!(
            TimeRange::new(i64::MAX - 1, i64::MAX).unwrap().offset(1),
            None
        );
    }

    #[test]
    fn global_range_unions_non_empty_ranges() {
        let ranges = [
            TimeRange::new(100, 200).unwrap(),
            TimeRange::new(-50, 50).unwrap(),
            TimeRange::point(500),
        ];

        assert_eq!(global_range(ranges), TimeRange::new(-50, 500));
        assert_eq!(global_range([]), None);
    }

    #[test]
    fn global_effective_range_applies_offsets_before_union() {
        let ranges = [
            (TimeRange::new(100, 200).unwrap(), -100),
            (TimeRange::new(50, 75).unwrap(), 1_000),
        ];

        assert_eq!(global_effective_range(ranges), TimeRange::new(0, 1_075));
        assert_eq!(global_effective_range([]), None);
    }
}
