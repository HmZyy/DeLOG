//! Data-quality scan.
//!
//! Post-load, per-topic sweep over canonical timestamps + seal-time column
//! stats: cross-chunk timestamp regressions, dt outliers (>10× the median
//! dt), duplicate timestamps, and NaN/Inf percentages. Each finding category
//! is summarized as a *single* diagnostic with counts — a noisy log produces
//! four rows, not four thousand. Pure and synchronous; the app runs it on a
//! worker thread ("async, post-load").

use crate::diagnostics::Diag;
use crate::identity::SourceId;
use crate::snapshot::StoreSnapshot;
use crate::store::TopicStore;

/// A dt this many times above the median is an outlier.
const DT_OUTLIER_FACTOR: i64 = 10;

/// Cap on dts collected for the median estimate; longer topics are sampled
/// evenly. Keeps the scan linear with a small constant.
const MEDIAN_SAMPLE_CAP: usize = 65_536;

/// Scan every topic of `source`, returning one summarized diagnostic per
/// finding category per topic. Empty when the data is clean.
pub fn scan_source(snapshot: &StoreSnapshot, source: SourceId) -> Vec<Diag> {
    let mut diags = Vec::new();
    let Some(src) = snapshot.source(source) else {
        return diags;
    };
    for &topic_id in src.topics.iter() {
        let Some(topic) = snapshot.topic(topic_id) else {
            continue;
        };
        if topic.entry.removed {
            continue;
        }
        let Some(store) = topic.store.as_deref() else {
            continue;
        };
        scan_topic(store, &topic.entry.name, source, &mut diags);
    }
    diags
}

fn scan_topic(store: &TopicStore, topic: &str, source: SourceId, diags: &mut Vec<Diag>) {
    let mut regressions = 0u64;
    let mut first_regression_us = None;
    let mut duplicates = 0u64;
    let mut first_duplicate_us = None;

    // Pass 1 over raw timestamps: regressions, duplicates and the sampled
    // dts for the median estimate.
    let rows = store.rows as usize;
    let stride = rows.div_ceil(MEDIAN_SAMPLE_CAP).max(1);
    let mut dts: Vec<i64> = Vec::with_capacity(rows.div_ceil(stride));
    let mut index = 0usize;
    let mut prev: Option<i64> = None;
    for chunk in store.chunks.iter() {
        for i in 0..chunk.t.len() {
            let t = chunk.t.value(i);
            if let Some(prev) = prev {
                let dt = t - prev;
                if dt < 0 {
                    regressions += 1;
                    first_regression_us.get_or_insert(t);
                } else if dt == 0 {
                    duplicates += 1;
                    first_duplicate_us.get_or_insert(t);
                } else if index.is_multiple_of(stride) {
                    dts.push(dt);
                }
                index += 1;
            }
            prev = Some(t);
        }
    }

    // Pass 2: count dts above 10× the median (positive dts only — regressions
    // and duplicates are already their own findings).
    let median = median_of(&mut dts);
    let mut outliers = 0u64;
    let mut first_outlier_us = None;
    let mut worst_dt_us = 0i64;
    if let Some(median) = median {
        let threshold = median.saturating_mul(DT_OUTLIER_FACTOR);
        let mut prev: Option<i64> = None;
        for chunk in store.chunks.iter() {
            for i in 0..chunk.t.len() {
                let t = chunk.t.value(i);
                if let Some(prev) = prev {
                    let dt = t - prev;
                    if dt > threshold {
                        outliers += 1;
                        first_outlier_us.get_or_insert(t);
                        worst_dt_us = worst_dt_us.max(dt);
                    }
                }
                prev = Some(t);
            }
        }
    }

    // NaN/Inf from seal-time column stats — no sample re-scan.
    let mut nan_cells = 0u64;
    let mut inf_fields: Vec<&str> = Vec::new();
    let mut numeric_cells = 0u64;
    for (col, field) in store.schema.fields().iter().enumerate() {
        let mut field_nans = 0u64;
        let mut field_inf = false;
        for chunk in store.chunks.iter() {
            if let Some(stats) = chunk.stats.get(col) {
                field_nans += stats.nan_count;
                field_inf |= stats.min.is_infinite() || stats.max.is_infinite();
            }
        }
        nan_cells += field_nans;
        numeric_cells += store.rows;
        if field_inf {
            inf_fields.push(&field.name);
        }
    }

    if regressions > 0 {
        let mut d = Diag::warning(
            "quality-regression",
            format!("{topic}: {regressions} timestamp regression(s)"),
        )
        .with_source(source);
        if let Some(t) = first_regression_us {
            d = d.at_time(t);
        }
        diags.push(d);
    }
    if duplicates > 0 {
        let mut d = Diag::info(
            "quality-duplicate-ts",
            format!("{topic}: {duplicates} duplicate timestamp(s)"),
        )
        .with_source(source);
        if let Some(t) = first_duplicate_us {
            d = d.at_time(t);
        }
        diags.push(d);
    }
    if outliers > 0 {
        let median = median.unwrap_or(0);
        let mut d = Diag::info(
            "quality-dt-outlier",
            format!(
                "{topic}: {outliers} dt outlier(s) >10× median ({} µs); worst {worst_dt_us} µs",
                median
            ),
        )
        .with_source(source);
        if let Some(t) = first_outlier_us {
            d = d.at_time(t);
        }
        diags.push(d);
    }
    if nan_cells > 0 || !inf_fields.is_empty() {
        let pct = if numeric_cells > 0 {
            nan_cells as f64 * 100.0 / numeric_cells as f64
        } else {
            0.0
        };
        let inf = if inf_fields.is_empty() {
            String::new()
        } else {
            format!("; ±Inf in {}", inf_fields.join(", "))
        };
        diags.push(
            Diag::info(
                "quality-nan",
                format!("{topic}: {nan_cells} NaN cell(s) ({pct:.2}%){inf}"),
            )
            .with_source(source),
        );
    }
}

