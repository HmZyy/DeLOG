use std::collections::BTreeMap;

use delog_core::identity::SourceParam;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ParamDiffKind {
    Same,
    Changed,
    OnlyHere,
    OnlyOther,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ParamDiffRow<'a> {
    pub name: &'a str,
    pub here: Option<&'a str>,
    pub other: Option<&'a str>,
    pub kind: ParamDiffKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ParamDiffCounts {
    pub changed: usize,
    pub only_here: usize,
    pub only_other: usize,
}

pub(super) fn diff_params<'a>(
    here: &'a [SourceParam],
    other: &'a [SourceParam],
) -> Vec<ParamDiffRow<'a>> {
    let mut merged: BTreeMap<&'a str, (Option<&'a str>, Option<&'a str>)> = BTreeMap::new();
    for param in here {
        merged.entry(&param.name).or_default().0 = Some(&param.value);
    }
    for param in other {
        merged.entry(&param.name).or_default().1 = Some(&param.value);
    }
    merged
        .into_iter()
        .map(|(name, (here, other))| ParamDiffRow {
            name,
            here,
            other,
            kind: match (here, other) {
                (Some(a), Some(b)) if values_match(a, b) => ParamDiffKind::Same,
                (Some(_), Some(_)) => ParamDiffKind::Changed,
                (Some(_), None) => ParamDiffKind::OnlyHere,
                (None, _) => ParamDiffKind::OnlyOther,
            },
        })
        .collect()
}

pub(super) fn diff_counts(rows: &[ParamDiffRow<'_>]) -> ParamDiffCounts {
    let mut counts = ParamDiffCounts::default();
    for row in rows {
        match row.kind {
            ParamDiffKind::Same => {}
            ParamDiffKind::Changed => counts.changed += 1,
            ParamDiffKind::OnlyHere => counts.only_here += 1,
            ParamDiffKind::OnlyOther => counts.only_other += 1,
        }
    }
    counts
}

fn values_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(name: &str, value: &str) -> SourceParam {
        SourceParam {
            name: name.to_owned(),
            ty: "float".to_owned(),
            value: value.to_owned(),
            default: None,
        }
    }

    #[test]
    fn rows_cover_both_sides_sorted_by_name() {
        let here = [param("B", "1"), param("A", "2"), param("C", "3")];
        let other = [param("D", "4"), param("A", "5"), param("C", "3")];

        let rows = diff_params(&here, &other);

        assert_eq!(
            rows,
            vec![
                ParamDiffRow {
                    name: "A",
                    here: Some("2"),
                    other: Some("5"),
                    kind: ParamDiffKind::Changed,
                },
                ParamDiffRow {
                    name: "B",
                    here: Some("1"),
                    other: None,
                    kind: ParamDiffKind::OnlyHere,
                },
                ParamDiffRow {
                    name: "C",
                    here: Some("3"),
                    other: Some("3"),
                    kind: ParamDiffKind::Same,
                },
                ParamDiffRow {
                    name: "D",
                    here: None,
                    other: Some("4"),
                    kind: ParamDiffKind::OnlyOther,
                },
            ]
        );
    }

    #[test]
    fn numeric_values_compare_by_value_not_spelling() {
        let here = [param("A", "1"), param("B", "0.50"), param("C", "1e3")];
        let other = [param("A", "1.0"), param("B", ".5"), param("C", "1000.0001")];

        let kinds: Vec<_> = diff_params(&here, &other)
            .iter()
            .map(|row| row.kind)
            .collect();

        assert_eq!(
            kinds,
            vec![
                ParamDiffKind::Same,
                ParamDiffKind::Same,
                ParamDiffKind::Changed
            ]
        );
    }

    #[test]
    fn text_values_compare_exactly() {
        let here = [param("A", "Auto"), param("B", "x")];
        let other = [param("A", "auto"), param("B", "x")];

        let kinds: Vec<_> = diff_params(&here, &other)
            .iter()
            .map(|row| row.kind)
            .collect();

        assert_eq!(kinds, vec![ParamDiffKind::Changed, ParamDiffKind::Same]);
    }

    #[test]
    fn duplicate_names_use_the_last_value() {
        let here = [param("A", "1"), param("A", "2")];
        let other = [param("A", "2")];

        let rows = diff_params(&here, &other);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].here, Some("2"));
        assert_eq!(rows[0].kind, ParamDiffKind::Same);
    }

    #[test]
    fn counts_tally_each_difference_kind() {
        let here = [param("A", "1"), param("B", "2"), param("C", "3")];
        let other = [
            param("A", "9"),
            param("C", "3"),
            param("D", "4"),
            param("E", "5"),
        ];

        let counts = diff_counts(&diff_params(&here, &other));

        assert_eq!(
            counts,
            ParamDiffCounts {
                changed: 1,
                only_here: 1,
                only_other: 2,
            }
        );
    }
}
