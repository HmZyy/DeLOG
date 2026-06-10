//! Branching-64 min/max pyramid (PLAN.md §8.4, CCH-05..07).
//!
//! `L0[i]` holds the (min, max) of the 64 samples `64i..64(i+1)`; each higher
//! level reduces 64:1. It answers visible-window y-range queries in
//! O(64 + log₆₄ n) — raw-scanning the unaligned edge samples and reading whole
//! aligned nodes in the middle, mathematically identical to a full scan — and
//! per-pixel-column extents for the decimated draw path (§9.5).
//!
//! **NaN is a gap, not a value** (§8.2): NaN samples never contribute to a
//! min/max, and a range with no finite sample reports `NaN`.

/// Branching factor: samples per L0 node, nodes per higher-level node.
pub const BRANCH: usize = 64;

/// Min/max of a sample range; `min`/`max` are `NaN` when the range held no
/// finite sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinMax {
    pub min: f32,
    pub max: f32,
}

impl MinMax {
    /// The identity for [`merge`](Self::merge): contributes nothing.
    pub const EMPTY: Self = Self {
        min: f32::NAN,
        max: f32::NAN,
    };

    /// Whether this range saw at least one finite sample.
    pub fn is_finite(&self) -> bool {
        self.min.is_finite() || self.max.is_finite()
    }

    /// Combine two ranges, ignoring `NaN` operands.
    pub fn merge(self, other: Self) -> Self {
        Self {
            min: nan_min(self.min, other.min),
            max: nan_max(self.max, other.max),
        }
    }

    fn observe(self, y: f32) -> Self {
        if y.is_nan() {
            self
        } else {
            Self {
                min: nan_min(self.min, y),
                max: nan_max(self.max, y),
            }
        }
    }
}

fn nan_min(a: f32, b: f32) -> f32 {
    if a.is_nan() {
        b
    } else if b.is_nan() {
        a
    } else {
        a.min(b)
    }
}

fn nan_max(a: f32, b: f32) -> f32 {
    if a.is_nan() {
        b
    } else if b.is_nan() {
        a
    } else {
        a.max(b)
    }
}

fn block_minmax(block: &[f32]) -> MinMax {
    block.iter().fold(MinMax::EMPTY, |acc, &y| acc.observe(y))
}

fn reduce(nodes: &[MinMax]) -> MinMax {
    nodes.iter().fold(MinMax::EMPTY, |acc, &m| acc.merge(m))
}

/// The pyramid. Levels are stored bottom-up; `levels[0]` is L0.
#[derive(Debug, Default, Clone)]
pub struct MinMaxPyramid {
    levels: Vec<Vec<MinMax>>,
    n: usize,
}

impl MinMaxPyramid {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sample count the pyramid was built over.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn levels(&self) -> usize {
        self.levels.len()
    }

    /// Rebuild from scratch over `ys`.
    pub fn build(ys: &[f32]) -> Self {
        let mut p = Self::new();
        p.rebuild(ys);
        p
    }

    fn rebuild(&mut self, ys: &[f32]) {
        self.levels.clear();
        self.n = ys.len();
        if ys.is_empty() {
            return;
        }
        let l0: Vec<MinMax> = ys.chunks(BRANCH).map(block_minmax).collect();
        self.levels.push(l0);
        while self.levels.last().unwrap().len() > 1 {
            let next: Vec<MinMax> = self
                .levels
                .last()
                .unwrap()
                .chunks(BRANCH)
                .map(reduce)
                .collect();
            self.levels.push(next);
        }
    }

    /// Incrementally extend to cover the full buffer `ys` (old prefix + new
    /// tail) without a full rebuild (CCH-05). Only the tail block of each level
    /// is recomputed.
    pub fn extend(&mut self, ys: &[f32]) {
        if self.levels.is_empty() {
            self.rebuild(ys);
            return;
        }
        if ys.len() <= self.n {
            return;
        }

        // L0: the last (possibly partial) block plus all new blocks.
        let from = self.n / BRANCH;
        self.levels[0].truncate(from);
        for block in ys[from * BRANCH..].chunks(BRANCH) {
            self.levels[0].push(block_minmax(block));
        }
        self.n = ys.len();

        // Propagate the changed tail upward, growing the pyramid if needed.
        let mut child_from = from;
        let mut lvl = 1;
        loop {
            if lvl == self.levels.len() {
                if self.levels[lvl - 1].len() <= 1 {
                    break;
                }
                self.levels.push(Vec::new());
            }
            let parent_from = child_from / BRANCH;
            let (lower, upper) = self.levels.split_at_mut(lvl);
            let child = &lower[lvl - 1];
            let parent = &mut upper[0];
            parent.truncate(parent_from);
            for block in child[parent_from * BRANCH..].chunks(BRANCH) {
                parent.push(reduce(block));
            }
            child_from = parent_from;
            lvl += 1;
        }
    }

