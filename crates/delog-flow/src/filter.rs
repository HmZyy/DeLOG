use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    Equal,
    NotEqual,
    LessThan,
    GreaterThan,
    Between,
    OutsideRange,
}

impl FilterKind {
    pub const ALL: [Self; 6] = [
        Self::Equal,
        Self::NotEqual,
        Self::LessThan,
        Self::GreaterThan,
        Self::Between,
        Self::OutsideRange,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Equal => "Filter Equal",
            Self::NotEqual => "Filter Not Equal",
            Self::LessThan => "Filter Less Than",
            Self::GreaterThan => "Filter Greater Than",
            Self::Between => "Filter Between",
            Self::OutsideRange => "Filter Outside Range",
        }
    }

    pub fn is_range(self) -> bool {
        matches!(self, Self::Between | Self::OutsideRange)
    }

    pub fn supports_inclusive(self) -> bool {
        matches!(self, Self::LessThan | Self::GreaterThan)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterSpec {
    pub filter: FilterKind,
    pub value: f64,
    pub upper: f64,
    #[serde(default)]
    pub inclusive: bool,
}

impl FilterSpec {
    pub fn new(filter: FilterKind) -> Self {
        Self {
            filter,
            value: 0.0,
            upper: 1.0,
            inclusive: false,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self.filter {
            FilterKind::Equal => "==",
            FilterKind::NotEqual => "!=",
            FilterKind::LessThan if self.inclusive => "≤",
            FilterKind::LessThan => "<",
            FilterKind::GreaterThan if self.inclusive => "≥",
            FilterKind::GreaterThan => ">",
            FilterKind::Between => "≤ x ≤",
            FilterKind::OutsideRange => "x < min or x > max",
        }
    }

    /// Whether a finite sample is kept. Validate the active limits before evaluating a signal.
    pub fn matches(&self, value: f64) -> bool {
        value.is_finite()
            && match self.filter {
                FilterKind::Equal => value == self.value,
                FilterKind::NotEqual => value != self.value,
                FilterKind::LessThan => {
                    value < self.value || (self.inclusive && value == self.value)
                }
                FilterKind::GreaterThan => {
                    value > self.value || (self.inclusive && value == self.value)
                }
                FilterKind::Between => value >= self.value && value <= self.upper,
                FilterKind::OutsideRange => value < self.value || value > self.upper,
            }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.value.is_finite() {
            return Err(if self.filter.is_range() {
                "Filter minimum must be finite."
            } else {
                "Filter threshold must be finite."
            }
            .to_owned());
        }
        if self.filter.is_range() {
            if !self.upper.is_finite() {
                return Err("Filter maximum must be finite.".to_owned());
            }
            if self.value > self.upper {
                return Err("Filter minimum must be less than or equal to maximum.".to_owned());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicates_keep_exact_matches_and_documented_boundaries() {
        let cases = [
            (FilterKind::Equal, [false, false, true, false, false]),
            (FilterKind::NotEqual, [true, true, false, true, true]),
            (FilterKind::LessThan, [true, true, false, false, false]),
            (FilterKind::GreaterThan, [false, false, false, true, true]),
            (FilterKind::Between, [false, false, true, true, false]),
            (FilterKind::OutsideRange, [true, true, false, false, true]),
        ];
        for (filter, expected) in cases {
            let mut spec = FilterSpec::new(filter);
            for (value, expected) in [-2.0, -1.0, 0.0, 1.0, 2.0].into_iter().zip(expected) {
                assert_eq!(spec.matches(value), expected, "{filter:?}: {value}");
            }
            for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                assert!(!spec.matches(value), "{filter:?}: {value}");
            }
            spec.inclusive = true;
            assert!(
                spec.matches(0.0)
                    || matches!(filter, FilterKind::NotEqual | FilterKind::OutsideRange)
            );
            if !filter.supports_inclusive() {
                for value in [-1.0, 0.0, 1.0, 2.0] {
                    assert_eq!(spec.matches(value), FilterSpec::new(filter).matches(value));
                }
            }
        }
        let mut equal = FilterSpec::new(FilterKind::Equal);
        equal.value = -1.0;
        assert!(equal.matches(-1.0));
        assert!(!equal.matches(-1.0 + f64::EPSILON));
        equal.value = -0.0;
        assert!(equal.matches(0.0));
    }

    #[test]
    fn inclusive_comparisons_update_boundary_and_symbol() {
        for (kind, strict, inclusive) in [
            (FilterKind::LessThan, "<", "≤"),
            (FilterKind::GreaterThan, ">", "≥"),
        ] {
            let mut spec = FilterSpec::new(kind);
            spec.value = -2.0;
            assert!(!spec.matches(-2.0));
            assert_eq!(spec.symbol(), strict);
            spec.inclusive = true;
            assert!(spec.matches(-2.0));
            assert_eq!(spec.symbol(), inclusive);
            assert_eq!(spec.matches(-3.0), kind == FilterKind::LessThan);
            assert_eq!(spec.matches(-1.0), kind == FilterKind::GreaterThan);
        }
    }

    #[test]
    fn validation_only_checks_active_limits_and_allows_equal_ranges() {
        for filter in FilterKind::ALL {
            let mut spec = FilterSpec::new(filter);
            assert_eq!((spec.value, spec.upper, spec.inclusive), (0.0, 1.0, false));
            assert!(spec.validate().is_ok());
            for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                spec.value = invalid;
                assert!(spec.validate().unwrap_err().contains("finite"));
                spec.value = 0.0;
                spec.upper = invalid;
                assert_eq!(spec.validate().is_err(), filter.is_range());
            }
            spec.upper = -1.0;
            assert_eq!(spec.validate().is_err(), filter.is_range());
            spec.upper = 0.0;
            assert!(spec.validate().is_ok());
            if filter.is_range() {
                assert_eq!(spec.matches(0.0), filter == FilterKind::Between);
                assert_eq!(spec.matches(-1.0), filter == FilterKind::OutsideRange);
                assert_eq!(spec.matches(1.0), filter == FilterKind::OutsideRange);
            }
        }
    }
}
