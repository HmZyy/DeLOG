use crate::{Error, Result};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TimestampMode {
    #[default]
    Effective,
    Original,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignmentMode {
    Prev,
    Nearest,
    Linear,
}

impl AlignmentMode {
    pub fn parse(mode: &str) -> Result<Self> {
        match mode {
            "prev" => Ok(Self::Prev),
            "nearest" => Ok(Self::Nearest),
            "linear" => Ok(Self::Linear),
            _ => Err(Error::invalid_input(
                "align mode must be 'prev', 'nearest', or 'linear'",
            )),
        }
    }
}

pub fn align_values(src_t: &[i64], src_v: &[f64], base: &[i64], mode: AlignmentMode) -> Vec<f64> {
    let mode = match mode {
        AlignmentMode::Prev => delog_core::align::AlignMode::Prev,
        AlignmentMode::Nearest => delog_core::align::AlignMode::Nearest,
        AlignmentMode::Linear => delog_core::align::AlignMode::Linear,
    };
    delog_core::align::align_values(src_t, src_v, base, mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_align(
        src_t: &[i64],
        src_v: &[f64],
        base: &[i64],
        mode: AlignmentMode,
    ) -> Vec<f64> {
        let mut canonical: Vec<(i64, f64)> = Vec::new();
        for (&time, &value) in src_t.iter().zip(src_v) {
            if canonical
                .last()
                .is_some_and(|(last_time, _)| *last_time == time)
            {
                canonical.last_mut().unwrap().1 = value;
            } else {
                canonical.push((time, value));
            }
        }

        base.iter()
            .map(|&base_time| match mode {
                AlignmentMode::Prev => canonical
                    .iter()
                    .rev()
                    .find(|(time, _)| *time <= base_time)
                    .map_or(f64::NAN, |(_, value)| *value),
                AlignmentMode::Nearest => canonical
                    .iter()
                    .enumerate()
                    .min_by_key(|(index, (time, _))| {
                        ((i128::from(*time) - i128::from(base_time)).abs(), *index)
                    })
                    .map_or(f64::NAN, |(_, (_, value))| *value),
                AlignmentMode::Linear => {
                    if let Some((_, value)) = canonical.iter().find(|(time, _)| *time == base_time)
                    {
                        *value
                    } else {
                        let lower = canonical.iter().rev().find(|(time, _)| *time < base_time);
                        let upper = canonical.iter().find(|(time, _)| *time > base_time);
                        match (lower, upper) {
                            (Some((start_time, start_value)), Some((end_time, end_value))) => {
                                let fraction = (i128::from(base_time) - i128::from(*start_time))
                                    as f64
                                    / (i128::from(*end_time) - i128::from(*start_time)) as f64;
                                start_value + fraction * (end_value - start_value)
                            }
                            _ => f64::NAN,
                        }
                    }
                }
            })
            .collect()
    }

    fn prop_assert_float_vectors_eq(
        actual: &[f64],
        expected: &[f64],
    ) -> std::result::Result<(), proptest::test_runner::TestCaseError> {
        proptest::prop_assert_eq!(actual.len(), expected.len());
        for (&actual, &expected) in actual.iter().zip(expected) {
            if expected.is_nan() {
                proptest::prop_assert!(actual.is_nan());
            } else {
                proptest::prop_assert_eq!(actual, expected);
            }
        }
        Ok(())
    }

    proptest::proptest! {
        #[test]
        fn align_modes_match_linear_scan_reference(
            src in proptest::collection::vec((-50i64..50, -1e6f64..1e6), 0..50),
            base in proptest::collection::vec(proptest::num::i64::ANY, 0..50),
        ) {
            let mut src = src;
            src.sort_by_key(|(time, _)| *time);
            let src_t: Vec<i64> = src.iter().map(|(time, _)| *time).collect();
            let src_v: Vec<f64> = src.iter().map(|(_, value)| *value).collect();

            for mode in [AlignmentMode::Prev, AlignmentMode::Nearest, AlignmentMode::Linear] {
                let actual = align_values(&src_t, &src_v, &base, mode);
                let expected = reference_align(&src_t, &src_v, &base, mode);
                prop_assert_float_vectors_eq(&actual, &expected)?;
            }
        }
    }
}
