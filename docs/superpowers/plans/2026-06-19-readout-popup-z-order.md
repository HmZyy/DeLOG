# Readout Popup Z-Order Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Paint hover and playhead value readouts above plot content but beneath every floating window.

**Architecture:** Define the shared readout area's egui layer policy once in `hover.rs` as `Order::Background`, then use it in `show_tooltip`. A source-policy integration test pins both the constant value and its use, providing regression coverage without exposing test-only production APIs.

**Tech Stack:** Rust 2024, egui 0.34, Cargo tests

---

### Task 1: Add failing layer-policy coverage

**Files:**
- Create: `crates/delog-app/tests/readout_layer_policy.rs`

- [x] **Step 1: Write the failing regression test**

```rust
const HOVER_SOURCE: &str = include_str!("../src/hover.rs");

#[test]
fn value_readout_uses_the_background_layer() {
    assert!(HOVER_SOURCE.contains(
        "const READOUT_ORDER: egui::Order = egui::Order::Background;"
    ));
    assert!(HOVER_SOURCE.contains(".order(READOUT_ORDER)"));
}
```

- [x] **Step 2: Run the test and confirm the RED state**

Run: `cargo test -p delog-app --test readout_layer_policy`

Expected: FAIL because `hover.rs` still uses `.order(egui::Order::Tooltip)` and has no `READOUT_ORDER` constant.

### Task 2: Move the shared readout beneath windows

**Files:**
- Modify: `crates/delog-app/src/hover.rs`

- [x] **Step 1: Define and use the Background order**

Add near the imports:

```rust
const READOUT_ORDER: egui::Order = egui::Order::Background;
```

Change the shared value readout builder from:

```rust
.order(egui::Order::Tooltip)
```

to:

```rust
.order(READOUT_ORDER)
```

This single builder serves both hover and playhead readouts. Do not change cursor lines, sample circles, position, pivot, contents, or visibility rules.

- [x] **Step 2: Run the focused test and confirm the GREEN state**

Run: `cargo test -p delog-app --test readout_layer_policy`

Expected: one test passes.

- [x] **Step 3: Format the workspace**

Run: `cargo fmt --all`

Expected: formatting succeeds without unrelated changes.

### Task 3: Verify and complete PLT-16

**Files:**
- Modify: `PLAN.md`

- [x] **Step 1: Run project verification**

Run: `cargo test -p delog-app`

Expected: all application tests pass.

Run: `cargo check --workspace`

Expected: workspace compilation succeeds.

Run: `cargo clippy --workspace -- -D warnings`

Expected: clippy succeeds without warnings.

- [x] **Step 2: Mark PLT-16 complete**

Change the checklist entry to:

```markdown
- [x] **PLT-16** — Hover and playhead value readouts remain visible over plots but always paint beneath floating windows; the shared readout area uses `Order::Background`, pinned by `readout_layer_policy`
```

- [x] **Step 3: Commit the implementation**

```bash
git add PLAN.md crates/delog-app/src/hover.rs crates/delog-app/tests/readout_layer_policy.rs docs/superpowers/plans/2026-06-19-readout-popup-z-order.md
git commit -m "Keep plot readouts beneath popup windows"
```