/// Median of the sampled dts (sorts in place; `None` when empty).
fn median_of(dts: &mut [i64]) -> Option<i64> {
    if dts.is_empty() {
        return None;
    }
    let mid = dts.len() / 2;
    let (_, median, _) = dts.select_nth_unstable(mid);
    Some(*median)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;

    use super::*;
    use crate::chunk::Chunk;
    use crate::identity::IdentityRegistry;
    use crate::schema::{FieldSchema, TopicSchema};

    fn snapshot_with(times: &[&[i64]], values: &[&[f64]]) -> (StoreSnapshot, SourceId) {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "GPS").unwrap();
        identity.add_field(topic, "Alt").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [FieldSchema::new("Alt", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunks: Vec<_> = times
            .iter()
            .zip(values)
            .map(|(t, v)| {
                let cols: Vec<ArrayRef> = vec![Arc::new(Float64Array::from(v.to_vec()))];
                Arc::new(Chunk::try_new(Int64Array::from(t.to_vec()), cols, &schema).unwrap())
            })
            .collect();
        let store = Arc::new(crate::store::TopicStore::from_chunks(schema, chunks).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();
        (snapshot, source)
    }

    #[test]
    fn clean_data_yields_no_findings() {
        let (snapshot, source) = snapshot_with(&[&[100, 200, 300, 400]], &[&[1.0, 2.0, 3.0, 4.0]]);
        assert!(scan_source(&snapshot, source).is_empty());
    }

    #[test]
    fn cross_chunk_regression_is_one_summarized_warning() {
        // Second chunk starts before the first ends (within-chunk order is
        // enforced at seal time, so regressions only appear across chunks).
        let (snapshot, source) = snapshot_with(
            &[&[100, 200, 300], &[150, 250]],
            &[&[1.0, 2.0, 3.0], &[4.0, 5.0]],
        );
        let diags = scan_source(&snapshot, source);
        let reg = diags
            .iter()
            .find(|d| d.code == "quality-regression")
            .expect("regression finding");
        assert!(reg.message.contains("1 timestamp regression"));
        assert_eq!(reg.time_us, Some(150));
        assert_eq!(reg.source, Some(source));
    }

    #[test]
    fn duplicates_and_dt_outliers_are_counted() {
        // dt median 100 µs; one 10 s gap; one duplicate pair.
        let mut times: Vec<i64> = (0..100).map(|i| i * 100).collect();
        times.push(9_900); // duplicate of the last sample
        times.push(10_000_000); // 10 s gap → outlier
        times.push(10_000_100);
        let values: Vec<f64> = times.iter().map(|_| 0.0).collect();
        let (snapshot, source) = snapshot_with(&[&times], &[&values]);

        let diags = scan_source(&snapshot, source);
        let dup = diags
            .iter()
            .find(|d| d.code == "quality-duplicate-ts")
            .expect("duplicate finding");
        assert!(dup.message.contains("1 duplicate"));
        assert_eq!(dup.time_us, Some(9_900));

        let outlier = diags
            .iter()
            .find(|d| d.code == "quality-dt-outlier")
            .expect("outlier finding");
        assert!(
            outlier.message.contains("1 dt outlier"),
            "{}",
            outlier.message
        );
        assert_eq!(outlier.time_us, Some(10_000_000));
    }

    #[test]
    fn nan_and_inf_percentages_come_from_chunk_stats() {
        let (snapshot, source) = snapshot_with(
            &[&[100, 200, 300, 400]],
            &[&[1.0, f64::NAN, f64::INFINITY, 4.0]],
        );
        let diags = scan_source(&snapshot, source);
        let nan = diags
            .iter()
            .find(|d| d.code == "quality-nan")
            .expect("nan finding");
        assert!(nan.message.contains("1 NaN cell"), "{}", nan.message);
        assert!(nan.message.contains("25.00%"), "{}", nan.message);
        assert!(nan.message.contains("±Inf in Alt"), "{}", nan.message);
    }

    #[test]
    fn unknown_source_scans_to_nothing() {
        let (snapshot, _) = snapshot_with(&[&[100, 200]], &[&[1.0, 2.0]]);
        assert!(scan_source(&snapshot, SourceId(42)).is_empty());
    }
}
