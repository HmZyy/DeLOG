# Visible-Window Field Statistics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add automatically refreshing exact visible-window statistics to the existing Field Stats popup, presented in Visible window and Global tabs.

**Architecture:** `delog-core` computes exact canonical statistics by folding sealed stats for fully covered chunks and scanning only partial chunks. A focused `delog-app` controller runs at most one background calculation, retains only the newest pending request, rejects stale results, and memoizes recent windows. The existing popup consumes that state and optionally uses the render pyramid for provisional min/max.

**Tech Stack:** Stable Rust 2024, Arrow 59, Rayon 1.12, std threads/channels, egui 0.34, Criterion 0.8, proptest 1.11.

---

## File Map

- Modify `Cargo.toml`: keep Rayon pinned at workspace level (already present).
- Modify `crates/delog-core/Cargo.toml`: add the workspace Rayon dependency and a visible-stats benchmark target.
- Modify `crates/delog-core/src/analysis.rs`: exact window aggregation, shared reduction, multiplier conversion, unit/property tests.
- Create `crates/delog-core/benches/visible_stats.rs`: small, fragmented, and multi-million-row query benchmarks.
- Create `crates/delog-app/src/field_stats.rs`: request keys, one-running/one-pending controller, bounded LRU, worker results, tests.
- Modify `crates/delog-app/src/main.rs`: declare the new app module.
- Modify `crates/delog-app/src/app.rs`: replace the selected-field option with the controller and render the two-tab popup.
- Modify `PLAN.md`: mark ANA-02 complete with implementation and verification evidence.

### Task 1: Exact Core Window Statistics

**Files:**
- Modify: `crates/delog-core/Cargo.toml`
- Modify: `crates/delog-core/src/analysis.rs`

- [ ] **Step 1: Add failing inclusive-window and naive-equivalence tests**

Add tests that construct two sealed chunks and assert that `visible_field_stats(&snapshot, field, t0, t1)` includes both endpoints, excludes outside rows, counts null/NaN values, applies source offsets, and matches a direct scan for min/max/mean/population-standard-deviation/count/missing/rate. Add a multiplier test covering `0.01` and `-2.0`.

- [ ] **Step 2: Run the focused tests and verify failure**

Run: `cargo test -p delog-core visible_stats -- --nocapture`

Expected: compilation fails because `visible_field_stats` is not defined.

- [ ] **Step 3: Add the core implementation**

Add `rayon = { workspace = true }` to `delog-core`. Introduce a public `FieldStats` value type (or a compatibility alias for `GlobalFieldStats`) and an internal accumulator containing min, max, sum, sum-squared, valid/missing counts, and first/last effective timestamps. Implement:

```rust
pub fn visible_field_stats(
    snapshot: &StoreSnapshot,
    field: FieldId,
    t0_us: i64,
    t1_us: i64,
) -> Result<Option<FieldStats>, FieldViewError>
```

Resolve the field entirely inside core. Normalize reversed bounds. Shift chunk bounds by the source offset safely. Skip disjoint chunks; fold `ColStats` for fully covered chunks; binary-search each partial chunk's timestamp array and scan the Arrow values without copying. Reduce chunk accumulators with Rayon. Apply the schema multiplier after canonical reduction (`min/max` swap for a negative multiplier, mean scales directly, standard deviation uses `abs(multiplier)`). Share finalization with `global_field_stats` so both tabs use the same unit semantics. Add `// upholds ZC-2` beside the partial Arrow scan.

- [ ] **Step 4: Run core tests**

Run: `cargo test -p delog-core analysis -- --nocapture`

Expected: all analysis tests pass.

### Task 2: Property Tests and Benchmark

**Files:**
- Modify: `crates/delog-core/src/analysis.rs`
- Create: `crates/delog-core/benches/visible_stats.rs`
- Modify: `crates/delog-core/Cargo.toml`

- [ ] **Step 1: Add a failing property test**

Generate numeric vectors containing finite values and NaNs plus arbitrary inclusive windows. Seal them into chunks and compare `visible_field_stats` with a naive timestamp/value scan, using tolerance checks for floating-point fields.

- [ ] **Step 2: Run the property test**

Run: `cargo test -p delog-core visible_stats_matches_naive -- --nocapture`

Expected: PASS if Task 1 is correct; otherwise preserve the smallest failing case and fix the core reducer before proceeding.

- [ ] **Step 3: Add Criterion cases**

Create `visible_stats.rs` with fixtures for: a narrow window in a normal chunk spine, a fragmented/overlapping spine, and a multi-million-row wide window. Benchmark the public helper through `criterion_group!` / `criterion_main!`. Register:

```toml
[[bench]]
name = "visible_stats"
harness = false
```