    /// Exact min/max over samples `[a, b)`; `ys` supplies the raw values for the
    /// unaligned edge blocks. Returns [`MinMax::EMPTY`] for an empty range.
    pub fn query(&self, ys: &[f32], a: usize, b: usize) -> MinMax {
        let a = a.min(self.n);
        let b = b.min(self.n);
        if a >= b {
            return MinMax::EMPTY;
        }

        let mut acc = MinMax::EMPTY;
        let mut i = a;
        while i < b {
            if i.is_multiple_of(BRANCH) && i + BRANCH <= b {
                // Climb to the largest aligned node that still fits in [i, b).
                let mut level = 0;
                let mut span = BRANCH;
                while level + 1 < self.levels.len() {
                    let bigger = span * BRANCH;
                    if i.is_multiple_of(bigger)
                        && i + bigger <= b
                        && i / bigger < self.levels[level + 1].len()
                    {
                        level += 1;
                        span = bigger;
                    } else {
                        break;
                    }
                }
                acc = acc.merge(self.levels[level][i / span]);
                i += span;
            } else {
                // Unaligned edge: exact raw scan.
                acc = acc.observe(ys[i]);
                i += 1;
            }
        }
        acc
    }

    /// Per-pixel-column extents over `[a, b)` split into `columns` equal index
    /// ranges (CCH-07 — drives the decimated draw path, §9.5).
    pub fn columns(&self, ys: &[f32], a: usize, b: usize, columns: usize) -> Vec<MinMax> {
        if columns == 0 || b <= a {
            return Vec::new();
        }
        let span = b - a;
        (0..columns)
            .map(|c| {
                let lo = a + span * c / columns;
                let hi = a + span * (c + 1) / columns;
                self.query(ys, lo, hi)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Naive nan-aware min/max over `ys[a..b]`.
    fn naive(ys: &[f32], a: usize, b: usize) -> MinMax {
        ys[a..b]
            .iter()
            .fold(MinMax::EMPTY, |acc, &y| acc.observe(y))
    }

    fn same(x: MinMax, y: MinMax) -> bool {
        let eq = |a: f32, b: f32| (a.is_nan() && b.is_nan()) || a == b;
        eq(x.min, y.min) && eq(x.max, y.max)
    }

    #[test]
    fn query_matches_naive_on_a_known_buffer() {
        let ys: Vec<f32> = (0..200).map(|i| i as f32).collect();
        let p = MinMaxPyramid::build(&ys);
        assert_eq!(p.len(), 200);
        let q = p.query(&ys, 10, 150);
        assert_eq!(q.min, 10.0);
        assert_eq!(q.max, 149.0);
    }

    #[test]
    fn nan_samples_are_ignored_but_an_all_nan_range_is_nan() {
        let mut ys: Vec<f32> = (0..130).map(|i| i as f32).collect();
        ys[5] = f32::NAN;
        ys[70] = f32::NAN;
        let p = MinMaxPyramid::build(&ys);
        let q = p.query(&ys, 0, 130);
        assert_eq!(q.min, 0.0);
        assert_eq!(q.max, 129.0);

        let all_nan = vec![f32::NAN; 80];
        let p2 = MinMaxPyramid::build(&all_nan);
        assert!(!p2.query(&all_nan, 0, 80).is_finite());
    }

    fn ys_strategy() -> impl Strategy<Value = Vec<f32>> {
        prop::collection::vec(
            prop_oneof![
                4 => -1.0e6_f32..1.0e6,
                1 => Just(f32::NAN),
            ],
            0..5000,
        )
    }

    proptest! {
        #[test]
        fn query_equals_naive_scan(ys in ys_strategy(), a in 0usize..5000, b in 0usize..5000) {
            let p = MinMaxPyramid::build(&ys);
            let (a, b) = (a.min(ys.len()), b.min(ys.len()));
            let (lo, hi) = (a.min(b), a.max(b));
            if lo < hi {
                prop_assert!(same(p.query(&ys, lo, hi), naive(&ys, lo, hi)));
            }
        }

        #[test]
        fn incremental_extend_equals_full_build(
            chunks in prop::collection::vec(prop::collection::vec(-1.0e3_f32..1.0e3, 0..200), 1..40)
        ) {
            // Build incrementally, appending one chunk at a time.
            let mut all = Vec::new();
            let mut inc = MinMaxPyramid::new();
            for chunk in &chunks {
                all.extend_from_slice(chunk);
                inc.extend(&all);
            }
            let full = MinMaxPyramid::build(&all);

            prop_assert_eq!(inc.len(), full.len());
            // Spot-check several ranges agree with a full build (and the naive scan).
            let n = all.len();
            for &(a, b) in &[(0, n), (0, n / 2), (n / 3, n), (n / 4, 3 * n / 4)] {
                if a < b {
                    prop_assert!(same(inc.query(&all, a, b), full.query(&all, a, b)));
                    prop_assert!(same(inc.query(&all, a, b), naive(&all, a, b)));
                }
            }
        }
    }

    #[test]
    fn columns_partition_the_range() {
        let ys: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let p = MinMaxPyramid::build(&ys);
        let cols = p.columns(&ys, 0, 1000, 10);
        assert_eq!(cols.len(), 10);
        assert_eq!(cols[0].min, 0.0);
        assert_eq!(cols[0].max, 99.0);
        assert_eq!(cols[9].min, 900.0);
        assert_eq!(cols[9].max, 999.0);
    }
}
