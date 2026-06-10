//! Render-cache benches (PLAN.md §20.4, CCH-11). Expanded as the cache lands.

use criterion::{Criterion, criterion_group, criterion_main};
use delog_cache::MinMaxPyramid;

fn bench_pyramid(c: &mut Criterion) {
    let ys: Vec<f32> = (0..1_000_000).map(|i| (i as f32 * 0.001).sin()).collect();
    let pyramid = MinMaxPyramid::build(&ys);

    c.bench_function("pyramid_build_1M", |b| {
        b.iter(|| MinMaxPyramid::build(&ys));
    });

    // A mid-span y-range query over 1M samples (the auto-visible-Y hot path).
    c.bench_function("pyramid_yquery_1M", |b| {
        b.iter(|| pyramid.query(&ys, 100_000, 900_000));
    });
}

criterion_group!(benches, bench_pyramid);
criterion_main!(benches);