- [ ] **Step 4: Compile and run the benchmark**

Run: `cargo bench -p delog-core --bench visible_stats --no-run`

Expected: benchmark target compiles.

Run: `cargo bench -p delog-core --bench visible_stats`

Expected: all three cases complete and Criterion reports timings without panics.

### Task 3: Coalescing Background Controller

**Files:**
- Create: `crates/delog-app/src/field_stats.rs`
- Modify: `crates/delog-app/src/main.rs`

- [ ] **Step 1: Write controller tests first**

Cover these behaviors with deterministic state-level tests: Visible is the default tab; requesting A then B while A runs leaves only B pending; accepting an A result while B is current does not replace displayed B state; epoch changes produce distinct keys; an eight-entry LRU evicts the oldest result; closing clears pending work and ignores later completion.

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p delog-app field_stats --no-default-features -- --nocapture`

Expected: compilation fails because the module/controller types do not exist.

- [ ] **Step 3: Implement the controller**

Define `StatsTab::{Visible, Global}`, a hashable `StatsRequestKey { field, epoch, t0_us, t1_us }`, and `FieldStatsController`. Keep `running: Option<StatsRequestKey>`, `pending: Option<(StatsRequestKey, Arc<StoreSnapshot>)>`, a result channel, a `VecDeque` LRU capped at eight, `last_launch: Option<Instant>`, and the selected field/tab. `request` replaces `pending`; `poll` accepts only the current key and launches the newest pending request no more often than every 100 ms. Worker threads call only `delog_core::analysis::visible_field_stats`. Closing clears selection/pending while permitting an owned snapshot job to finish safely.

- [ ] **Step 4: Run controller tests**

Run: `cargo test -p delog-app field_stats --no-default-features -- --nocapture`

Expected: all controller tests pass.

### Task 4: Tabbed Popup Integration

**Files:**
- Modify: `crates/delog-app/src/app.rs`

- [ ] **Step 1: Add failing UI-state/format tests**

Add pure tests for visible-window empty formatting (`-`, zero samples), multiplier-consistent global formatting, and controller selection changes from both browser and plot actions. Keep egui paint geometry for manual verification.

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p delog-app field_stats --no-default-features -- --nocapture`

Expected: at least one new assertion fails before popup integration.

- [ ] **Step 3: Replace the existing popup state and renderer**

Replace `field_stats_dialog: Option<FieldId>` with `field_stats: FieldStatsController`. Route browser and workspace `inspect_field_stats` actions through `field_stats.open(field)`. Pass the current snapshot epoch and shared `ViewX` to the controller every frame while open. Render `Visible window` and `Global` tabs with Visible selected on each fresh open.

Visible displays bounds, status, and Min/Max/Mean/Std dev/Samples/Missing/Rate. While updating, retain and dim the last accepted values and show `Updating...`; with no result use `-`. For a ready trace cache, convert `ViewX` to cache seconds, use `index_range` and `pyramid.query` without context samples, and show provisional min/max until the canonical result arrives. Global continues to call `global_field_stats`. Both tabs preserve the non-numeric/error behavior and close if the field disappears.

- [ ] **Step 4: Run app tests**

Run: `cargo test -p delog-app --no-default-features`

Expected: all app tests pass.

- [ ] **Step 5: Manually inspect the popup**

Run: `cargo run -p delog-app --no-default-features`

Expected: Field Stats opens on Visible window; values refresh while pan/zoom remains smooth; Global switches instantly; updating values are visibly dimmed; closing/reopening selects Visible.

### Task 5: Checklist and Full Verification

**Files:**
- Modify: `PLAN.md`

- [ ] **Step 1: Mark ANA-02 complete**

Change ANA-02 to `[x]` and append a concise implementation summary covering exact chunk/stat folding, background coalescing, tabbed popup, tests, benchmark result, and any manual-GUI limitation.

- [ ] **Step 2: Run formatting and static checks**

Run: `cargo fmt --all -- --check`

Expected: exit 0.

Run: `cargo clippy --workspace --all-targets -- -D warnings`

Expected: exit 0 with no warnings.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test --workspace`

Expected: all workspace tests pass.

- [ ] **Step 4: Check the final diff and commit**

Run: `git diff --check && git status --short`

Expected: no whitespace errors; only ANA-02 implementation, tests, benchmark, plan/spec, dependency manifest, and checklist files are changed.

Commit all ANA-02 files together as required by PLAN.md section 0:

```bash
git add Cargo.toml PLAN.md crates/delog-core crates/delog-app \
  docs/superpowers/specs/2026-06-19-ana-02-visible-window-stats-design.md \
  docs/superpowers/plans/2026-06-19-ana-02-visible-window-stats.md
git commit -m "Display visible-window field statistics"
```
