//! Timeline alignment shared by the scripting API and the data-flow evaluator.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignMode {
    Prev,
    Nearest,
    Linear,
}

impl AlignMode {
    pub fn parse(mode: &str) -> Result<Self, String> {
        match mode {
            "prev" => Ok(Self::Prev),
            "nearest" => Ok(Self::Nearest),
            "linear" => Ok(Self::Linear),
            _ => Err("align mode must be 'prev', 'nearest', or 'linear'".to_owned()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prev => "prev",
            Self::Nearest => "nearest",
            Self::Linear => "linear",
        }
    }
}

pub fn align_values(src_t: &[i64], src_v: &[f64], base: &[i64], mode: AlignMode) -> Vec<f64> {
    let mut times = Vec::with_capacity(src_t.len());
    let mut values = Vec::with_capacity(src_v.len());
    for (&time, &value) in src_t.iter().zip(src_v) {
        if times.last() == Some(&time) {
            *values.last_mut().unwrap() = value;
        } else {
            times.push(time);
            values.push(value);
        }
    }

    base.iter()
        .map(|&bt| match times.binary_search(&bt) {
            Ok(index) => values[index],
            Err(index) => match mode {
                AlignMode::Prev => index
                    .checked_sub(1)
                    .map_or(f64::NAN, |previous| values[previous]),
                AlignMode::Nearest => match (index.checked_sub(1), times.get(index)) {
                    (None, None) => f64::NAN,
                    (None, Some(_)) => values[index],
                    (Some(previous), None) => values[previous],
                    (Some(previous), Some(&next_time)) => {
                        let previous_distance =
                            (i128::from(bt) - i128::from(times[previous])).abs();
                        let next_distance = (i128::from(next_time) - i128::from(bt)).abs();
                        if previous_distance <= next_distance {
                            values[previous]
                        } else {
                            values[index]
                        }
                    }
                },
                AlignMode::Linear => match (index.checked_sub(1), times.get(index)) {
                    (Some(previous), Some(&next_time)) => {
                        let span = i128::from(next_time) - i128::from(times[previous]);
                        let offset = i128::from(bt) - i128::from(times[previous]);
                        let fraction = offset as f64 / span as f64;
                        values[previous] + fraction * (values[index] - values[previous])
                    }
                    _ => f64::NAN,
                },
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_prev_uses_last_duplicate_and_preserves_unsorted_base() {
        let got = align_values(
            &[10, 20, 20, 30],
            &[1.0, 2.0, 22.0, 3.0],
            &[25, 5, 20, 40],
            AlignMode::Prev,
        );
        assert_eq!(got[0], 22.0);
        assert!(got[1].is_nan());
        assert_eq!(&got[2..], &[22.0, 3.0]);
    }

    #[test]
    fn align_nearest_prefers_earlier_time_on_ties() {
        let got = align_values(
            &[10, 20, 20, 30],
            &[1.0, 2.0, 22.0, 3.0],
            &[0, 15, 20, 25, 40],
            AlignMode::Nearest,
        );
        assert_eq!(got, vec![1.0, 1.0, 22.0, 22.0, 3.0]);
    }

    #[test]
    fn align_linear_interpolates_only_between_distinct_times() {
        let got = align_values(
            &[10, 20, 20, 30],
            &[1.0, 2.0, 4.0, 8.0],
            &[5, 10, 15, 20, 25, 30, 35],
            AlignMode::Linear,
        );
        assert!(got[0].is_nan());
        assert_eq!(&got[1..6], &[1.0, 2.5, 4.0, 6.0, 8.0]);
        assert!(got[6].is_nan());
    }

    #[test]
    fn align_modes_propagate_nan_values() {
        let src_t = [0, 10, 20];
        let src_v = [1.0, f64::NAN, 3.0];
        assert!(align_values(&src_t, &src_v, &[15], AlignMode::Prev)[0].is_nan());
        assert!(align_values(&src_t, &src_v, &[10], AlignMode::Nearest)[0].is_nan());
        assert!(align_values(&src_t, &src_v, &[15], AlignMode::Linear)[0].is_nan());
    }

    #[test]
    fn parse_accepts_known_modes_and_rejects_others() {
        assert_eq!(AlignMode::parse("prev"), Ok(AlignMode::Prev));
        assert_eq!(AlignMode::parse("nearest"), Ok(AlignMode::Nearest));
        assert_eq!(AlignMode::parse("linear"), Ok(AlignMode::Linear));
        assert!(AlignMode::parse("cubic").is_err());
    }
}
